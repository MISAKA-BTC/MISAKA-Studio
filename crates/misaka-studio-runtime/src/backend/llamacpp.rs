//! The llama.cpp backend — `llama-server`, supervised.
//!
//! This is the default engine on every platform the Studio targets: CUDA or Vulkan on Windows and
//! Linux, Metal on Apple Silicon, plain CPU everywhere else. It is also the one the MISAKA
//! network's PALW work already uses, which matters for the long path — the artifacts a validator
//! pins are llama.cpp GGUFs under a pinned llama.cpp build.
//!
//! # Finding the binary
//!
//! Three places, in order, because each is right for a different kind of user:
//!
//! 1. **The configured path** — someone who built llama.cpp with flags they care about, or the
//!    build the Studio installed for them (`crate::engines` writes this setting).
//! 2. **Next to the Studio executable** — the packaged desktop app ships an engine beside itself.
//! 3. **`PATH`** — a developer with `llama-server` installed system-wide.
//!
//! When none of them has it, the backend reports unavailable *with the remedy*, and the app
//! keeps running on the mock backend rather than failing to start. A local-LLM app that refuses
//! to open because an engine is missing has made the user's first problem unsolvable from inside
//! the app.
//!
//! # What the engine can drive
//!
//! `--n-gpu-layers` is a request. Whether anything honours it depends on the **build** — a
//! `llama-server` from a package repository is usually CPU-only and accepts the flag in silence.
//! So this backend asks the binary first (`--list-devices`, see [`super::devices`]) and, for an
//! engine new enough to answer, loads at `-lv 4` so the engine's own buffer report says where the
//! weights went. Both answers travel with the loaded model, evidence attached.

use super::devices::{EngineDevice, accelerator_tag_for, offload_from_devices, offload_from_log};
use super::openai_child::{ChildEngine, ChildEngineConfig};
use super::{Availability, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, StreamEvent};
use crate::Result;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use misaka_studio_core::settings::FlashAttention;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct LlamaCppBackend {
    engine: ChildEngine,
    /// The tag the hardware probe suggests. Used only when the engine cannot say for itself.
    probed_accelerator_tag: String,
}

impl LlamaCppBackend {
    /// `configured` is `backend.llama_server_path`; `accelerator_tag` is `cuda`, `metal`, `rocm`
    /// or `cpu` from the hardware probe. The engine's own device list overrides it when known,
    /// because the same source built for a different accelerator is different arithmetic — and a
    /// CPU-only build on a CUDA machine is the CPU's arithmetic, whatever the card.
    pub fn new(configured: Option<PathBuf>, accelerator_tag: impl Into<String>, startup_timeout: Duration) -> Self {
        let program = resolve_program(configured);
        LlamaCppBackend {
            probed_accelerator_tag: accelerator_tag.into(),
            engine: ChildEngine::new(ChildEngineConfig {
                name: "llamacpp",
                program: program.clone(),
                args: Box::new(build_args),
                // llama-server answers /health with 503 while the model loads and 200 once it is
                // ready, which is exactly the signal a supervisor needs.
                health_path: "/health",
                context_from_health: None,
                startup_timeout,
                env: library_path_env(&program),
                offload_from_log: Some(offload_from_log),
            }),
        }
    }

    pub fn recent_log(&self) -> Vec<String> {
        self.engine.recent_log()
    }

    /// The binary this backend will run.
    pub fn program(&self) -> &Path {
        self.engine.program()
    }
}

/// Where a resolved engine came from — shown beside the path, because "which llama-server is
/// this" is the first question when offload does not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramSource {
    /// `backend.llama_server_path`.
    Configured,
    /// Beside the Studio's own executable, or in `engines/` beside it.
    BesideApp,
    /// Found on `PATH`.
    Path,
    /// Nowhere: the bare name, so the error names what is missing.
    Missing,
}

/// Where the engine binary is.
pub fn resolve_program(configured: Option<PathBuf>) -> PathBuf {
    resolve_program_with_source(configured).0
}

