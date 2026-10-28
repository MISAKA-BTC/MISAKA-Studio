//! A backend that generates text without a model.
//!
//! It exists for three things that are otherwise impossible: testing the streaming path in CI
//! (no GPU, no 4 GB download), building the UI without waiting on a load, and giving a first-run
//! user something that responds before they have downloaded anything.
//!
//! Its output is a **deterministic function of the prompt**, which is what makes it usable as a
//! test fixture: the same request produces the same bytes, so the inference hash over a mock run
//! is stable and the provenance path can be tested end to end.
//!
//! It is never a silent fallback. Selecting it is explicit, every reply says what it is, and its
//! runtime class tag (`misaka-studio-mock/v1`) is distinct — so a record produced here can never
//! be confused for one produced by a real engine.

use super::{
    Availability, GeneratedImage, GenerationRequest, ImageBackend, ImageLoadRequest, InferenceBackend, LoadRequest, LoadedImageModel,
    LoadedModel, StreamEvent, Txt2ImgRequest, Usage, approximate_tokens,
};
use crate::{Error, Result};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use std::sync::Mutex;
use std::time::Duration;

/// The class tag every mock record carries. Distinct by construction: nothing that ran on real
/// weights can land in this class.
pub const MOCK_CLASS_TAG: &str = "misaka-studio-mock/v1";

pub struct MockBackend {
    loaded: Mutex<Option<LoadedModel>>,
    /// Delay between tokens. Zero in tests, ~20 ms when driving the UI so streaming looks like
    /// streaming.
    token_delay: Duration,
}

impl MockBackend {
    pub fn new(token_delay: Duration) -> Self {
        MockBackend { loaded: Mutex::new(None), token_delay }
    }

    /// The reply, as a pure function of the request.
    fn compose(request: &GenerationRequest) -> String {
        let ask = request
            .prompt
            .clone()
            .or_else(|| request.messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()))
            .unwrap_or_default();
        let trimmed: String = ask.chars().take(160).collect();
        format!(
            "This is the MISAKA Studio mock runtime — no model is loaded, so nothing here was inferred. \
             You said: \"{trimmed}\". Load a GGUF from the Models tab to get real answers; every reply \
             then carries its model hash, runtime identity and inference hash."
        )
    }

    /// The reasoning a thinking model would stream ahead of that reply — only when the last user
    /// turn carries Qwen3's `/think` switch and the request did not turn thinking off.
    ///
    /// Opt-in, so every other mock reply, and every fixture built on one, is byte-for-byte what it
    /// was; and a pure function of the request like the reply. What it is for is the UI: the
    /// thinking display can be built and tested against `--backend mock`, with no model loaded.
    fn compose_reasoning(request: &GenerationRequest) -> Option<String> {
        if request.disable_thinking || request.prompt.is_some() {
            return None;
        }
        let ask = request.messages.iter().rev().find(|m| m.role == "user")?;
        ask.content.contains("/think").then(|| {
            format!(
                "The user wrote {} characters and asked me to think first. This is the mock runtime, so there is \
                 nothing to reason about; these words only exercise the reasoning stream.",
                ask.content.chars().count()
            )
        })
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new(Duration::from_millis(20))
    }
}

