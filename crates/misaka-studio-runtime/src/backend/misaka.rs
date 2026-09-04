//! The MISAKA runtime backend — the integer engine, driven like any other.
//!
//! This is the runtime the misakas repository carries for PALW (`misaka-palw-base0`), served over
//! an OpenAI-compatible socket by `misaka-palw-serve` and supervised here through the same
//! [`ChildEngine`] that drives llama.cpp and MLX. The Studio therefore has an engine that needs no
//! llama.cpp at all.
//!
//! # Why this one is different from the engines beside it
//!
//! `openai_child`'s note says a binary we did not build cannot tell us its build flags, so its
//! determinism class is scoped to (backend, OS, arch, accelerator) and the rest is `unknown`. This
//! engine is the exception the note anticipates. Its arithmetic is integer and its weights are a
//! `.palwart` whose digest a chain registers: two machines running the same artifact produce the
//! same tokens, and the class is the artifact rather than the host. That is also what makes a run
//! here eligible to become work — the same execution a court can recompute.
//!
//! It also means the model file is not interchangeable with the other backends'. A GGUF is not a
//! `.palwart`, and pointing this engine at one fails at load rather than half-working, which is
//! the honest direction: an engine that quietly accepted the wrong file would record a class the
//! execution does not belong to.
//!
//! # Not yet adjudicable, and it says so out loud
//!
//! The A16 family produces logit rows and generated tokens; it does not capture the activation,
//! checkpoint and step legs that make an execution disputable, so `Qwen25A16Backend` leaves
//! `supports_court` false and `misaka-palw-serve` reports `court_capable: false`. Chat through
//! this backend is real inference under a registered class; it is not yet a free-prompt claim
//! anyone can mine. The Network tab is where that distinction is shown, and it must keep being
//! shown: this module drives inference and makes no mining claim of its own.

use super::openai_child::{ChildEngine, ChildEngineConfig};
use super::{Availability, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, StreamEvent};
use crate::Result;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use std::path::PathBuf;
use std::time::Duration;

/// The class tag this runtime registers under. Unchanged from when this module was a placeholder:
/// the tag was chosen for the engine that has now arrived, and moving it would orphan the records
/// written under it.
pub const MISAKA_CLASS_TAG: &str = "misaka-palw-base0/deterministic-integer/v1";

pub struct MisakaBackend {
    engine: ChildEngine,
}

impl MisakaBackend {
    /// The name this backend answers to, everywhere. The load gate in `state.rs` compares against
    /// it to decide whether a PALW artifact has an engine that can read it, and a gate comparing
    /// against a second copy of the string is a gate that opens the day one of them is renamed.
    pub const NAME: &'static str = "misaka";

    /// `serve` is `backend.misaka_serve_path` and `tokenizer` is `backend.misaka_tokenizer_path`;
    /// both `None` fall back to the resolutions documented on the settings fields.
    pub fn new(serve: Option<PathBuf>, tokenizer: Option<PathBuf>, startup_timeout: Duration) -> Self {
        let program = resolve_program(serve);
        MisakaBackend {
            engine: ChildEngine::new(ChildEngineConfig {
                name: "misaka",
                program,
                args: Box::new(move |request, port| build_args(request, port, tokenizer.as_deref())),
                health_path: "/health",
                startup_timeout,
                env: Vec::new(),
            }),
        }
    }

    pub fn recent_log(&self) -> Vec<String> {
        self.engine.recent_log()
    }
}

/// Where the server binary is: configured, then beside the Studio (how a packaged app ships one),
/// then PATH. A bare name is the last answer so a failure names what is missing rather than an
/// absolute path that never existed.
pub fn resolve_program(configured: Option<PathBuf>) -> PathBuf {
    let exe_name = if cfg!(windows) { "misaka-palw-serve.exe" } else { "misaka-palw-serve" };
    if let Some(path) = configured {
        return path;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join(exe_name), dir.join("engines").join(exe_name)] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(found) = which(exe_name) {
        return found;
    }
    PathBuf::from(exe_name)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|c| c.is_file())
}

