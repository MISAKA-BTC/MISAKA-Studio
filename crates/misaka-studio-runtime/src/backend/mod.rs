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
use serde_json::Value;
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
///
/// **The content is one string, whatever shape it arrived in.** Every current OpenAI SDK sends
/// `content` as a list of `{type:"text"}` parts by default — the multimodal-shaped form — and a
/// message type that only read the string form failed a stock client on its first request
/// (ADR-0096 §1.1). So the wire form is [`RawChatMessage`], which accepts both and flattens the
/// parts (joined by `\n`), and this type is what every engine gets: a plain string, which is what
/// a chat template renders and what the record commits to. A part that is not text (`image_url`,
/// `input_audio`, `file`) is refused by name — this surface serves text, and dropping the image
/// silently would send the model a question about a picture it never saw.
///
/// `name`, `tool_calls` and `tool_call_id` are OpenAI's tool-round-trip fields (ADR-0096 Decision
/// 2: a tool call is a turn of text; the round-trip is the app's). They are kept as the SDK sent
/// them and serialized only when present, so an engine that has never heard of them sees the
/// same `{role, content}` it always did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// OpenAI's `tool_calls` on an assistant turn, in OpenAI's own shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    /// Which call a `tool` turn answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage { role: role.into(), content: content.into(), name: None, tool_calls: None, tool_call_id: None }
    }
}

impl<'de> Deserialize<'de> for ChatMessage {
    /// Accepts the wire form and flattens it. A refusal here cannot name the message's position
    /// in `messages[]` — serde hands an element no index — so the request parser converts
    /// [`RawChatMessage`]s itself, with the index; this impl is for every other reader.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        RawChatMessage::deserialize(deserializer)?.into_message(None).map_err(serde::de::Error::custom)
    }
}

/// A message as an OpenAI-shaped client sends it — `content` a string, a list of parts, or
/// `null` (an assistant turn that only carried `tool_calls`).
#[derive(Debug, Deserialize)]
pub struct RawChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<RawContent>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// `content`, in both shapes the SDKs produce.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RawContent {
    Text(String),
    Parts(Vec<Value>),
}

impl RawChatMessage {
    /// Flatten into a [`ChatMessage`]. `index` is the message's position in `messages[]`, when
    /// the caller knows it, so a refusal names the part a person can go and look at.
    pub fn into_message(self, index: Option<usize>) -> std::result::Result<ChatMessage, String> {
        let content = match self.content {
            None => String::new(),
            Some(RawContent::Text(text)) => text,
            Some(RawContent::Parts(parts)) => flatten_content_parts(&parts, index)?,
        };
        Ok(ChatMessage { role: self.role, content, name: self.name, tool_calls: self.tool_calls, tool_call_id: self.tool_call_id })
    }
}

/// The parts of a `content` list, joined by `\n` — or the name of the first part that is not
/// text, with its index, because a refusal that says "unsupported content" sends a person
/// reading a 40-line request body to guess.
pub fn flatten_content_parts(parts: &[Value], message_index: Option<usize>) -> std::result::Result<String, String> {
    let at = |j: usize| match message_index {
        Some(i) => format!("messages[{i}].content[{j}]"),
        None => format!("content[{j}]"),
    };
    let mut texts = Vec::with_capacity(parts.len());
    for (j, part) in parts.iter().enumerate() {
        let Some(kind) = part.get("type").and_then(Value::as_str) else {
            return Err(format!("{}: a content part must be an object with a `type`; got {part}", at(j)));
        };
        if kind != "text" {
            return Err(format!(
                "{}: a `{kind}` part is not text — this surface serves text only (ADR-0096 Decision 1). \
                 Send the text as a string, or as {{\"type\":\"text\"}} parts.",
                at(j)
            ));
        }
        match part.get("text").and_then(Value::as_str) {
            Some(text) => texts.push(text),
            None => return Err(format!("{}: a text part carries its text under `text`; got {part}", at(j))),
        }
    }
    Ok(texts.join("\n"))
}

/// **How far one request may be split into lane jobs** (ADR-0096 Decision 5).
///
/// Carried on the request rather than read from settings by the backend: the gateway backend is
/// built once from the settings it copies at construction, and a limit that lived only there
/// would be the 2026-09-05 bug again — a setting changed in the file and not in the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegLimits {
    /// A trim that drops more turns than this runs a summary job first.
    pub summarize_after_turns: u32,
    /// Follow-up jobs allowed after a `length` finish that delivered less than was asked.
    pub continue_max_legs: u32,
}

