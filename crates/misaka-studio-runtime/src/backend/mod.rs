//! **The backend seam.**
//!
//! `Studio UI → MISAKA Runtime API → backend → GPU/CPU`. Everything above this module speaks
//! [`GenerationRequest`] and [`StreamEvent`]; everything below it is llama.cpp, or MLX, or one
//! day the deterministic runtime the misakas repository already carries for PALW. Nothing above names
//! an engine, which is the property that makes the engine replaceable.
//!
//! # Why a trait and not an enum
//!
//! An enum would be shorter today and wrong later: adding the MISAKA runtime should not mean
//! editing every match in the codebase. The trait is dyn-compatible through boxed futures
//! ([`BoxFuture`]) rather than `async fn`, which keeps `Arc<dyn InferenceBackend>` usable as the
//! one handle the API layer holds.
//!
//! # What a backend must answer for
//!
//! Not just tokens — **identity**. [`InferenceBackend::descriptor`] returns the
//! [`RuntimeDescriptor`] that becomes `h_R`, and a backend that cannot say which commit and
//! build profile it is must say so with the literal `unknown` rather than inventing a plausible
//! string. An `h_R` derived from a guess is worse than no `h_R`: it is a number that will not
//! match the machine it claims to describe, discovered only when a verification layer starts
//! comparing them.

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::{RuntimeDescriptor, SamplingCommitment};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

pub mod gateway;
pub mod llamacpp;
pub mod misaka;
pub mod mlx;
pub mod mock;
pub mod openai_child;
pub(crate) use openai_child::SseParser;

/// One turn in a conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage { role: role.into(), content: content.into() }
    }
}

/// What to generate, and how.
#[derive(Clone, Debug)]
pub struct GenerationRequest {
    /// Model id as the Studio knows it — the loaded model, checked by the caller.
    pub model: String,
    /// Chat turns. Empty for a raw-completion request.
    pub messages: Vec<ChatMessage>,
    /// Raw prompt, for `/v1/completions`. Mutually exclusive with `messages`.
    pub prompt: Option<String>,
    pub params: SamplingCommitment,
    pub stop: Vec<String>,
}

/// Token accounting, in OpenAI's shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// One event from a generation.
///
/// `Done` carries the usage because that is the only point at which it is known — and because a
/// caller that needs tokens/sec must not have to count tokens itself with a different tokenizer
/// than the one that produced them.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// A chunk of generated text.
    Delta(String),
    /// Generation finished normally.
    Done { usage: Usage, finish_reason: String },
}

/// What to load.
#[derive(Clone, Debug)]
pub struct LoadRequest {
    pub model_id: String,
    pub model_path: PathBuf,
    pub context_size: u32,
    /// Layers to place on the accelerator. `None` lets the backend decide.
    pub gpu_layers: Option<u32>,
    pub threads: Option<u32>,
    pub flash_attention: misaka_studio_core::settings::FlashAttention,
    pub use_mmap: bool,
    pub use_mlock: bool,
    /// The model carries no chat template of its own, so name one explicitly.
    ///
    /// Not a workaround — current llama.cpp already falls back to ChatML on its own (measured).
    /// It is about **which** template, staying fixed. The record's `h_M` binds the GGUF, and the
    /// argument that `(h_M, prompt_commitment)` determines the token sequence holds only because
    /// the template lives in that GGUF. For a model with no template, the renderer is the
    /// engine's built-in default instead — a value that can change between engine versions, which
    /// would silently make the same conversation render to different tokens on a different
    /// machine. Naming it takes that dependency out.
    pub needs_default_chat_template: bool,
    pub extra_args: Vec<String>,
}

/// What a backend reports about the model it currently holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadedModel {
    pub model_id: String,
    pub context_size: u32,
    /// Layers actually on the accelerator, when the engine reports it.
    pub gpu_layers: Option<u32>,
    /// How long the load took. The number people want when deciding whether to keep a model
    /// resident.
    pub load_ms: u64,
}

/// An engine, behind one interface.
pub trait InferenceBackend: Send + Sync {
    /// Stable name: `llamacpp`, `mlx`, `mock`.
    fn name(&self) -> &'static str;

    /// The identity of this engine — what `h_R` is derived from.
    ///
    /// Returned as a future because discovering it usually means running the engine's
    /// `--version` and reading what comes back.
    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor>;