/// Where the engine binary is, and how it was found.
pub fn resolve_program_with_source(configured: Option<PathBuf>) -> (PathBuf, ProgramSource) {
    let exe_name = engine_file_name();

    if let Some(path) = configured {
        return (path, ProgramSource::Configured);
    }
    // Beside the Studio's own executable: how the packaged app ships an engine.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join(exe_name), dir.join("engines").join(exe_name)] {
            if candidate.is_file() {
                return (candidate, ProgramSource::BesideApp);
            }
        }
    }
    if let Some(found) = which(exe_name) {
        return (found, ProgramSource::Path);
    }
    // Not found: return the bare name so the error names the thing that is missing rather than
    // an absolute path that never existed.
    (PathBuf::from(exe_name), ProgramSource::Missing)
}

/// `llama-server`, or `llama-server.exe`.
pub fn engine_file_name() -> &'static str {
    if cfg!(windows) { "llama-server.exe" } else { "llama-server" }
}

/// A minimal `which`, to avoid a dependency for eleven lines.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|c| c.is_file())
}

/// The environment an engine needs to find the libraries beside it.
///
/// A release tarball is `llama-server` plus `libllama.so`, `libggml-vulkan.so` and the rest in
/// one directory, and on Linux a binary finds a library beside itself only if it was linked with
/// `$ORIGIN` in its rpath — which upstream's has not always been. Putting the binary's own
/// directory first on `LD_LIBRARY_PATH` costs nothing when the rpath is right and is the
/// difference between "GPU" and "cannot open shared object file" when it is not. macOS uses
/// `@rpath`/`@loader_path` and strips `DYLD_*` for hardened binaries; Windows searches the
/// executable's directory by default. Neither needs help.
fn library_path_env(program: &Path) -> Vec<(String, String)> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    let Some(dir) = program.parent().filter(|d| d.is_dir()) else { return Vec::new() };
    let mut value = dir.display().to_string();
    if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH").filter(|v| !v.is_empty()) {
        value.push(':');
        value.push_str(&existing.to_string_lossy());
    }
    vec![("LD_LIBRARY_PATH".to_string(), value)]
}

/// The command line.
///
/// Conservative on purpose: only flags `llama-server` has accepted for years, because a user's
/// engine build may be any age and an unknown flag makes it exit with a usage message instead of
/// loading. Anything newer goes through `backend.extra_args`, where the user owns the risk — and
/// through [`LlamaCppBackend::load`], which adds `-lv 4` only to an engine it has already seen
/// answer `--list-devices`, a flag of the same vintage.
fn build_args(request: &LoadRequest, port: u16) -> Vec<String> {
    let mut args = vec![
        "--model".into(),
        request.model_path.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--ctx-size".into(),
        request.context_size.to_string(),
        // So the engine's own /v1/models reports the id the Studio uses.
        "--alias".into(),
        request.model_id.clone(),
    ];
    if let Some(layers) = request.gpu_layers {
        args.push("--n-gpu-layers".into());
        args.push(layers.to_string());
    }
    if let Some(threads) = request.threads {
        args.push("--threads".into());
        args.push(threads.to_string());
    }
    match request.flash_attention {
        // Nothing at all: the engine's own default is `auto`, and every engine old enough not to
        // have a default is also old enough to reject the value form of the flag.
        FlashAttention::Auto => {}
        FlashAttention::On => {
            args.push("--flash-attn".into());
            args.push("on".into());
        }
        FlashAttention::Off => {
            args.push("--flash-attn".into());
            args.push("off".into());
        }
    }
    if !request.use_mmap {
        args.push("--no-mmap".into());
    }
    if request.use_mlock {
        args.push("--mlock".into());
    }
    if request.needs_default_chat_template {
        // ChatML: what current llama.cpp would pick anyway, named here so that stays true when
        // the engine's default changes. A model that ships its own template never reaches this
        // branch — the engine uses that one, which is what `h_M` binds.
        args.push("--chat-template".into());
        args.push("chatml".into());
    }
    args.extend(request.extra_args.iter().cloned());
    args
}

