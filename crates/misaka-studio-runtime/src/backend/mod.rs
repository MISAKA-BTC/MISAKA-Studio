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

pub mod devices;
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
    /// The templated prompt's length, counted with the model's own tokenizer, when the context
    /// manager had one. An engine that sizes its decode ceiling against a fixed window uses it in
    /// place of its own estimate; `None` means nobody counted exactly.
    pub prompt_tokens: Option<u64>,
    /// Ask a reasoning model to answer without thinking first (`chat_template_kwargs.enable_thinking
    /// = false`). Measured on Qwen3.5-2B: asked for a 120-token summary, it spent all 120 on a
    /// "Thinking Process" that arrives as `reasoning_content`, and the content was empty. Ignored
    /// by templates that do not read the flag.
    pub disable_thinking: bool,
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
    /// Layers on the accelerator: what the engine's own account says when it gave one
    /// (`offload`), otherwise what was asked for. `Some(0)` is a CPU run.
    pub gpu_layers: Option<u32>,
    /// How long the load took. The number people want when deciding whether to keep a model
    /// resident.
    pub load_ms: u64,
    /// What became of the offload request, with the evidence for it. `None` for engines that have
    /// no accelerator to speak of (the integer runtime, a remote gateway, the mock).
    #[serde(default)]
    pub offload: Option<devices::Offload>,
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

    /// The devices this engine can put layers on, from the engine itself.
    ///
    /// `None` means the engine cannot say — it is the answer for engines with no offload at all
    /// and for a `llama-server` too old to list its devices — and the caller then plans from the
    /// hardware probe as it always did. `Some(empty)` is a definite "none": a CPU-only build on a
    /// machine that may well have a GPU, which is the case the planner must not offload into.
    fn devices(&self) -> BoxFuture<'_, Option<Vec<devices::EngineDevice>>> {
        Box::pin(async { None })
    }

    /// Whether a conversation that ENDS with an assistant turn is continued from inside that turn.
    ///
    /// `llama-server` does this by default (`--prefill-assistant`): the template leaves the last
    /// assistant turn open and the model writes its next token. An engine whose template closes
    /// every turn and opens a fresh `assistant` — the free-prompt gateway's, fixed by the class —
    /// would instead answer the partial text as if it were finished, so it gets the continuation
    /// spelled out as an instruction ([`continuation_as_instruction`]). `false` is the safe default.
    fn continues_assistant_turn(&self) -> bool {
        false
    }
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
/// **The share of a context window kept for the answer, before the question is sent.**
///
/// A window is shared between the conversation and the reply, and only one of the two is visible
/// while it fills. On `qwen25-1.5b-a16` — 512 positions, fixed by the artifact's rotary table —
/// the measured turn was prompt 432, answer 80, cut mid-sentence, with no refusal anywhere: the
/// engine accepted a request that fit and then ran out of window while answering it.
///
/// Half the window, capped by what was actually asked for. Not all of `max_tokens`: the app's
/// default ask is 2048, sized for a 32K GGUF, and reserving that against a small class would leave
/// nothing to remember the conversation with. The floor is there so a very small window still
/// yields a sentence rather than a word.
pub fn answer_room(max_tokens: u64, window: u64) -> u64 {
    const AT_LEAST: u64 = 96;
    max_tokens.min(window / 2).max(AT_LEAST)
}

/// **The context an engine's own refusal names.**
///
/// Engines say the same thing two ways. The free-prompt worker counts the whole request —
/// "prompt 51 + decode ceiling 476 exceeds max_context_tokens 512" — and the artifact runtime
/// counts only the prompt: "the prompt is 524 tokens and this artifact's rotary table covers 512".
/// The second shape had no reader, so a conversation that outgrew the class produced a raw engine
/// string, no answer, and no way for the app to act. Both give the number that matters.
/// **A continuation, spelled out for an engine that cannot continue a turn from inside it.**
///
/// The conversation ends with an assistant reply that was cut off. Rewritten as: the system
/// prompt, then ONE user turn carrying the question, an instruction, and as much of the reply's
/// end as fits `budget`. One turn rather than the whole history because on a 512-token class the
/// history is exactly what does not fit — and a model that sees the question and where it stopped
/// can continue; a model that sees only "continue" cannot.
///
/// `Err` is a sentence for the user when not even the question and a useful tail fit — a reply
/// that already filled the class's window has no room left to be continued, and saying so beats a
/// continuation of nothing.
pub fn continuation_as_instruction(
    messages: &[ChatMessage],
    budget: u64,
    counter: &crate::context::tokens::TokenCounter,
) -> Result<Vec<ChatMessage>, String> {
    /// Below this, the model is not continuing text, it is guessing at a fragment.
    const MIN_TAIL_TOKENS: u64 = 16;
    let Some(last_user) = messages.iter().rposition(|m| m.role == "user") else {
        return Err("there is no question to continue the answer to".to_string());
    };
    let partial: String = messages[last_user + 1..].iter().filter(|m| m.role == "assistant").map(|m| m.content.as_str()).collect();
    if partial.trim().is_empty() {
        return Err("there is no answer to continue".to_string());
    }
    let system: Vec<ChatMessage> = messages.iter().filter(|m| m.role == "system").cloned().collect();
    let question = messages[last_user].content.trim();
    let japanese = question.chars().chain(partial.chars()).any(|c| !c.is_ascii());
    let (header, marker) = if japanese {
        (
            "上の質問への回答が途中で切れました。繰り返さず、前置きなしで、切れた箇所の続きだけを書いてください。",
            "途中までの回答（末尾）:",
        )
    } else {
        (
            "The answer above was cut off. Do not repeat it and add no preamble: write only what comes next.",
            "The answer so far (its end):",
        )
    };
    let compose = |tail: &str| {
        let mut out = system.clone();
        out.push(ChatMessage::new("user", format!("{question}\n\n---\n{header}\n\n{marker}\n{tail}")));
        out
    };

    let chars: Vec<char> = partial.chars().collect();
    let fits = |start: usize| counter.messages(&compose(&chars[start..].iter().collect::<String>())) <= budget;
    if fits(0) {
        return Ok(compose(&partial));
    }
    // The longest suffix that fits: binary search on where it starts.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(mid) { hi = mid } else { lo = mid + 1 }
    }
    let tail: String = chars[lo..].iter().collect();
    let tail_tokens = counter.text(&tail);
    if lo >= chars.len() || !fits(lo) || tail_tokens < MIN_TAIL_TOKENS {
        return Err(if japanese {
            "この回答はモデルの文脈をほぼ使い切っていて、質問と途中の回答を渡すと続きを書く余地が残りません。新しいチャットで、残りの部分だけを質問してください（例:「(2)だけ解いてください」）。".to_string()
        } else {
            "This answer already fills the model's context: with the question and the answer so far there is no room left to continue. Ask for the remaining part on its own in a new chat.".to_string()
        });
    }
    Ok(compose(&tail))
}