    /// Whether this backend can run on this machine at all. A missing binary is a normal answer,
    /// not an error: the UI lists backends and greys out the ones that are not installed.
    fn availability(&self) -> BoxFuture<'_, Availability>;

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, crate::Result<LoadedModel>>;

    fn unload(&self) -> BoxFuture<'_, crate::Result<()>>;

    /// The currently loaded model, if any.
    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>>;

    /// Generate, as a stream of events.
    ///
    /// The stream is `'static` so the HTTP layer can hand it straight to a response body without
    /// borrowing the backend for the life of the request.
    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, crate::Result<BoxStream<'static, crate::Result<StreamEvent>>>>;
}

/// Whether a backend can be used here, and if not, what would fix it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available {
        detail: String,
    },
    /// Present in principle, missing something concrete. `remedy` is shown to the user, so it is
    /// an instruction ("install llama.cpp, or set backend.llama_server_path") rather than a
    /// diagnosis.
    Unavailable {
        reason: String,
        remedy: String,
    },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available { .. })
    }
}

/// A backend handle, shared.
pub type SharedBackend = Arc<dyn InferenceBackend>;

/// Render chat turns into a single prompt.
///
/// A fallback, used only by backends that have no chat endpoint of their own. The real chat
/// template lives in the GGUF and llama.cpp applies it; a house format applied on top of a model
/// trained on a different one is the classic cause of "the model repeats itself" and
/// "it never stops generating".
pub fn render_fallback_prompt(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        out.push_str(&format!("<|{}|>\n{}\n", m.role, m.content));
    }
    out.push_str("<|assistant|>\n");
    out
}

/// A rough token count for text, used only where the engine does not report usage.
///
/// Deliberately crude — ~4 characters per token — and never used to bill anything or to size a
/// context window. Its one job is keeping a tokens/sec readout from being blank.
/// **A conversation trimmed to what the model can actually hold.**
///
/// A class artifact's context is not a setting: `qwen25-1.5b-a16`'s rotary table covers 512
/// positions, that number is part of the root the chain registered, and no app can raise it. What
/// an app CAN do is stop spending it on history the answer does not need — which is what was
/// happening: every turn re-sent the whole conversation, so a two-character question arrived as
/// 524 tokens and was refused outright, with nothing generated and nothing to act on.
///
/// Keeps the system prompt (it is an instruction, not history — dropping it changes the answer's
/// language) and the newest turns, and drops from the oldest until the estimate fits. Returns the
/// kept messages and how many were dropped, so the caller can say so rather than quietly forgetting
/// what the user typed.
///
/// `budget` is the whole context minus whatever room the answer needs; the caller owns that split.
/// The estimate is [`prompt_tokens_upper_bound`]'s, deliberately high — over-counting drops one
/// turn too many, under-counting loses the request.
pub fn fit_messages_to_budget(messages: &[ChatMessage], budget: u64) -> (Vec<ChatMessage>, usize) {
    if prompt_tokens_upper_bound(messages) <= budget {
        return (messages.to_vec(), 0);
    }
    let (system, rest): (Vec<_>, Vec<_>) = messages.iter().cloned().partition(|m| m.role == "system");
    // The newest turn is the question; it is never dropped. If it alone does not fit, the caller
    // gets it back and the error it deserves — a message that says the prompt itself is too long,
    // not one that says the history was.
    let mut kept: Vec<ChatMessage> = Vec::new();
    for m in rest.iter().rev() {
        let mut candidate = system.clone();
        candidate.extend(kept.iter().rev().cloned());
        candidate.push(m.clone());
        let mut ordered = candidate.clone();
        ordered.sort_by_key(|x| if x.role == "system" { 0 } else { 1 });
        if !kept.is_empty() && prompt_tokens_upper_bound(&ordered) > budget {
            break;
        }
        kept.push(m.clone());
    }
    kept.reverse();
    let dropped = rest.len() - kept.len();
    let mut out = system;
    out.extend(kept);
    (out, dropped)
}