/// The verbosity at which `llama-server` narrates the load: `device_info`, `using device …`,
/// `offloaded N/M layers` and the per-device buffer sizes. Its default (3) prints none of them.
const LOAD_LOG_VERBOSITY: &str = "4";

impl InferenceBackend for LlamaCppBackend {
    fn name(&self) -> &'static str {
        "llamacpp"
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async move {
            let probe = self.engine.probe().await;
            let tag = accelerator_tag_for(probe.devices.as_deref()).unwrap_or(self.probed_accelerator_tag.as_str());
            self.engine.descriptor(tag).await
        })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            let availability = self
                .engine
                .availability(
                    "Install llama.cpp (its `llama-server` binary) — Settings → Backend can download a build for this \
                     machine's GPU — or set backend.llama_server_path to a build you already have.",
                )
                .await;
            // Say what the build can drive, beside its version: the line people read when a model
            // is slow, and the difference between a Metal build and a CPU one is not in the banner.
            match availability {
                Availability::Available { detail } => {
                    let probe = self.engine.probe().await;
                    let devices = match probe.devices.as_deref() {
                        Some([]) => " · CPU-only build (no GPU device)".to_string(),
                        Some(list) => match super::devices::first_gpu(list) {
                            Some(gpu) => format!(" · GPU: {}", gpu.label()),
                            None => " · CPU-only build (no GPU device)".to_string(),
                        },
                        None => " · too old to list its devices".to_string(),
                    };
                    Availability::Available { detail: format!("{detail}{devices}") }
                }
                unavailable => unavailable,
            }
        })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move {
            let probe = self.engine.probe().await;
            let mut request = request;
            if probe.devices.is_some() {
                // An engine that answers `--list-devices` also takes `-lv`; both arrived with the
                // same argument parser. First, so the user's own `extra_args` can still override.
                request.extra_args.splice(0..0, ["-lv".to_string(), LOAD_LOG_VERBOSITY.to_string()]);
            }
            let asked = request.gpu_layers;
            let mut loaded = self.engine.load(request).await?;
            if loaded.offload.is_none() {
                // The log did not say (an old engine, or the lines are gone): the device list is
                // the next best witness, and it says which kind of witness it is.
                let offload = offload_from_devices(probe.devices.as_deref(), asked);
                loaded.gpu_layers = offload.layers;
                loaded.offload = Some(offload);
            }
            Ok(loaded)
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { self.engine.unload().await })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>> {
        Box::pin(async move { self.engine.loaded().await })
    }

    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<StreamEvent>>>> {
        Box::pin(async move { self.engine.generate(request).await })
    }

    fn devices(&self) -> BoxFuture<'_, Option<Vec<EngineDevice>>> {
        Box::pin(async move { self.engine.probe().await.devices })
    }

    /// `llama-server` prefills a trailing assistant message by default (`--prefill-assistant`,
    /// measured in build 10330's help): the answer's continuation, not a new answer.
    fn continues_assistant_turn(&self) -> bool {
        true
    }
}

/// Which accelerator tag this machine should use, from what the hardware probe found.
pub fn accelerator_tag(hardware: &misaka_studio_core::HardwareSnapshot) -> &'static str {
    use misaka_studio_core::hardware::AcceleratorKind;
    match hardware.accelerators.iter().map(|a| a.kind).find(|k| *k != AcceleratorKind::Cpu) {
        Some(AcceleratorKind::Cuda) => "cuda",
        Some(AcceleratorKind::AppleUnified) => "metal",
        Some(AcceleratorKind::Rocm) => "rocm",
        Some(AcceleratorKind::Vulkan) => "vulkan",
        _ => "cpu",
    }
}

