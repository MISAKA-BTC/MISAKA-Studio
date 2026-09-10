//! The MISAKA runtime backend — the class's own family worker, behind an answer-only gateway.
//!
//! **What runs (ADR-0096 Decision 10).** `misaka-palw-gateway --answer-never-commit` over the
//! family worker (`palw-a16-fp-worker` for a `.palwart`, `palw-qwen36-fp-worker` for a
//! `.palwq36`), supervised here through the same [`ChildEngine`] that drives llama.cpp and MLX,
//! and spoken to through [`GatewayBackend`] — the one implementation of the lane's request shape
//! (no sampling knobs, the `misaka` object, the trim to the class's 512 tokens, the summary and
//! continue legs). The server this module used to drive, `misaka-palw-serve`, was deleted from the
//! node tree on 2026-09-02 (misakas `2f688bc1`, ADR-0077 Decision 1: the server IS the worker), so
//! the Studio's chat now runs the exact binary a producer runs — the same tokens a court would
//! recompute, from a process that files nothing.
//!
//! **Answer-only, and what that removes.** The gateway is given the identity `{}` and a synthetic
//! anchor ([`LOCAL_ANSWER_ONLY_ANCHOR_HEX`]): no bond, no key, no class id (the gateway adopts its
//! worker's, the one value the worker would refuse any other of), and `--answer-never-commit`, so
//! `/health` says `can_submit: false` and every answer says `committed: false`. Nothing produced
//! here can reach a chain, which is why nothing about a chain is needed to chat.
//!
//! **The tokenizer must be bound — measured.** The family worker refuses, at boot, an artifact
//! whose `tokenizer_commitment` is all zeros, and the published dense file
//! (`qwen25-1.5b-a16.palwart`, sha `a8c4e53e…`) is one (measured 2026-09-10). Binding is one
//! command in the node tree — `palw-class bind-tokenizer` — and its output, from the public file
//! and the public `tokenizer.json`, is byte-identical to the file the testnet-11 fleet runs
//! (`3f8fc506…`). When the worker refuses for that reason and the retired `misaka-palw-serve` is
//! still on this machine, this backend falls back to it and SAYS so — in the log, the descriptor
//! and the fingerprint — rather than taking away a chat that worked yesterday; with no fallback,
//! the refusal names the command that fixes it.
//!
//! # Not adjudicable here, and it says so
//!
//! Chat through this backend is real inference under a registered class. It is not a claim: the
//! gateway never commits. The Network tab's mining paths are where a claim is made.

use super::gateway::GatewayBackend;
use super::openai_child::{ChildEngine, ChildEngineConfig};
use super::{Availability, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, RuntimeFingerprint, StreamEvent};
use crate::components::{ComponentId, Resolution, resolve_beside, resolve_component};
use crate::{Error, Result};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::RwLock;

/// The class tag this runtime registers under. Unchanged from when this module was a placeholder:
/// the tag was chosen for the engine that has now arrived, and moving it would orphan the records
/// written under it.
pub const MISAKA_CLASS_TAG: &str = "misaka-palw-base0/deterministic-integer/v1";

/// **The anchor an answer-only job carries: a value no chain holds.**
///
/// `blake2b-512("misaka-studio/local-answer-only-anchor/v1")`. A free-prompt job binds an anchor
/// block into its id (ADR-0077 Decision 3), and the gateway refuses the all-zero one as "no
/// anchor available", so an answer-only gateway needs SOME anchor; a real block's hash would be a
/// chain point a commitment could ride, and none may. A hash of a label is neither zero nor a
/// block, and anyone can recompute it.
pub const LOCAL_ANSWER_ONLY_ANCHOR_HEX: &str =
    "4a14dd2b2fe0c508fdca4187f0bb9e7e57336a5ccc9491b371ecd125b77674652173bb02502b02ba200e96690c07f31424e4dd68f54eecabf35056a129fc4c33";