/// **The context an engine's own refusal names.**
///
/// Engines say the same thing two ways. The free-prompt worker counts the whole request —
/// "prompt 51 + decode ceiling 476 exceeds max_context_tokens 512" — and the artifact runtime
/// counts only the prompt: "the prompt is 524 tokens and this artifact's rotary table covers 512".
/// The second shape had no reader, so a conversation that outgrew the class produced a raw engine
/// string, no answer, and no way for the app to act. Both give the number that matters.
pub fn context_limit_from_refusal(message: &str) -> Option<u64> {
    let after = |needle: &str| -> Option<u64> {
        let rest = message.split(needle).nth(1)?;
        let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    after("max_context_tokens ").or_else(|| after("rotary table covers "))
}

/// The token cost of a whole conversation, over-estimated on purpose.
///
/// One token per non-ASCII character (CJK sits at roughly one, sometimes more), a quarter of the
/// ASCII, plus the chat template's markers per message. The tokenizer that would answer exactly
/// lives with the engine, so this is the number an app can compute before it asks.
pub fn prompt_tokens_upper_bound(messages: &[ChatMessage]) -> u64 {
    const PER_MESSAGE_MARKERS: u64 = 8;
    messages
        .iter()
        .map(|m| {
            let ascii = m.content.chars().filter(char::is_ascii).count() as u64;
            let other = m.content.chars().count() as u64 - ascii;
            ascii.div_ceil(4) + other + PER_MESSAGE_MARKERS
        })
        .sum()
}

pub fn approximate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_prompt_ends_with_the_assistant_turn() {
        let p = render_fallback_prompt(&[ChatMessage::new("system", "be brief"), ChatMessage::new("user", "hi")]);
        assert!(p.ends_with("<|assistant|>\n"), "got {p:?}");
        assert!(p.contains("be brief"));
    }

    #[test]
    fn token_estimates_are_never_zero_for_non_empty_text() {
        assert_eq!(approximate_tokens(""), 0);
        assert_eq!(approximate_tokens("ab"), 1);
        assert_eq!(approximate_tokens("12345678"), 2);
    }
}

#[cfg(test)]
mod context_fit_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
    }

    /// Both engines say it, and both had to be readable — the second shape is the one a person
    /// actually hit, and it had no reader at all.
    #[test]
    fn either_refusal_names_the_context() {
        assert_eq!(
            context_limit_from_refusal("the worker refused the job: prompt 51 + decode ceiling 476 exceeds max_context_tokens 512"),
            Some(512)
        );
        assert_eq!(
            context_limit_from_refusal("misaka: the prompt is 524 tokens and this artifact's rotary table covers 512"),
            Some(512)
        );
        assert_eq!(context_limit_from_refusal("connection refused"), None, "an unrelated failure must not look like a context limit");
    }

    /// The system prompt is an instruction, not history. Dropping it to make room changes the
    /// answer's language, which is exactly the setting a user just went and set.
    #[test]
    fn the_system_prompt_and_the_question_survive_the_trim() {
        let long = "あ".repeat(300);
        let messages = vec![
            msg("system", "日本語で答えてください。"),
            msg("user", &long),
            msg("assistant", &long),
            msg("user", "Cでhelloworldのコードは"),
        ];
        let (kept, dropped) = fit_messages_to_budget(&messages, 416);
        assert!(dropped > 0, "a conversation past the budget must lose something");
        assert_eq!(kept.first().map(|m| m.role.as_str()), Some("system"), "the instruction stays first");
        assert_eq!(kept.last().map(|m| m.content.as_str()), Some("Cでhelloworldのコードは"), "the question is never dropped");
        assert!(prompt_tokens_upper_bound(&kept) <= 416, "and what is kept must actually fit");
    }

    /// A conversation that already fits is returned untouched — trimming that is not needed is
    /// silent context loss.
    #[test]
    fn a_conversation_that_fits_is_left_alone() {
        let messages = vec![msg("system", "hi"), msg("user", "Cで")];
        let (kept, dropped) = fit_messages_to_budget(&messages, 416);
        assert_eq!(dropped, 0);
        assert_eq!(kept.len(), 2);
    }

    /// One message too long for the class cannot be fixed by dropping history — there is none.
    /// The caller distinguishes the two cases by `dropped == 0`, so this must not silently
    /// return something that fits.
    #[test]
    fn a_single_oversized_message_drops_nothing_and_says_so() {
        let messages = vec![msg("user", &"あ".repeat(900))];
        let (kept, dropped) = fit_messages_to_budget(&messages, 416);
        assert_eq!(dropped, 0, "there was no history to drop");
        assert_eq!(kept.len(), 1, "and the question is still handed back");
        assert!(prompt_tokens_upper_bound(&kept) > 416, "it genuinely does not fit");
    }
}