/// **The tokenizer, which the artifact deliberately does not carry.**
///
/// A `.palwart` commits to what the ids MEAN (`tokenizer_commitment`) without shipping the file,
/// because consensus never runs a tokenizer — so the file is the operator's to supply. Configured
/// first; otherwise `tokenizer.json` beside the artifact, which is where a downloaded class puts
/// it; otherwise the bare name, so the server's error says which file it wanted.
fn resolve_tokenizer(configured: Option<&std::path::Path>, model_path: &std::path::Path) -> PathBuf {
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

fn build_args(request: &LoadRequest, port: u16, tokenizer: Option<&std::path::Path>) -> Vec<String> {
    vec![
        "--artifact".into(),
        request.model_path.display().to_string(),
        "--tokenizer".into(),
        resolve_tokenizer(tokenizer, &request.model_path).display().to_string(),
        "--listen".into(),
        format!("127.0.0.1:{port}"),
    ]
    // Deliberately nothing else. `gpu_layers`, `flash_attention`, `mmap` and `mlock` are
    // llama.cpp's knobs for a llama.cpp graph; this engine's execution is fixed by the artifact,
    // and a flag that silently changed arithmetic would change the class.
}

impl InferenceBackend for MisakaBackend {
    fn name(&self) -> &'static str {
        MisakaBackend::NAME
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async {
            RuntimeDescriptor {
                backend: "misaka".into(),
                // The engine is built from this workspace's own pinned crate rather than fetched
                // as somebody's binary, so the honest values here are the ones the build knows.
                engine_commit: env!("CARGO_PKG_VERSION").into(),
                engine_patch_sha256: "none".into(),
                engine_build_number: 0,
                build_profile: "release".into(),
                class_tag: MISAKA_CLASS_TAG.into(),
            }
        })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            self.engine
                .availability(
                    "Build it with `cargo build --release -p misaka-palw-base0 --bin misaka-palw-serve` in the misakas \
                     repository, or set backend.misaka_serve_path in Settings to a build you already have. This backend \
                     will not fall back to another engine: a record naming `misaka` must come from the MISAKA runtime.",
                )
                .await
        })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move { self.engine.load(request).await })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { self.engine.unload().await })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>> {
        Box::pin(async { self.engine.loaded().await })
    }

    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<StreamEvent>>>> {
        Box::pin(async move { self.engine.generate(request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> MisakaBackend {
        MisakaBackend::new(Some(PathBuf::from("/nonexistent/misaka-palw-serve")), None, Duration::from_secs(1))
    }

    /// **The substitution this backend has always existed to prevent.** It used to refuse because
    /// it had no engine; now it has one, and the rule is unchanged — a missing server is an error
    /// that names the server, never a quiet hand-off to llama.cpp.
    #[tokio::test]
    async fn a_missing_server_is_named_and_never_substituted() {
        match backend().availability().await {
            Availability::Unavailable { reason, remedy } => {
                assert!(reason.contains("misaka-palw-serve"), "the reason names the binary: {reason}");
                assert!(remedy.contains("will not fall back"), "the remedy states the rule: {remedy}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    /// Its determinism class must not collide with any other engine's.
    #[tokio::test]
    async fn its_class_is_its_own() {
        let descriptor = backend().descriptor().await;
        assert_eq!(descriptor.class_tag, MISAKA_CLASS_TAG);
        assert_ne!(descriptor.class_tag, super::super::mock::MOCK_CLASS_TAG);
        assert!(!descriptor.class_tag.contains("llamacpp"));
    }

    /// The command line carries the artifact, a tokenizer and a port — and none of llama.cpp's
    /// arithmetic knobs, because this engine's execution is the artifact's.
    #[test]
    fn the_command_line_is_the_artifact_a_tokenizer_and_a_port() {
        let request = LoadRequest {
            model_id: "qwen25-a16".into(),
            model_path: "/models/qwen25-1.5b-a16.palwart".into(),
            context_size: 4096,
            gpu_layers: Some(99),
            threads: Some(8),
            flash_attention: misaka_studio_core::settings::FlashAttention::On,
            use_mmap: true,
            use_mlock: true,
            needs_default_chat_template: false,
            extra_args: Vec::new(),
        };
        let args = build_args(&request, 1339, Some(std::path::Path::new("/models/tokenizer.json")));
        assert_eq!(
            args,
            vec![
                "--artifact",
                "/models/qwen25-1.5b-a16.palwart",
                "--tokenizer",
                "/models/tokenizer.json",
                "--listen",
                "127.0.0.1:1339",
            ]
        );
        for forbidden in ["--n-gpu-layers", "--threads", "--flash-attn", "--no-mmap", "--mlock"] {
            assert!(!args.iter().any(|a| a == forbidden), "{forbidden} would change arithmetic this class fixes");
        }
    }

    /// A tokenizer beside the artifact is the downloaded-class layout; the configured path wins
    /// over it; and with neither, the bare name makes the server's error name the missing file.
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