/// The worker's refusal when the artifact declares no tokenizer — the one refusal this backend
/// treats as "bind it, or fall back", matched on the worker's own words.
const UNBOUND_TOKENIZER_REFUSAL: &str = "declares no tokenizer";

/// The decode cap handed to the gateway: the widest shipped row. The gateway also checks
/// `prompt + decode ≤ n_ctx`, so this is a ceiling, never a promise.
const LOCAL_MAX_DECODE_CAP: u32 = 512;

static WORKDIR: OnceLock<PathBuf> = OnceLock::new();

/// Where the local gateway keeps its identity, anchor and outbox — set ONCE by the runtime from
/// its data directory (which `--data-dir` may override); `default_data_dir()` otherwise.
pub fn set_local_gateway_workdir(dir: PathBuf) {
    let _ = WORKDIR.set(dir);
}

fn local_gateway_workdir() -> PathBuf {
    WORKDIR.get().cloned().unwrap_or_else(|| misaka_studio_core::settings::default_data_dir().join("local-gateway"))
}

/// Which family worker a class artifact needs, read off its extension — the container each
/// family writes, and the only thing the Studio knows about a file before a worker opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// `.palwart` — the dense A16 tier (`palw-a16-fp-worker`).
    A16,
    /// `.palwq36` — the hybrid tier (`palw-qwen36-fp-worker`).
    Qwen36,
}

impl Family {
    pub fn of(path: &Path) -> Option<Family> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("palwart") => Some(Family::A16),
            Some("palwq36") => Some(Family::Qwen36),
            _ => None,
        }
    }

    pub fn worker(self) -> ComponentId {
        match self {
            Family::A16 => ComponentId::PalwA16FpWorker,
            Family::Qwen36 => ComponentId::PalwQwen36FpWorker,
        }
    }
}

/// Which engine answered the last load.
enum Active {
    /// The gateway over the family worker (ADR-0096 Decision 10), and the load as it happened:
    /// the spawn's time (the lane's own `load` is only a health round trip) with the class's
    /// window from the gateway's `/health`.
    Gateway(Arc<GatewayBackend>, LoadedModel),
    /// The retired `misaka-palw-serve`, for an unbound artifact on a machine that still has it.
    Legacy,
}

pub struct MisakaBackend {
    /// Supervises `misaka-palw-gateway`.
    gateway_engine: ChildEngine,
    /// Supervises the retired `misaka-palw-serve` — used only as the named fallback.
    legacy_engine: ChildEngine,
    active: RwLock<Option<Active>>,
    gateway: Resolution,
    a16_worker: Resolution,
    qwen36_worker: Resolution,
    legacy: Resolution,
    /// `backend.misaka_tokenizer_path`, kept so the fingerprint can name it: a closure cannot be
    /// asked what it captured.
    tokenizer: Option<PathBuf>,
    network: &'static str,
    workdir: PathBuf,
}

impl MisakaBackend {
    /// The name this backend answers to, everywhere. The load gate in `state.rs` compares against
    /// it to decide whether a PALW artifact has an engine that can read it, and a gate comparing
    /// against a second copy of the string is a gate that opens the day one of them is renamed.
    pub const NAME: &'static str = "misaka";