/// True when `path` looks like a directory of MLX weights rather than a GGUF.
pub fn is_gguf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> LoadRequest {
        LoadRequest {
            model_id: "Qwen3-4B-Q4_K_M".into(),
            model_path: PathBuf::from("/models/Qwen3-4B-Q4_K_M.gguf"),
            context_size: 8192,
            gpu_layers: Some(33),
            threads: Some(8),
            flash_attention: FlashAttention::On,
            use_mmap: true,
            use_mlock: false,
            needs_default_chat_template: false,
            extra_args: vec!["--verbose".into()],
        }
    }

    #[test]
    fn the_command_line_carries_the_load_request() {
        let args = build_args(&request(), 5599);
        let joined = args.join(" ");
        assert!(joined.contains("--model /models/Qwen3-4B-Q4_K_M.gguf"));
        assert!(joined.contains("--port 5599"));
        assert!(joined.contains("--ctx-size 8192"));
        assert!(joined.contains("--n-gpu-layers 33"));
        assert!(joined.contains("--threads 8"));
        assert!(joined.contains("--flash-attn on"));
        assert!(joined.ends_with("--verbose"), "extra args go last so they can override");
        assert!(!joined.contains("--no-mmap"));
    }

    /// The engine must never be told to listen anywhere but loopback: it has no authentication
    /// of its own, and the Studio's API key check happens in front of it.
    #[test]
    fn the_engine_only_ever_binds_loopback() {
        let args = build_args(&request(), 1234);
        let host = args.iter().position(|a| a == "--host").map(|i| args[i + 1].clone());
        assert_eq!(host.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn absent_options_are_absent_flags() {
        let mut req = request();
        req.gpu_layers = None;
        req.threads = None;
        req.flash_attention = FlashAttention::Auto;
        req.extra_args.clear();
        let joined = build_args(&req, 1).join(" ");
        assert!(!joined.contains("--n-gpu-layers"));
        assert!(!joined.contains("--threads"));
        assert!(!joined.contains("--flash-attn"), "auto says nothing at all: {joined}");
        // The verbosity that reads the placement is not part of the base command line: it is
        // added at load, and only for an engine known to accept it.
        assert!(!joined.contains("-lv"), "{joined}");
    }

    /// The 400-with-a-C++-message bug: a model with no template of its own must have one supplied,
    /// and a model that has one must not be overridden.
    #[test]
    fn a_model_without_a_chat_template_is_given_one() {
        let mut req = request();
        req.needs_default_chat_template = true;
        let joined = build_args(&req, 1).join(" ");
        assert!(joined.contains("--chat-template chatml"), "got {joined}");

        assert!(!build_args(&request(), 1).join(" ").contains("--chat-template"));
    }

    #[test]
    fn mlock_and_no_mmap_are_passed_when_asked_for() {
        let mut req = request();
        req.use_mmap = false;
        req.use_mlock = true;
        let joined = build_args(&req, 1).join(" ");
        assert!(joined.contains("--no-mmap"));
        assert!(joined.contains("--mlock"));
    }

    #[test]
    fn a_configured_path_wins() {
        let configured = PathBuf::from("/opt/llama/llama-server");
        assert_eq!(resolve_program(Some(configured.clone())), configured);
        assert_eq!(resolve_program_with_source(Some(configured.clone())).1, ProgramSource::Configured);
    }

    /// An engine nobody installed resolves to its bare name and says so, so that the message a
    /// user sees names `llama-server` and not a path that never existed.
    #[test]
    fn a_missing_engine_is_named_not_pathed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty_path = dir.path().display().to_string();
        // A PATH with nothing on it, restored afterwards; `which` reads the real one otherwise.
        let saved = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", &empty_path) };
        let (program, source) = resolve_program_with_source(None);
        match saved {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        if source == ProgramSource::Missing {
            assert_eq!(program, PathBuf::from(engine_file_name()));
        } else {
            // A packaged layout with an engine beside the test binary is a legitimate find.
            assert_eq!(source, ProgramSource::BesideApp);
        }
    }
}