impl InferenceBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async {
            RuntimeDescriptor {
                backend: "mock".into(),
                engine_commit: "mock".into(),
                engine_patch_sha256: "unpatched".into(),
                engine_build_number: 0,
                build_profile: "mock/deterministic/v1".into(),
                class_tag: MOCK_CLASS_TAG.into(),
            }
        })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async { Availability::Available { detail: "built in; generates canned text".into() } })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move {
            let model = LoadedModel {
                model_id: request.model_id,
                context_size: request.context_size,
                gpu_layers: Some(0),
                load_ms: 0,
                offload: None,
            };
            *self.loaded.lock().expect("mock lock") = Some(model.clone());
            Ok(model)
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            *self.loaded.lock().expect("mock lock") = None;
            Ok(())
        })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>> {
        Box::pin(async { self.loaded.lock().expect("mock lock").clone() })
    }

    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<StreamEvent>>>> {
        let delay = self.token_delay;
        Box::pin(async move {
            let text = Self::compose(&request);
            let prompt_tokens = approximate_tokens(
                &request
                    .prompt
                    .clone()
                    .unwrap_or_else(|| request.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ")),
            );
            // Split on spaces but keep them, so reassembling the deltas reproduces the text
            // exactly — a stream whose concatenation differs from the whole answer is the bug
            // this shape prevents. Reasoning, when asked for, comes first and spends the same
            // token budget, as a real thinking model's does.
            let reasoning = Self::compose_reasoning(&request).unwrap_or_default();
            let chunks: Vec<StreamEvent> = reasoning
                .split_inclusive(' ')
                .map(|c| StreamEvent::Reasoning(c.to_string()))
                .chain(text.split_inclusive(' ').map(|c| StreamEvent::Delta(c.to_string())))
                .collect();
            let completion_tokens = chunks.len() as u64;
            let limit = request.params.max_tokens.max(1);

            let stream = async_stream(move |tx| async move {
                for (i, chunk) in chunks.into_iter().enumerate() {
                    if i as u64 >= limit {
                        let _ = tx
                            .send(Ok(StreamEvent::Done {
                                usage: Usage { prompt_tokens, completion_tokens: limit, total_tokens: prompt_tokens + limit },
                                finish_reason: "length".into(),
                            }))
                            .await;
                        return;
                    }
                    if tx.send(Ok(chunk)).await.is_err() {
                        return; // The client hung up; stop generating.
                    }
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                }
                let _ = tx
                    .send(Ok(StreamEvent::Done {
                        usage: Usage { prompt_tokens, completion_tokens, total_tokens: prompt_tokens + completion_tokens },
                        finish_reason: "stop".into(),
                    }))
                    .await;
            });
            Ok(stream)
        })
    }
}

/// The image-generation seam's "nothing configured" state.
///
/// The chat seam's `BackendKind::Auto` always resolves to a real engine, but there is no
/// platform-independent default image engine to fall back to — a
/// diffusion sidecar is an opt-in install, not something to launch unasked. This is what
/// `image_backend.kind: none` (the default) builds, and it says so plainly rather than the
/// generic "no model loaded" a missing chat engine would produce.
pub struct NoneImageBackend;

impl ImageBackend for NoneImageBackend {
    fn name(&self) -> &'static str {
        "none"
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            Availability::Unavailable {
                reason: "no image backend is configured".into(),
                remedy: "Set image_backend.kind to \"diffusion\" in Settings, and point python_path/diffusion_server_path at \
                         sidecars/diffusion/server.py."
                    .into(),
            }
        })
    }

    fn load(&self, _request: ImageLoadRequest) -> BoxFuture<'_, Result<LoadedImageModel>> {
        Box::pin(async {
            Err(Error::BackendUnavailable {
                backend: "none".into(),
                reason: "no image backend is configured".into(),
                remedy: "Set image_backend.kind to \"diffusion\" in Settings.".into(),
            })
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedImageModel>> {
        Box::pin(async { None })
    }

    fn generate(&self, _request: Txt2ImgRequest) -> BoxFuture<'_, Result<Vec<GeneratedImage>>> {
        Box::pin(async {
            Err(Error::BackendUnavailable {
                backend: "none".into(),
                reason: "no image backend is configured".into(),
                remedy: "Set image_backend.kind to \"diffusion\" in Settings.".into(),
            })
        })
    }
}

