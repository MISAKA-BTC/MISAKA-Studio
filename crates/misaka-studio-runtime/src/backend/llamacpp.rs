//! The llama.cpp backend — `llama-server`, supervised.
//!
//! This is the default engine on every platform the Studio targets: CUDA on Windows and Linux,
//! Metal on Apple Silicon, plain CPU everywhere else. It is also the one the MISAKA network's PALW
//! work already uses, which matters for the long path — the artifacts a validator pins are
//! llama.cpp GGUFs under a pinned llama.cpp build.
//!
//! # Finding the binary
//!
//! The one search order every component shares (`crate::components::resolve_component`,
//! ADR-0096 Decision 10): the configured path — someone who built llama.cpp with flags they care
//! about — then beside the Studio executable and its `engines/`, where the packaged app ships or
//! installs an engine, then `PATH` for a developer with `llama-server` system-wide.
//!
//! When none of them has it, the backend reports unavailable *with the remedy*, and the app
//! keeps running on the mock backend rather than failing to start. A local-LLM app that refuses
//! to open because an engine is missing has made the user's first problem unsolvable from inside
//! the app.

use super::openai_child::{ChildEngine, ChildEngineConfig};
use super::{Availability, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, RuntimeFingerprint, StreamEvent};
use crate::Result;
use crate::components::{ComponentId, resolve_component};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use misaka_studio_core::settings::FlashAttention;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct LlamaCppBackend {
    engine: ChildEngine,
    accelerator_tag: String,
}

impl LlamaCppBackend {
    /// The name this backend answers to, everywhere.
    pub const NAME: &'static str = "llamacpp";

    /// `configured` is `backend.llama_server_path`; `accelerator_tag` is `cuda`, `metal`, `rocm`
    /// or `cpu` and becomes part of the determinism class, because the same source built for a
    /// different accelerator is different arithmetic.
    pub fn new(configured: Option<PathBuf>, accelerator_tag: impl Into<String>, startup_timeout: Duration) -> Self {
        let resolution = resolve_component(&ComponentId::LlamaServer, configured.as_deref(), None);
        LlamaCppBackend {
            accelerator_tag: accelerator_tag.into(),
            engine: ChildEngine::new(ChildEngineConfig {
                name: Self::NAME,
                program: resolution.path,
                program_candidate: resolution.candidate,
                args: Box::new(build_args),
                // llama-server answers /health with 503 while the model loads and 200 once it is
                // ready, which is exactly the signal a supervisor needs.
                health_path: "/health",
                startup_timeout,
                env: Vec::new(),
                load_env: None,
            }),
        }
    }

    pub fn recent_log(&self) -> Vec<String> {
        self.engine.recent_log()
    }
}

/// Where the engine binary is — the one search order, for this component. Kept under its old
/// name and signature for the callers that had it; the order itself is spelled once, in
/// `crate::components`.
pub fn resolve_program(configured: Option<PathBuf>) -> PathBuf {
    resolve_component(&ComponentId::LlamaServer, configured.as_deref(), None).path
}

/// The command line.
///
/// Conservative on purpose: only flags `llama-server` has accepted for years, because a user's
/// engine build may be any age and an unknown flag makes it exit with a usage message instead of
/// loading. Anything newer goes through `backend.extra_args`, where the user owns the risk.
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

impl InferenceBackend for LlamaCppBackend {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn fingerprint(&self) -> RuntimeFingerprint {
        let mut fingerprint = self.engine.fingerprint();
        fingerprint.extra.insert("accelerator_tag".into(), self.accelerator_tag.clone());
        fingerprint
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async move { self.engine.descriptor(&self.accelerator_tag).await })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            self.engine
                .availability(
                    "Install llama.cpp (its `llama-server` binary), or set backend.llama_server_path in Settings \
                     to a build you already have.",
                )
                .await
        })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move { self.engine.load(request).await })
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
    }

    /// The fingerprint is the constructor's inputs and nothing else: the program as resolved,
    /// where it was found, the timeout and the accelerator tag. Two instances built from the
    /// same inputs agree; a different input shows up as a different value, not as a rebuild
    /// nobody can explain.
    #[test]
    fn the_fingerprint_is_the_constructors_inputs() {
        let a = LlamaCppBackend::new(Some(PathBuf::from("/opt/llama/llama-server")), "metal", Duration::from_secs(30)).fingerprint();
        let b = LlamaCppBackend::new(Some(PathBuf::from("/opt/llama/llama-server")), "metal", Duration::from_secs(30)).fingerprint();
        assert_eq!(a, b);
        assert_eq!(a.kind, "llamacpp");
        assert_eq!(a.program.as_deref(), Some(Path::new("/opt/llama/llama-server")));
        assert_eq!(a.startup_timeout_secs, Some(30));
        assert_eq!(a.extra.get("accelerator_tag").map(String::as_str), Some("metal"));
        assert_eq!(a.extra.get("program_candidate").map(String::as_str), Some("configured"));
        assert!(a.url.is_none() && a.token_sha256_prefix.is_none() && a.tokenizer.is_none());

        let timeout =
            LlamaCppBackend::new(Some(PathBuf::from("/opt/llama/llama-server")), "metal", Duration::from_secs(31)).fingerprint();
        assert_ne!(a, timeout);
        let tag = LlamaCppBackend::new(Some(PathBuf::from("/opt/llama/llama-server")), "cuda", Duration::from_secs(30)).fingerprint();
        assert_ne!(a, tag);
    }
}