    /// `gateway` is `backend.misaka_gateway_path` (the family workers are looked for beside it
    /// first, then by the one search order); `serve` is the retired `backend.misaka_serve_path`,
    /// kept only for the named fallback; `tokenizer` is `backend.misaka_tokenizer_path`; `network`
    /// is `node.network`'s id, which the worker requires (`MISAKA_PALW_NETWORK_ID`).
    pub fn new(
        gateway: Option<PathBuf>,
        serve: Option<PathBuf>,
        tokenizer: Option<PathBuf>,
        network: &'static str,
        startup_timeout: Duration,
    ) -> Self {
        let gateway_resolution = resolve_component(&ComponentId::MisakaPalwGateway, gateway.as_deref(), None);
        let a16_worker = resolve_beside(&Family::A16.worker(), &gateway_resolution);
        let qwen36_worker = resolve_beside(&Family::Qwen36.worker(), &gateway_resolution);
        let legacy = resolve_component(&ComponentId::MisakaPalwServe, serve.as_deref(), None);
        let workdir = local_gateway_workdir();

        let (args_workdir, args_a16, args_qwen36) = (workdir.clone(), a16_worker.path.clone(), qwen36_worker.path.clone());
        let env_tokenizer = tokenizer.clone();
        let legacy_tokenizer = tokenizer.clone();
        MisakaBackend {
            gateway_engine: ChildEngine::new(ChildEngineConfig {
                name: Self::NAME,
                program: gateway_resolution.path.clone(),
                program_candidate: gateway_resolution.candidate,
                args: Box::new(move |request, port| {
                    let worker = match Family::of(&request.model_path) {
                        Some(Family::Qwen36) => &args_qwen36,
                        _ => &args_a16,
                    };
                    gateway_args(&args_workdir, worker, port)
                }),
                health_path: "/health",
                startup_timeout,
                // The worker's stderr is swallowed by default (ADR-0079 SA-7); a local chat engine
                // has no stranger's prompt in it, and a boot refusal that nobody can read is the
                // failure this backend has to name.
                env: vec![("MISAKA_PALW_GATEWAY_LOG_WORKER_STDERR".into(), "1".into())],
                load_env: Some(Box::new(move |request| worker_env(network, &request.model_path, env_tokenizer.as_deref()))),
            }),
            legacy_engine: ChildEngine::new(ChildEngineConfig {
                name: Self::NAME,
                program: legacy.path.clone(),
                program_candidate: legacy.candidate,
                args: Box::new(move |request, port| legacy_args(request, port, legacy_tokenizer.as_deref())),
                health_path: "/health",
                startup_timeout,
                env: Vec::new(),
                load_env: None,
            }),
            active: RwLock::new(None),
            gateway: gateway_resolution,
            a16_worker,
            qwen36_worker,
            legacy,
            tokenizer,
            network,
            workdir,
        }
    }

    pub fn recent_log(&self) -> Vec<String> {
        let mut log = self.gateway_engine.recent_log();
        log.extend(self.legacy_engine.recent_log());
        log
    }

    fn worker_for(&self, family: Family) -> &Resolution {
        match family {
            Family::A16 => &self.a16_worker,
            Family::Qwen36 => &self.qwen36_worker,
        }
    }

    /// Whether this machine can run the gateway path for at least one family.
    pub fn gateway_installed(&self) -> bool {
        self.gateway.found && (self.a16_worker.found || self.qwen36_worker.found)
    }

    /// Whether this machine has any local integer engine at all — the gateway path, or the
    /// retired server it falls back to.
    pub fn any_installed(&self) -> bool {
        self.gateway_installed() || self.legacy.found
    }

    async fn load_legacy(&self, request: LoadRequest, why: &str) -> Result<LoadedModel> {
        tracing::warn!(
            "{why}: answering with the RETIRED misaka-palw-serve at {} (deleted from the node tree 2026-09-02, misakas \
             2f688bc1). Bind the artifact's tokenizer with `palw-class bind-tokenizer` and install misaka-palw-gateway plus \
             the family worker to move off it (ADR-0096 Decision 10).",
            self.legacy.path.display()
        );
        let loaded = self.legacy_engine.load(request).await?;
        *self.active.write().await = Some(Active::Legacy);
        Ok(loaded)
    }
}

/// **The gateway's command line** — the exact flag set an operator can run by hand (the
/// `node.rs::build_args` rule: built as data, shown verbatim, runnable without the Studio).
pub fn gateway_args(workdir: &Path, worker: &Path, port: u16) -> Vec<String> {
    vec![
        "--listen".into(),
        format!("127.0.0.1:{port}"),
        "--worker".into(),
        worker.display().to_string(),
        "--outbox".into(),
        workdir.join("outbox").display().to_string(),
        "--identity".into(),
        workdir.join("identity.json").display().to_string(),
        "--anchor".into(),
        workdir.join("anchor.json").display().to_string(),
        "--answer-never-commit".into(),
        "--max-decode-cap".into(),
        LOCAL_MAX_DECODE_CAP.to_string(),
    ]
}