/// Turn a task that sends into a channel into a stream.
///
/// A small helper rather than a dependency on `async-stream`: the whole need is "spawn a producer,
/// hand back the receiver as a stream", and a bounded channel gives the backpressure that keeps a
/// slow client from making the producer race ahead.
pub(crate) fn async_stream<F, Fut>(producer: F) -> BoxStream<'static, Result<StreamEvent>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<Result<StreamEvent>>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(producer(tx));
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ChatMessage;
    use futures_util::StreamExt;
    use misaka_studio_core::provenance::SamplingCommitment;

    fn request(text: &str) -> GenerationRequest {
        GenerationRequest {
            prompt_tokens: None,
            disable_thinking: false,
            model: "mock".into(),
            messages: vec![ChatMessage::new("user", text)],
            prompt: None,
            params: SamplingCommitment::default(),
            stop: Vec::new(),
        }
    }

    async fn collect(backend: &MockBackend, req: GenerationRequest) -> (String, Usage) {
        let mut stream = backend.generate(req).await.expect("generates");
        let mut text = String::new();
        let mut usage = Usage::default();
        while let Some(event) = stream.next().await {
            match event.expect("no error") {
                StreamEvent::Delta(d) => text.push_str(&d),
                StreamEvent::Reasoning(_) => {}
                StreamEvent::Done { usage: u, .. } => usage = u,
            }
        }
        (text, usage)
    }

    /// `/think` in the question turns on a reasoning stream ahead of the reply; the reply itself
    /// is unchanged, and `disable_thinking` turns the reasoning back off.
    #[tokio::test]
    async fn think_asks_the_mock_to_reason_first() {
        let backend = MockBackend::new(Duration::ZERO);
        let events = |req: GenerationRequest| {
            let backend = &backend;
            async move { backend.generate(req).await.expect("generates").map(|e| e.expect("no error")).collect::<Vec<_>>().await }
        };
        let thinking = events(request("why? /think")).await;
        let reasoning: String =
            thinking.iter().filter_map(|e| if let StreamEvent::Reasoning(r) = e { Some(r.as_str()) } else { None }).collect();
        let answer: String =
            thinking.iter().filter_map(|e| if let StreamEvent::Delta(d) = e { Some(d.as_str()) } else { None }).collect();
        assert!(reasoning.contains("think first"), "{reasoning}");
        assert_eq!(answer, MockBackend::compose(&request("why? /think")));
        let first_delta = thinking.iter().position(|e| matches!(e, StreamEvent::Delta(_))).unwrap();
        assert!(thinking[..first_delta].iter().all(|e| matches!(e, StreamEvent::Reasoning(_))), "reasoning comes first");

        let mut off = request("why? /think");
        off.disable_thinking = true;
        assert!(!events(off).await.iter().any(|e| matches!(e, StreamEvent::Reasoning(_))));
        assert!(!events(request("why?")).await.iter().any(|e| matches!(e, StreamEvent::Reasoning(_))), "opt-in only");
        assert!(!events(request("why? /no_think")).await.iter().any(|e| matches!(e, StreamEvent::Reasoning(_))));
    }

    #[tokio::test]
    async fn the_deltas_reassemble_into_the_whole_answer() {
        let backend = MockBackend::new(Duration::ZERO);
        let (text, usage) = collect(&backend, request("hello there")).await;
        assert_eq!(text, MockBackend::compose(&request("hello there")));
        assert!(text.contains("hello there"), "the mock echoes the prompt");
        assert!(usage.completion_tokens > 0);
        assert_eq!(usage.total_tokens, usage.prompt_tokens + usage.completion_tokens);
    }

    /// The property that makes this usable as a fixture: same input, same bytes. If it ever
    /// stops holding, every provenance test that hashes a mock run becomes flaky.
    #[tokio::test]
    async fn the_same_prompt_gives_the_same_bytes() {
        let backend = MockBackend::new(Duration::ZERO);
        let (a, _) = collect(&backend, request("determinism")).await;
        let (b, _) = collect(&backend, request("determinism")).await;
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn max_tokens_stops_it_and_says_why() {
        let backend = MockBackend::new(Duration::ZERO);
        let mut req = request("hello");
        req.params.max_tokens = 3;
        let mut stream = backend.generate(req).await.expect("generates");
        let mut deltas = 0;
        let mut reason = String::new();
        while let Some(event) = stream.next().await {
            match event.expect("no error") {
                StreamEvent::Delta(_) => deltas += 1,
                StreamEvent::Reasoning(_) => {}
                StreamEvent::Done { finish_reason, .. } => reason = finish_reason,
            }
        }
        assert_eq!(deltas, 3);
        assert_eq!(reason, "length");
    }

    #[tokio::test]
    async fn load_and_unload_are_visible() {
        let backend = MockBackend::new(Duration::ZERO);
        assert!(backend.loaded().await.is_none());
        backend
            .load(LoadRequest {
                model_id: "m".into(),
                model_path: "/tmp/m.gguf".into(),
                context_size: 4096,
                gpu_layers: None,
                threads: None,
                flash_attention: misaka_studio_core::settings::FlashAttention::Auto,
                use_mmap: true,
                use_mlock: false,
                needs_default_chat_template: false,
                extra_args: Vec::new(),
            })
            .await
            .expect("loads");
        assert_eq!(backend.loaded().await.expect("loaded").context_size, 4096);
        backend.unload().await.expect("unloads");
        assert!(backend.loaded().await.is_none());
    }
}