pub fn context_limit_from_refusal(message: &str) -> Option<u64> {
    let after = |needle: &str| -> Option<u64> {
        let rest = message.split(needle).nth(1)?;
        let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    after("max_context_tokens ").or_else(|| after("rotary table covers "))
}

/// **The prompt length an engine's own refusal names**, in the engine's tokens.
///
/// "prompt 51 + decode ceiling 476 exceeds max_context_tokens 512" and "the prompt is 524 tokens
/// and this artifact's rotary table covers 512" both carry it. Set against what the app counted
/// for the same prompt, it says how far off the count was — which is what the retry plans with.
pub fn refusal_prompt_tokens(message: &str) -> Option<u64> {
    let after = |needle: &str| -> Option<u64> {
        let rest = message.split(needle).nth(1)?;
        let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    after("the prompt is ").or_else(|| after("prompt "))
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

    /// The engine's count of the prompt, from either spelling — what a retry measures the app's
    /// own count against.
    #[test]
    fn either_refusal_names_the_prompt_length() {
        assert_eq!(
            refusal_prompt_tokens("the worker refused the job: prompt 51 + decode ceiling 476 exceeds max_context_tokens 512"),
            Some(51)
        );
        assert_eq!(refusal_prompt_tokens("misaka: the prompt is 524 tokens and this artifact's rotary table covers 512"), Some(524));
        assert_eq!(refusal_prompt_tokens("connection refused"), None);
    }

    /// The turn that started this: `qwen25-1.5b-a16` holds 512 positions, the app asks for its
    /// 2048-token default, and the reserve has to come out of the window rather than the ask.
    #[test]
    fn the_answers_half_is_reserved_out_of_the_window_not_out_of_the_ask() {
        assert_eq!(answer_room(2048, 512), 256, "half the class, not the app's 32K-sized default");
        assert_eq!(answer_room(128, 512), 128, "a smaller ask is the ask — the reserve never invents length");
        assert_eq!(answer_room(2048, 32_768), 2048, "a window with room to spare reserves only what was asked for");
        assert_eq!(answer_room(2048, 64), 96, "and a tiny window still leaves enough for a sentence");
    }

    /// The instruction form carries the question and the END of the partial answer, fits the
    /// budget, and says what to do in the language of the chat.
    #[test]
    fn a_continuation_for_a_small_window_carries_the_question_and_the_tail() {
        let counter = crate::context::tokens::TokenCounter::estimate();
        let question = "原点 O を中心とする半径 1 の円に円外の点 P から 2 本の接線を引く。中点 Q の座標を求めよ。";
        let partial = format!("{}これらを x_1 と", "接線の方程式は l_1: y - y_0 = m(x - x_0) です。".repeat(30));
        let messages = vec![msg("system", "日本語で答えてください。"), msg("user", question), msg("assistant", &partial)];
        let rewritten = continuation_as_instruction(&messages, 256, &counter).expect("fits");
        assert_eq!(rewritten.len(), 2, "system + one user turn");
        assert_eq!(rewritten[1].role, "user", "never ends with an assistant turn the template would close");
        let content = &rewritten[1].content;
        assert!(content.starts_with(question), "the question is there, whole");
        assert!(content.contains("繰り返さず"), "the instruction is in Japanese: {content}");
        assert!(content.ends_with("これらを x_1 と"), "the tail is the END of the answer: {content}");
        assert!(counter.messages(&rewritten) <= 256);

        let short = vec![msg("user", "Explain RSA."), msg("assistant", "RSA relies on the difficulty of factoring")];
        let whole = continuation_as_instruction(&short, 10_000, &counter).expect("fits");
        assert!(whole[0].content.contains("Do not repeat"), "English chat, English instruction");
        assert!(whole[0].content.ends_with("difficulty of factoring"));
    }

    /// No room is a sentence, not a continuation of nothing.
    #[test]
    fn a_continuation_with_no_room_says_so() {
        let counter = crate::context::tokens::TokenCounter::estimate();
        let messages = vec![msg("user", &"長い質問".repeat(80)), msg("assistant", "答えの途中")];
        let refused = continuation_as_instruction(&messages, 256, &counter).unwrap_err();
        assert!(refused.contains("新しいチャット"), "{refused}");
        assert!(continuation_as_instruction(&[msg("assistant", "x")], 256, &counter).is_err(), "nothing to continue to");
    }
}