/// **The worker's three variables, in the order it reads them** (network, artifact, tokenizer),
/// all absolute. The gateway passes exactly these through to the worker it spawns (the ADR-0079
/// allowlist), so they are set on the gateway.
pub fn worker_env(network: &str, artifact: &Path, tokenizer: Option<&Path>) -> Vec<(String, String)> {
    vec![
        ("MISAKA_PALW_NETWORK_ID".into(), network.to_string()),
        ("MISAKA_PALW_ARTIFACT".into(), absolute(artifact).display().to_string()),
        ("MISAKA_PALW_TOKENIZER".into(), absolute(&resolve_tokenizer(tokenizer, artifact)).display().to_string()),
    ]
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
}

/// Write the answer-only identity and the synthetic anchor, and empty the outbox.
///
/// The outbox holds answer-only summaries and retained traces for runs nothing references — the
/// gateway never commits here — so it is emptied at every load rather than left to grow by a
/// trace per chat. Only a directory named `outbox` inside a workdir named `local-gateway` is ever
/// removed, so a misconfigured path cannot turn this into a delete of something else.
pub fn prepare_workdir(workdir: &Path) -> Result<()> {
    let io = |what: &Path, e: std::io::Error| Error::io(what.display(), e);
    std::fs::create_dir_all(workdir).map_err(|e| io(workdir, e))?;
    let outbox = workdir.join("outbox");
    if workdir.file_name().and_then(|n| n.to_str()) == Some("local-gateway") && outbox.is_dir() {
        std::fs::remove_dir_all(&outbox).map_err(|e| io(&outbox, e))?;
    }
    std::fs::create_dir_all(&outbox).map_err(|e| io(&outbox, e))?;
    // `{}`: no bond, no key, no class — the gateway adopts its worker's class (misakas `70d56519`).
    std::fs::write(workdir.join("identity.json"), b"{}\n").map_err(|e| io(workdir, e))?;
    let anchor = serde_json::json!({ "anchor_block": LOCAL_ANSWER_ONLY_ANCHOR_HEX, "anchor_daa": 0 });
    std::fs::write(workdir.join("anchor.json"), format!("{anchor}\n")).map_err(|e| io(workdir, e))?;
    Ok(())
}

/// Whether a load failure is the worker refusing an artifact that declares no tokenizer.
pub fn is_unbound_tokenizer_refusal(message: &str) -> bool {
    message.contains(UNBOUND_TOKENIZER_REFUSAL)
}

/// The command that fixes an unbound artifact, for the class this worker serves.
pub fn bind_command(artifact: &Path, tokenizer: &Path, network: &str) -> String {
    let out = artifact.with_extension("bound.palwart");
    format!(
        "palw-class bind-tokenizer --network {network} --tokenizer {} --out {} --model-id 'Qwen/Qwen2.5-1.5B/graph-v5@512' {}",
        tokenizer.display(),
        out.display(),
        artifact.display()
    )
}

/// **The tokenizer, which the artifact deliberately does not carry.**
///
/// A `.palwart` commits to what the ids MEAN (`tokenizer_commitment`) without shipping the file,
/// because consensus never runs a tokenizer — so the file is the operator's to supply. Configured
/// first; otherwise `tokenizer.json` beside the artifact, which is where a downloaded class puts
/// it; otherwise the bare name, so the engine's error says which file it wanted.
fn resolve_tokenizer(configured: Option<&Path>, model_path: &Path) -> PathBuf {
    if let Some(path) = configured {
        return path.to_path_buf();
    }
    if let Some(dir) = model_path.parent() {
        let beside = dir.join("tokenizer.json");
        if beside.is_file() {
            return beside;
        }
    }
    PathBuf::from("tokenizer.json")
}