impl Default for LegLimits {
    fn default() -> Self {
        LegLimits { summarize_after_turns: 4, continue_max_legs: 2 }
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
    /// OpenAI's `tools`, as sent. Forwarded to engines that implement them (llama-server renders
    /// them into the template; the gateway renders them as the model's own `<tools>` text).
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    /// OpenAI's `response_format`, as sent — shape-checked by the API layer, enforced (or
    /// rendered as advice, and said so) by the engine.
    pub response_format: Option<Value>,
    /// The request's `misaka` extension object (`{require_committed_format?: bool}`), for the
    /// gateway backend. Other engines have nothing to commit and refuse a request that requires it.
    pub misaka: Option<Value>,
    /// How far this request may be split into lane jobs.
    pub legs: LegLimits,
}

impl GenerationRequest {
    /// A plain request: messages or a prompt, the sampling, and nothing of the tool or format
    /// surface. What every backend test and the raw-completion path start from.
    pub fn plain(model: impl Into<String>, messages: Vec<ChatMessage>, prompt: Option<String>, params: SamplingCommitment) -> Self {
        GenerationRequest {
            model: model.into(),
            messages,
            prompt,
            params,
            stop: Vec::new(),
            tools: None,
            tool_choice: None,
            response_format: None,
            misaka: None,
            legs: LegLimits::default(),
        }
    }
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
    /// One entry of OpenAI's streamed `delta.tool_calls[]`, as the engine sent it: `{index, id?,
    /// type?, function: {name?, arguments}}`, where later entries with the same `index` carry
    /// more of `arguments`. llama-server streams these; the gateway sends the parsed calls whole
    /// in its final event. The API layer assembles them by index.
    ToolCallDelta(Value),
    /// Generation finished normally.
    ///
    /// `misaka` is the answer's extension object — the gateway's (job, claim, roots, `jobs[]`,
    /// `context`, `format`) merged with the Studio's own notices (`sampling`, `ignored_fields`);
    /// see [`merge_misaka`] for who wins. `None` from an engine that has nothing to say.
    Done { usage: Usage, finish_reason: String, misaka: Option<Value> },
}

/// **One `misaka` object out of two, and the gateway's word wins.**
///
/// The Studio adds what it knows — that it dropped a temperature, that it ignored `store` — and
/// the gateway adds what it did. Where both name the same key the gateway is describing what
/// RAN, and the app's notice is describing what it asked for, so the gateway's value is the one
/// that must survive: a notice that overwrote the lane's own report would be the app lying about
/// the chain. Objects merge key by key (so `sampling.requested` from the app and
/// `sampling.enforced` from the gateway both live); anything else is replaced whole.
pub fn merge_misaka(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(existing) => merge_misaka(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
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
/// lives with the engine, so this is the number an app can compute before it asks. A turn's
/// `tool_calls` are rendered into the prompt as text by every template that knows them, so their
/// JSON counts too — a call with a long `arguments` string is not free.
pub fn prompt_tokens_upper_bound(messages: &[ChatMessage]) -> u64 {
    const PER_MESSAGE_MARKERS: u64 = 8;
    messages
        .iter()
        .map(|m| {
            let calls = m.tool_calls.as_ref().map(|c| c.to_string()).unwrap_or_default();
            text_tokens_upper_bound(&m.content) + text_tokens_upper_bound(&calls) + PER_MESSAGE_MARKERS
        })
        .sum()
}

/// The same bound for one piece of text, without a message's markers.
pub fn text_tokens_upper_bound(text: &str) -> u64 {
    let ascii = text.chars().filter(char::is_ascii).count() as u64;
    let other = text.chars().count() as u64 - ascii;
    ascii.div_ceil(4) + other
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
mod wire_shape_tests {
    use super::*;

    /// The multimodal-shaped default every current SDK sends: parts, flattened, one string.
    #[test]
    fn content_parts_flatten_to_one_string_joined_by_newlines() {
        let m: ChatMessage =
            serde_json::from_str(r#"{"role":"user","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#)
                .expect("parses");
        assert_eq!(m.content, "a\nb");
        let plain: ChatMessage = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).expect("parses");
        assert_eq!(plain.content, "hi");
    }

    /// A picture the model will never see must not become a question about a picture. Refused,
    /// and the refusal says which part — by index — and which kind.
    #[test]
    fn a_non_text_part_is_refused_by_name_with_its_index() {
        let raw: RawChatMessage = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"text","text":"what is this"},{"type":"image_url","image_url":{"url":"data:..."}}]}"#,
        )
        .expect("the wire form parses");
        let err = raw.into_message(Some(3)).expect_err("refused");
        assert!(err.starts_with("messages[3].content[1]"), "{err}");
        assert!(err.contains("`image_url`"), "{err}");
        assert!(err.contains("text only"), "{err}");

        let err = serde_json::from_str::<ChatMessage>(r#"{"role":"user","content":[{"type":"input_audio"}]}"#)
            .expect_err("refused through serde too");
        assert!(err.to_string().contains("content[0]") && err.to_string().contains("`input_audio`"), "{err}");
    }

    /// An assistant turn that only called a tool has `content: null`; that is an empty string
    /// here, and the calls ride along.
    #[test]
    fn null_content_is_empty_text_and_the_tool_fields_ride_along() {
        let m: ChatMessage = serde_json::from_str(
            r#"{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"f","arguments":"{}"}}]}"#,
        )
        .expect("parses");
        assert_eq!(m.content, "");
        assert_eq!(m.tool_calls.as_ref().and_then(|c| c[0]["id"].as_str()), Some("call_1"));
        let t: ChatMessage = serde_json::from_str(r#"{"role":"tool","tool_call_id":"call_1","content":"42"}"#).expect("parses");
        assert_eq!(t.tool_call_id.as_deref(), Some("call_1"));
    }

    /// What an engine receives: `content` as a plain string, and the optional fields only when
    /// they were there — an engine that predates them must see the message it always saw.
    #[test]
    fn a_message_serializes_its_content_as_a_plain_string_and_optionals_only_when_present() {
        let plain = serde_json::to_value(ChatMessage::new("user", "hi")).expect("json");
        assert_eq!(plain, serde_json::json!({"role":"user","content":"hi"}));
        let tool = ChatMessage { tool_call_id: Some("c1".into()), ..ChatMessage::new("tool", "42") };
        let v = serde_json::to_value(tool).expect("json");
        assert_eq!(v["tool_call_id"], "c1");
        assert!(v.get("name").is_none() && v.get("tool_calls").is_none());
    }

    /// The merge rule: the app's notice and the gateway's report become one object, objects
    /// merge key by key, and where they collide the gateway is describing what ran.
    #[test]
    fn merge_misaka_lets_the_gateway_win_on_conflict_and_keeps_the_rest() {
        let mut base = serde_json::json!({
            "ignored_fields": ["store"],
            "sampling": { "requested": { "temperature": 0.7 }, "reason": "the app's sentence" }
        });
        let gateway = serde_json::json!({
            "fp_claim_id": "d673",
            "sampling": { "reason": "the gateway's sentence", "enforced": "greedy" },
            "ignored_fields": ["user"]
        });
        merge_misaka(&mut base, &gateway);
        assert_eq!(base["fp_claim_id"], "d673", "the gateway's keys arrive");
        assert_eq!(base["sampling"]["requested"]["temperature"], 0.7, "the app's nested keys survive");
        assert_eq!(base["sampling"]["enforced"], "greedy");
        assert_eq!(base["sampling"]["reason"], "the gateway's sentence", "on conflict the gateway wins");
        assert_eq!(base["ignored_fields"], serde_json::json!(["user"]), "a non-object is replaced whole");
    }

    #[test]
    fn tool_calls_count_toward_the_prompt_estimate() {
        let plain = [ChatMessage::new("assistant", "")];
        let with_call = [ChatMessage {
            tool_calls: Some(serde_json::json!([{"id":"c","function":{"name":"lookup","arguments":"{\"city\":\"Tokyo\"}"}}])),
            ..ChatMessage::new("assistant", "")
        }];
        assert!(prompt_tokens_upper_bound(&with_call) > prompt_tokens_upper_bound(&plain));
    }
}

#[cfg(test)]
mod context_fit_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage::new(role, content)
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