/// The retired server's command line, unchanged — it is only ever the fallback.
fn legacy_args(request: &LoadRequest, port: u16, tokenizer: Option<&Path>) -> Vec<String> {
    vec![
        "--artifact".into(),
        request.model_path.display().to_string(),
        "--tokenizer".into(),
        resolve_tokenizer(tokenizer, &request.model_path).display().to_string(),
        "--listen".into(),
        format!("127.0.0.1:{port}"),
    ]
}

impl InferenceBackend for MisakaBackend {
    fn name(&self) -> &'static str {
        MisakaBackend::NAME
    }

    fn fingerprint(&self) -> RuntimeFingerprint {
        let mut fingerprint = self.gateway_engine.fingerprint();
        fingerprint.tokenizer = self.tokenizer.clone();
        fingerprint.extra.insert("network".into(), self.network.to_string());
        fingerprint.extra.insert("workdir".into(), self.workdir.display().to_string());
        fingerprint.extra.insert("worker:a16".into(), self.a16_worker.path.display().to_string());
        fingerprint.extra.insert("worker:qwen36".into(), self.qwen36_worker.path.display().to_string());
        fingerprint.extra.insert("legacy:misaka-palw-serve".into(), self.legacy.path.display().to_string());
        fingerprint
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async {
            let legacy = matches!(*self.active.read().await, Some(Active::Legacy));
            RuntimeDescriptor {
                backend: "misaka".into(),
                // A binary built in another repository: its commit is not something this process
                // can prove, so it is the literal `unknown` (the `h_R` rule in `backend/mod.rs`).
                engine_commit: "unknown".into(),
                engine_patch_sha256: "none".into(),
                engine_build_number: 0,
                build_profile: if legacy {
                    "misaka-palw-serve (retired 2026-09-02, fallback for an unbound artifact)".into()
                } else {
                    "misaka-palw-gateway --answer-never-commit + family worker".into()
                },
                class_tag: MISAKA_CLASS_TAG.into(),
            }
        })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            if self.gateway_installed() {
                return Availability::Available {
                    detail: format!(
                        "misaka-palw-gateway at {} ({}); a16 worker {}; qwen36 worker {}",
                        self.gateway.path.display(),
                        self.gateway.candidate,
                        if self.a16_worker.found { "found" } else { "missing" },
                        if self.qwen36_worker.found { "found" } else { "missing" }
                    ),
                };
            }
            if self.legacy.found {
                return Availability::Available {
                    detail: format!(
                        "only the RETIRED misaka-palw-serve at {} — install misaka-palw-gateway and the family worker \
                         (Components) to run the binary a producer runs",
                        self.legacy.path.display()
                    ),
                };
            }
            Availability::Unavailable {
                reason: format!(
                    "misaka-palw-gateway {} and no family worker is installed",
                    if self.gateway.found { "is installed," } else { "is not installed" }
                ),
                remedy: "Install misaka-palw-gateway and palw-a16-fp-worker from the Components page, or build them in the \
                         misakas repository (`cargo build --release -p misaka-palw-gateway -p misaka-palw-base0 --bin \
                         misaka-palw-gateway --bin palw-a16-fp-worker`) and set backend.misaka_gateway_path. This backend will \
                         not fall back to another engine: a record naming `misaka` must come from the MISAKA runtime."
                    .into(),
            }
        })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move {
            let Some(family) = Family::of(&request.model_path) else {
                return Err(Error::bad_request(format!(
                    "{} is not a class artifact (.palwart or .palwq36); the MISAKA runtime reads nothing else",
                    request.model_path.display()
                )));
            };
            self.unload().await?;
            let worker = self.worker_for(family);
            if !(self.gateway.found && worker.found) {
                if self.legacy.found && family == Family::A16 {
                    return self.load_legacy(request, "misaka-palw-gateway or palw-a16-fp-worker is not installed").await;
                }
                return Err(Error::BackendUnavailable {
                    backend: Self::NAME.into(),
                    reason: format!(
                        "{} {} not installed",
                        if self.gateway.found { family.worker().file_name() } else { "misaka-palw-gateway".to_string() },
                        "is"
                    ),
                    remedy: "Install it from the Components page, or set backend.misaka_gateway_path to a directory that \
                             holds the gateway and the family workers."
                        .into(),
                });
            }
            prepare_workdir(&self.workdir)?;
            match self.gateway_engine.load(request.clone()).await {
                Ok(spawned) => {
                    let lane = Arc::new(GatewayBackend::new(self.gateway_engine.base_url().await?, None));
                    // The gateway's /health is the window: n_ctx is the class's, and the lane's
                    // trim is sized from it.
                    let mut loaded = lane.load(request).await?;
                    loaded.load_ms = spawned.load_ms;
                    *self.active.write().await = Some(Active::Gateway(lane, loaded.clone()));
                    Ok(loaded)
                }
                Err(e) if is_unbound_tokenizer_refusal(&e.to_string()) => {
                    if self.legacy.found && family == Family::A16 {
                        return self.load_legacy(request, "the family worker refused this artifact: it declares no tokenizer").await;
                    }
                    let tokenizer = resolve_tokenizer(self.tokenizer.as_deref(), &request.model_path);
                    Err(Error::BackendUnavailable {
                        backend: Self::NAME.into(),
                        reason: format!(
                            "{} declares no tokenizer, and the family worker refuses such an artifact at boot (a zero \
                             tokenizer_commitment pins nothing a replay could check)",
                            request.model_path.display()
                        ),
                        remedy: format!(
                            "Bind it once (about 40 s; the output keeps the class's registered root and is byte-identical to \
                             the file the testnet-11 fleet runs), then load the .bound.palwart: {}",
                            bind_command(&request.model_path, &tokenizer, self.network)
                        ),
                    })
                }
                Err(e) => Err(e),
            }
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            *self.active.write().await = None;
            self.gateway_engine.unload().await?;
            self.legacy_engine.unload().await
        })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>> {
        Box::pin(async {
            match &*self.active.read().await {
                Some(Active::Gateway(_, loaded)) => Some(loaded.clone()),
                Some(Active::Legacy) => self.legacy_engine.loaded().await,
                None => None,
            }
        })
    }

    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<StreamEvent>>>> {
        Box::pin(async move {
            let lane = match &*self.active.read().await {
                Some(Active::Gateway(lane, _)) => lane.clone(),
                Some(Active::Legacy) => return self.legacy_engine.generate(request).await,
                None => return Err(Error::NoModelLoaded),
            };
            lane.generate(request).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing() -> MisakaBackend {
        MisakaBackend::new(
            Some(PathBuf::from("/nonexistent/misaka-palw-gateway")),
            Some(PathBuf::from("/nonexistent/misaka-palw-serve")),
            None,
            "testnet-11",
            Duration::from_secs(1),
        )
    }

    fn request(path: &str) -> LoadRequest {
        LoadRequest {
            model_id: "qwen25-a16".into(),
            model_path: path.into(),
            context_size: 4096,
            gpu_layers: Some(99),
            threads: Some(8),
            flash_attention: misaka_studio_core::settings::FlashAttention::On,
            use_mmap: true,
            use_mlock: true,
            needs_default_chat_template: false,
            extra_args: Vec::new(),
        }
    }

    /// **The substitution this backend has always existed to prevent.** A missing engine is an
    /// error that names what is missing, never a quiet hand-off to llama.cpp.
    #[tokio::test]
    async fn a_missing_engine_is_named_and_never_substituted() {
        match missing().availability().await {
            Availability::Unavailable { reason, remedy } => {
                assert!(reason.contains("misaka-palw-gateway"), "the reason names the binary: {reason}");
                assert!(remedy.contains("will not fall back"), "the remedy states the rule: {remedy}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    /// Its determinism class must not collide with any other engine's.
    #[tokio::test]
    async fn its_class_is_its_own() {
        let descriptor = missing().descriptor().await;
        assert_eq!(descriptor.class_tag, MISAKA_CLASS_TAG);
        assert_ne!(descriptor.class_tag, super::super::mock::MOCK_CLASS_TAG);
        assert!(!descriptor.class_tag.contains("llamacpp"));
        assert_eq!(descriptor.engine_commit, "unknown", "a binary built elsewhere cannot prove its commit");
    }

    /// **The command an operator can run by hand**, flag for flag: answer-only, the offline
    /// anchor form, the identity and outbox in the workdir, the worker by absolute path — and none
    /// of llama.cpp's arithmetic knobs, because this engine's execution is the artifact's.
    #[test]
    fn the_gateway_command_line_is_exactly_the_answer_only_flag_set() {
        let args = gateway_args(Path::new("/data/local-gateway"), Path::new("/engines/palw-a16-fp-worker"), 1339);
        assert_eq!(
            args,
            vec![
                "--listen",
                "127.0.0.1:1339",
                "--worker",
                "/engines/palw-a16-fp-worker",
                "--outbox",
                "/data/local-gateway/outbox",
                "--identity",
                "/data/local-gateway/identity.json",
                "--anchor",
                "/data/local-gateway/anchor.json",
                "--answer-never-commit",
                "--max-decode-cap",
                "512",
            ]
        );
        for forbidden in ["--rpc", "--derive-seed", "--n-gpu-layers", "--threads", "--temperature"] {
            assert!(!args.iter().any(|a| a == forbidden), "{forbidden} does not belong on an answer-only local gateway");
        }
    }

    /// The worker reads network, artifact and tokenizer, in that order, absolute — the gateway
    /// passes exactly these through its allowlist.
    #[test]
    fn the_worker_environment_is_the_three_variables_absolute() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("qwen25-1.5b-a16.bound.palwart");
        let tokenizer = dir.path().join("tokenizer.json");
        let env = worker_env("testnet-11", &artifact, Some(&tokenizer));
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["MISAKA_PALW_NETWORK_ID", "MISAKA_PALW_ARTIFACT", "MISAKA_PALW_TOKENIZER"]);
        assert_eq!(env[0].1, "testnet-11");
        assert!(Path::new(&env[1].1).is_absolute() && Path::new(&env[2].1).is_absolute());
        let relative = worker_env("testnet-11", Path::new("models/x.palwart"), None);
        assert!(Path::new(&relative[1].1).is_absolute(), "a relative artifact path is made absolute: the gateway's cwd differs");
    }

    /// The workdir holds `{}` and the synthetic anchor, and only the outbox inside a directory
    /// named `local-gateway` is ever emptied.
    #[test]
    fn the_workdir_is_an_empty_identity_and_an_anchor_no_chain_holds() {
        let root = tempfile::tempdir().expect("tempdir");
        let workdir = root.path().join("local-gateway");
        std::fs::create_dir_all(workdir.join("outbox/traces")).unwrap();
        std::fs::write(workdir.join("outbox/traces/old.bin"), b"stale").unwrap();
        prepare_workdir(&workdir).expect("prepares");
        let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(workdir.join("identity.json")).unwrap()).unwrap();
        assert_eq!(identity, serde_json::json!({}), "no bond, no key, no class: the gateway adopts its worker's");
        let anchor: serde_json::Value = serde_json::from_slice(&std::fs::read(workdir.join("anchor.json")).unwrap()).unwrap();
        assert_eq!(anchor["anchor_block"], LOCAL_ANSWER_ONLY_ANCHOR_HEX);
        assert_eq!(anchor["anchor_daa"], 0);
        assert_eq!(LOCAL_ANSWER_ONLY_ANCHOR_HEX.len(), 128);
        assert!(LOCAL_ANSWER_ONLY_ANCHOR_HEX.chars().any(|c| c != '0'), "the gateway refuses a zero anchor");
        assert!(!workdir.join("outbox/traces/old.bin").exists(), "the answer-only outbox is emptied at load");
        // A directory with another name keeps its outbox: the delete is scoped by name.
        let other = root.path().join("somewhere-else");
        std::fs::create_dir_all(other.join("outbox")).unwrap();
        std::fs::write(other.join("outbox/keep.bin"), b"keep").unwrap();
        prepare_workdir(&other).unwrap();
        assert!(other.join("outbox/keep.bin").exists(), "only a local-gateway workdir's outbox is removed");
    }

    /// The worker's own words select the remedy, and the remedy is a runnable command.
    #[test]
    fn an_unbound_artifact_is_recognised_and_the_bind_command_is_named() {
        let refusal = "engine exited with exit status: 1 before it was ready:\n[palw-a16-fp-worker] fatal: \
                       Qwen/Qwen2.5-1.5B/graph-v5@512: this artifact declares no tokenizer: `tokenizer_commitment` is all zeros";
        assert!(is_unbound_tokenizer_refusal(refusal));
        assert!(!is_unbound_tokenizer_refusal("connection refused"));
        let command = bind_command(Path::new("/m/qwen25-1.5b-a16.palwart"), Path::new("/t/tokenizer.json"), "testnet-11");
        assert!(command.starts_with("palw-class bind-tokenizer --network testnet-11 --tokenizer /t/tokenizer.json"));
        assert!(command.contains("--out /m/qwen25-1.5b-a16.bound.palwart"), "{command}");
        assert!(command.ends_with("/m/qwen25-1.5b-a16.palwart"));
    }

    /// Every path the backend was built from is in the fingerprint, so a settings change to any of
    /// them replaces the engine (ADR-0096 Decision 11).
    #[test]
    fn the_fingerprint_names_the_gateway_the_workers_the_tokenizer_and_the_fallback() {
        let a = missing().fingerprint();
        let b = MisakaBackend::new(
            Some(PathBuf::from("/nonexistent/misaka-palw-gateway")),
            Some(PathBuf::from("/nonexistent/misaka-palw-serve")),
            Some(PathBuf::from("/models/tokenizer.json")),
            "testnet-11",
            Duration::from_secs(1),
        )
        .fingerprint();
        assert_eq!(a.kind, "misaka");
        assert_eq!(a.program.as_deref(), Some(Path::new("/nonexistent/misaka-palw-gateway")));
        for key in ["worker:a16", "worker:qwen36", "legacy:misaka-palw-serve", "network", "workdir"] {
            assert!(a.extra.contains_key(key), "{key} is a constructor input and must be in the fingerprint");
        }
        assert_ne!(a, b, "the tokenizer path is a constructor input");
    }

    /// A GGUF is not a class artifact: refused by name, never handed to a worker.
    #[tokio::test]
    async fn a_file_that_is_not_a_class_artifact_is_refused_by_name() {
        let error = missing().load(request("/models/model.gguf")).await.expect_err("a GGUF is not an artifact");
        assert!(error.to_string().contains(".palwart"), "{error}");
        assert_eq!(Family::of(Path::new("x.palwart")), Some(Family::A16));
        assert_eq!(Family::of(Path::new("x.palwq36")), Some(Family::Qwen36));
    }

    /// The tokenizer: configured, then beside the artifact, then named.
    #[test]
    fn the_tokenizer_is_configured_then_beside_the_artifact_then_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("class.palwart");
        let beside = dir.path().join("tokenizer.json");
        assert_eq!(resolve_tokenizer(None, &artifact), PathBuf::from("tokenizer.json"));
        std::fs::write(&beside, b"{}").expect("write");
        assert_eq!(resolve_tokenizer(None, &artifact), beside);
        let configured = PathBuf::from("/elsewhere/tokenizer.json");
        assert_eq!(resolve_tokenizer(Some(&configured), &artifact), configured);
    }
}
