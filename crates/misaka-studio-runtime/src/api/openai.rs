//! The OpenAI-compatible surface.
//!
//! Compatibility here means a client written for `api.openai.com` works against
//! `http://127.0.0.1:1338/v1` with nothing changed but the base URL — same request fields, same
//! response envelope, same SSE framing down to the terminating `data: [DONE]`.
//!
//! # One surface, spelled once (ADR-0096 Decision 1)
//!
//! The request shape is a table, and this module is its Studio half: the fields in
//! [`CHAT_FIELDS`] are served; a field the table does not name is **refused by name**, never
//! dropped — a client that sent `logit_bias` and got an answer that ignored it has been told a
//! false thing about what ran. Two deliberate exceptions, both reported rather than silent: a knob
//! at its identity value (`frequency_penalty: 0`, `n: 1`, `logprobs: false`), which a stock SDK
//! sends by default and which asks for nothing, is accepted and listed under
//! `misaka.sampling.requested`; and the fields OpenAI defines as having no effect on the answer
//! (`user`, `metadata`, `store`, `parallel_tool_calls`, `max_completion_tokens` as an alias of
//! `max_tokens`) are accepted and listed in `misaka.ignored_fields`.
//!
//! The body is parsed by hand rather than by axum's `Json` extractor, because a refusal has to
//! reach the client as the 400 with OpenAI's `{"error": {...}}` shape that an SDK's error handling
//! already understands, not as axum's 422 plain-text rejection.
//!
//! # Just-in-time loading
//!
//! `"model": "Qwen3-4B-Q4_K_M"` on a request for a model that is not loaded loads it. Without
//! this, every client would need a Studio-specific "load first" call, which is exactly the
//! non-compatibility the endpoint exists to avoid. Loading is serialised by the backend and the
//! request waits for it, so the first call after a cold start is slow and correct rather than
//! fast and wrong.
//!
//! # Extra sampling fields
//!
//! `top_k`, `min_p` and `repeat_penalty` are not OpenAI fields; they are what local engines
//! actually expose, and leaving them out would make the Studio's own UI unable to use its own
//! API. They are additive — a client that never sends them gets the configured defaults.
//! `frequency_penalty` and `presence_penalty` are OpenAI's and are NOT mapped onto
//! `repeat_penalty`: they are different arithmetic (an additive penalty is not a multiplicative
//! one), the record commits to `repeat_penalty` alone, and the old alias turned a stock client's
//! `frequency_penalty: 0` into a repeat penalty of zero. At their identity value they are
//! accepted; at any other they are refused by name.

use crate::backend::{ChatMessage, RawChatMessage, StreamEvent, Usage};
use crate::state::{AppState, GenerateInputs};
use crate::{Error, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use misaka_studio_core::provenance::SamplingCommitment;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
}

/// The fields `/v1/chat/completions` serves — Decision 1's table, Studio column.
pub const CHAT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "repeat_penalty",
    "repetition_penalty",
    "frequency_penalty",
    "presence_penalty",
    "max_tokens",
    "max_completion_tokens",
    "seed",
    "stop",
    "n",
    "logprobs",
    "tools",
    "tool_choice",
    "response_format",
    "parallel_tool_calls",
    "user",
    "metadata",
    "store",
    "misaka",
];

/// The fields `/v1/completions` serves: the chat table without the chat, plus `prompt`.
pub const COMPLETION_FIELDS: &[&str] = &[
    "model",
    "prompt",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "repeat_penalty",
    "repetition_penalty",
    "frequency_penalty",
    "presence_penalty",
    "max_tokens",
    "max_completion_tokens",
    "seed",
    "stop",
    "n",
    "logprobs",
    "user",
    "metadata",
    "store",
    "misaka",
];

/// The roles a turn may carry. `tool` is Decision 2's; `developer` and the legacy `function`
/// are refused by name rather than mapped, because a role rename is the template's business and
/// the template is the model's (ADR-0077 Decision 6).
const ROLES: [&str; 4] = ["system", "user", "assistant", "tool"];

/// `stop` is a string in some clients and a list in others.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum StopField {
    One(String),
    Many(Vec<String>),
}

impl StopField {
    fn into_vec(self) -> Vec<String> {
        match self {
            StopField::One(s) => vec![s],
            StopField::Many(v) => v,
        }
    }
}

/// The sampling fields both endpoints share.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SamplingFields {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub min_p: Option<f64>,
    #[serde(alias = "repetition_penalty")]
    pub repeat_penalty: Option<f64>,
    pub max_tokens: Option<u64>,
    pub seed: Option<u64>,
    pub stop: Option<StopField>,
}

impl SamplingFields {
    /// Request values over configured defaults.
    fn resolve(self, defaults: SamplingCommitment) -> (SamplingCommitment, Vec<String>) {
        let params = SamplingCommitment {
            temperature: self.temperature.unwrap_or(defaults.temperature),
            top_p: self.top_p.unwrap_or(defaults.top_p),
            top_k: self.top_k.unwrap_or(defaults.top_k),
            min_p: self.min_p.unwrap_or(defaults.min_p),
            repeat_penalty: self.repeat_penalty.unwrap_or(defaults.repeat_penalty),
            max_tokens: self.max_tokens.unwrap_or(defaults.max_tokens),
            seed: self.seed.or(defaults.seed),
        };
        (params, self.stop.map(StopField::into_vec).unwrap_or_default())
    }
}

/// What serde reads of the fields both endpoints share, before the rules are applied.
#[derive(Debug, Deserialize)]
struct RawCommon {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stream_options: Option<Value>,
    #[serde(flatten)]
    sampling: SamplingFields,
    #[serde(default)]
    max_completion_tokens: Option<u64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    n: Option<Value>,
    #[serde(default)]
    logprobs: Option<Value>,
    #[serde(default)]
    user: Option<Value>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    store: Option<Value>,
    #[serde(default)]
    misaka: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawChatCompletionRequest {
    #[serde(flatten)]
    common: RawCommon,
    messages: Vec<RawChatMessage>,
    #[serde(default)]
    tools: Option<Value>,
    #[serde(default)]
    tool_choice: Option<Value>,
    #[serde(default)]
    response_format: Option<Value>,
    #[serde(default)]
    parallel_tool_calls: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawCompletionRequest {
    #[serde(flatten)]
    common: RawCommon,
    prompt: String,
}

/// The shared fields, checked.
#[derive(Debug, Default)]
struct Common {
    model: Option<String>,
    stream: bool,
    sampling: SamplingFields,
    misaka: Option<Value>,
    notices: Map<String, Value>,
}

/// A chat request, checked: every field either served, or refused before this exists.
#[derive(Debug)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub stream: bool,
    pub sampling: SamplingFields,
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<Value>,
    /// The `misaka` extension object, shape-checked, for the gateway backend.
    pub misaka: Option<Value>,
    /// The Studio's half of the answer's `misaka`: `ignored_fields`, and the identity-valued
    /// knobs under `sampling.requested`.
    pub notices: Map<String, Value>,
}

impl ChatCompletionRequest {
    pub fn parse(body: &[u8]) -> Result<Self> {
        let object = body_object(body, CHAT_FIELDS)?;
        let raw: RawChatCompletionRequest =
            serde_json::from_value(Value::Object(object)).map_err(|e| Error::bad_request(e.to_string()))?;
        let mut common = check_common(raw.common)?;
        if raw.messages.is_empty() {
            return Err(Error::bad_request("messages must not be empty"));
        }
        let mut messages = Vec::with_capacity(raw.messages.len());
        for (i, raw_message) in raw.messages.into_iter().enumerate() {
            let message = raw_message.into_message(Some(i)).map_err(Error::bad_request)?;
            check_message(i, &message)?;
            messages.push(message);
        }
        if let Some(tools) = present(&raw.tools) {
            check_tools(tools)?;
        }
        if let Some(choice) = present(&raw.tool_choice) {
            check_tool_choice(choice)?;
        }
        if let Some(format) = present(&raw.response_format) {
            check_response_format(format)?;
        }
        if present(&raw.parallel_tool_calls).is_some() {
            note_ignored(&mut common.notices, "parallel_tool_calls");
        }
        Ok(ChatCompletionRequest {
            model: common.model,
            messages,
            stream: common.stream,
            sampling: common.sampling,
            tools: present(&raw.tools).cloned(),
            tool_choice: present(&raw.tool_choice).cloned(),
            response_format: present(&raw.response_format).cloned(),
            misaka: common.misaka,
            notices: common.notices,
        })
    }
}

#[derive(Debug)]
pub struct CompletionRequest {
    pub model: Option<String>,
    pub prompt: String,
    pub stream: bool,
    pub sampling: SamplingFields,
    pub misaka: Option<Value>,
    pub notices: Map<String, Value>,
}

impl CompletionRequest {
    pub fn parse(body: &[u8]) -> Result<Self> {
        let object = body_object(body, COMPLETION_FIELDS)?;
        let raw: RawCompletionRequest =
            serde_json::from_value(Value::Object(object)).map_err(|e| Error::bad_request(e.to_string()))?;
        let common = check_common(raw.common)?;
        Ok(CompletionRequest {
            model: common.model,
            prompt: raw.prompt,
            stream: common.stream,
            sampling: common.sampling,
            misaka: common.misaka,
            notices: common.notices,
        })
    }
}

/// `Some` only for a value that was actually sent: `null` is what an SDK writes for "not set".
fn present(value: &Option<Value>) -> Option<&Value> {
    value.as_ref().filter(|v| !v.is_null())
}

fn note_ignored(notices: &mut Map<String, Value>, field: &str) {
    let list = notices.entry("ignored_fields").or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(items) = list {
        items.push(Value::String(field.to_string()));
    }
}

/// The body as an object, with every top-level key checked against the table first.
fn body_object(body: &[u8], accepted: &[&str]) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(body).map_err(|e| Error::bad_request(format!("the request body is not JSON: {e}")))?;
    let Value::Object(object) = value else {
        return Err(Error::bad_request("the request body must be a JSON object"));
    };
    for key in object.keys() {
        if accepted.contains(&key.as_str()) {
            continue;
        }
        return Err(Error::bad_request(named_refusal(key).unwrap_or_else(|| {
            format!(
                "`{key}` is not a field this surface serves (ADR-0096 Decision 1) — it is refused rather than dropped, \
                 so nothing about what ran is left unsaid. The fields served are: {}.",
                accepted.join(", ")
            )
        })));
    }
    Ok(object)
}

/// The fields refused with a sentence of their own, because the generic one would send a person
/// to the wrong fix.
fn named_refusal(field: &str) -> Option<String> {
    match field {
        "functions" | "function_call" => Some(format!(
            "`{field}` is OpenAI's legacy function-calling field and is not served; send `tools` and `tool_choice` \
             (ADR-0096 Decision 2)."
        )),
        "top_logprobs" => Some(
            "`top_logprobs` is not served: log-probabilities are not part of the answer — the lane commits to token ids, \
             not to distributions (ADR-0096 Decision 1)."
                .into(),
        ),
        _ => None,
    }
}

/// The rules over the fields both endpoints share.
fn check_common(raw: RawCommon) -> Result<Common> {
    let mut notices = Map::new();
    let mut sampling = raw.sampling;

    if let Some(options) = present(&raw.stream_options) {
        let Some(object) = options.as_object() else {
            return Err(Error::bad_request("`stream_options` must be an object"));
        };
        for (key, value) in object {
            if key != "include_usage" {
                return Err(Error::bad_request(format!(
                    "`stream_options.{key}` is not served; the only stream option is `include_usage` (ADR-0096 Decision 1)"
                )));
            }
            if !value.is_boolean() {
                return Err(Error::bad_request("`stream_options.include_usage` must be true or false"));
            }
        }
    }
    if let Some(n) = present(&raw.n)
        && n.as_u64() != Some(1)
    {
        return Err(Error::bad_request(format!("`n` must be 1: one inference is one claim (ADR-0077 R0); got {n}")));
    }
    match present(&raw.logprobs) {
        None | Some(Value::Bool(false)) => {}
        Some(other) => {
            return Err(Error::bad_request(format!(
                "`logprobs` must be false or absent; got {other}. Log-probabilities are not part of the answer — the lane \
                 commits to token ids, not to distributions (ADR-0096 Decision 1)."
            )));
        }
    }
    match (sampling.max_tokens, raw.max_completion_tokens) {
        (Some(a), Some(b)) if a != b => {
            return Err(Error::bad_request(format!(
                "`max_tokens` ({a}) and `max_completion_tokens` ({b}) disagree; send one of them, or the same value in both"
            )));
        }
        (None, Some(alias)) => {
            sampling.max_tokens = Some(alias);
            note_ignored(&mut notices, "max_completion_tokens");
        }
        (Some(_), Some(_)) => note_ignored(&mut notices, "max_completion_tokens"),
        (_, None) => {}
    }
    for (name, value) in [("frequency_penalty", raw.frequency_penalty), ("presence_penalty", raw.presence_penalty)] {
        let Some(value) = value else { continue };
        if value != 0.0 {
            return Err(Error::bad_request(format!(
                "`{name}` {value} is not served: the engine's repetition control is `repeat_penalty`, which the record \
                 commits to, and an additive penalty is not that. Send 0 or omit it (ADR-0096 Decision 1)."
            )));
        }
        // An identity value asks for nothing; it is listed with what was requested, not hidden.
        let sampling_notice = notices.entry("sampling").or_insert_with(|| json!({ "requested": {} }));
        sampling_notice["requested"][name] = json!(value);
    }
    for (name, value) in [("user", &raw.user), ("metadata", &raw.metadata), ("store", &raw.store)] {
        if present(value).is_some() {
            note_ignored(&mut notices, name);
        }
    }
    let misaka = match present(&raw.misaka) {
        Some(extension) => Some(check_misaka(extension)?.clone()),
        None => None,
    };
    Ok(Common { model: raw.model, stream: raw.stream, sampling, misaka, notices })
}

/// The `misaka` request extension: `{require_committed_format?: bool}` and nothing else.
fn check_misaka(extension: &Value) -> Result<&Value> {
    let Some(object) = extension.as_object() else {
        return Err(Error::bad_request("`misaka` must be an object: {\"require_committed_format\": true|false}"));
    };
    for (key, value) in object {
        match key.as_str() {
            "require_committed_format" if value.is_boolean() => {}
            "require_committed_format" => {
                return Err(Error::bad_request("`misaka.require_committed_format` must be true or false"));
            }
            other => {
                return Err(Error::bad_request(format!(
                    "`misaka.{other}` is not a request extension this surface serves; the one served is `require_committed_format` \
                     (ADR-0096 Decision 3)"
                )));
            }
        }
    }
    Ok(extension)
}

fn check_message(index: usize, message: &ChatMessage) -> Result<()> {
    if !ROLES.contains(&message.role.as_str()) {
        let hint = match message.role.as_str() {
            "function" => " (`function` is the legacy role; a tool's reply is role `tool` with its `tool_call_id`)",
            "developer" => {
                " (send the instruction as role `system`; a role rename is the template's, and the template is the model's)"
            }
            _ => "",
        };
        return Err(Error::bad_request(format!(
            "messages[{index}].role `{}` is not served; the roles are {}{hint}",
            message.role,
            ROLES.join(", ")
        )));
    }
    if message.role == "tool" && message.tool_call_id.as_deref().unwrap_or("").is_empty() {
        return Err(Error::bad_request(format!(
            "messages[{index}] is a `tool` turn without a `tool_call_id`; a tool's reply names the call it answers"
        )));
    }
    if let Some(calls) = &message.tool_calls
        && !calls.is_array()
    {
        return Err(Error::bad_request(format!("messages[{index}].tool_calls must be a list of calls in OpenAI's shape")));
    }
    Ok(())
}

fn check_tools(tools: &Value) -> Result<()> {
    let Some(list) = tools.as_array() else {
        return Err(Error::bad_request("`tools` must be a list of {type: \"function\", function: {name, parameters}}"));
    };
    for (i, tool) in list.iter().enumerate() {
        let is_function = tool.get("type").and_then(Value::as_str) == Some("function");
        let named = tool.get("function").and_then(|f| f.get("name")).and_then(Value::as_str).is_some_and(|n| !n.is_empty());
        if !is_function || !named {
            return Err(Error::bad_request(format!(
                "tools[{i}] must be {{type: \"function\", function: {{name, parameters}}}}; got {tool}"
            )));
        }
    }
    Ok(())
}

fn check_tool_choice(choice: &Value) -> Result<()> {
    let ok = match choice {
        Value::String(mode) => ["none", "auto", "required"].contains(&mode.as_str()),
        Value::Object(object) => {
            object.get("type").and_then(Value::as_str) == Some("function")
                && object.get("function").and_then(|f| f.get("name")).and_then(Value::as_str).is_some()
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(Error::bad_request(format!(
            "`tool_choice` must be \"none\", \"auto\", \"required\" or {{type: \"function\", function: {{name}}}}; got {choice}"
        )))
    }
}

/// Shape only (ADR-0096 Decision 3): the type, and a `json_schema` object when the type says
/// so. Whether the schema is honoured, and how, is the engine's to report.
fn check_response_format(format: &Value) -> Result<()> {
    let kind = format.get("type").and_then(Value::as_str);
    match kind {
        Some("text") | Some("json_object") => Ok(()),
        Some("json_schema") if format.get("json_schema").is_some_and(Value::is_object) => Ok(()),
        Some("json_schema") => Err(Error::bad_request(
            "`response_format.type` json_schema needs a `json_schema` object beside it: {type: \"json_schema\", json_schema: {name, schema}}",
        )),
        _ => Err(Error::bad_request(format!(
            "`response_format.type` must be text, json_object or json_schema (ADR-0096 Decision 3); got {format}"
        ))),
    }
}

#[derive(Serialize)]
struct ModelList {
    object: &'static str,
    data: Vec<ModelEntry>,
}

#[derive(Serialize)]
struct ModelEntry {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

async fn list_models(State(state): State<Arc<AppState>>) -> Result<Json<ModelList>> {
    let models = state.store.list().await;
    Ok(Json(ModelList {
        object: "list",
        data: models
            .iter()
            .map(|m| ModelEntry { id: m.id.clone(), object: "model", created: m.modified_at.unwrap_or(0), owned_by: "misaka-studio" })
            .collect(),
    }))
}

/// Make sure the requested model is the loaded one.
async fn ensure_loaded(state: &Arc<AppState>, requested: Option<String>) -> Result<String> {
    let current = state.loaded().await.map(|s| s.model.id);
    match (requested, current) {
        (Some(want), Some(have)) if want == have => Ok(have),
        (Some(want), _) => {
            state.load(&want, None).await?;
            Ok(want)
        }
        (None, Some(have)) => Ok(have),
        (None, None) => Err(Error::NoModelLoaded),
    }
}

async fn chat_completions(State(state): State<Arc<AppState>>, body: Bytes) -> Result<Response> {
    let request = ChatCompletionRequest::parse(&body)?;
    let model = ensure_loaded(&state, request.model.clone()).await?;
    let defaults = state.settings.read().await.generation.sampling();
    let (params, stop) = request.sampling.resolve(defaults);

    let stream = state
        .generate_with(GenerateInputs {
            messages: request.messages,
            prompt: None,
            params,
            stop,
            tools: request.tools,
            tool_choice: request.tool_choice,
            response_format: request.response_format,
            misaka: request.misaka,
            notices: request.notices,
        })
        .await?;
    Ok(if request.stream { sse_response(stream, model, true) } else { aggregate(stream, model, true).await? })
}

async fn completions(State(state): State<Arc<AppState>>, body: Bytes) -> Result<Response> {
    let request = CompletionRequest::parse(&body)?;
    let model = ensure_loaded(&state, request.model.clone()).await?;
    let defaults = state.settings.read().await.generation.sampling();
    let (params, stop) = request.sampling.resolve(defaults);

    let stream = state
        .generate_with(GenerateInputs {
            messages: Vec::new(),
            prompt: Some(request.prompt),
            params,
            stop,
            misaka: request.misaka,
            notices: request.notices,
            ..GenerateInputs::default()
        })
        .await?;
    Ok(if request.stream { sse_response(stream, model, false) } else { aggregate(stream, model, false).await? })
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn completion_id(chat: bool) -> String {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    if chat { format!("chatcmpl-{uuid}") } else { format!("cmpl-{uuid}") }
}

/// OpenAI's finish reason for an answer that called tools: `tool_calls` where the engine said
/// `stop`. A `length` stays `length` — a call cut short by the ceiling is a truncation, whatever
/// it was in the middle of.
fn finish_reason_with_tools(reason: String, any_tool_calls: bool) -> String {
    if any_tool_calls && reason == "stop" { "tool_calls".to_string() } else { reason }
}

/// **OpenAI's streamed `tool_calls[]` fragments, assembled by `index`.**
///
/// The first fragment of a call carries `id`, `type` and `function.name`; the ones after it carry
/// the same `index` and another piece of `function.arguments`. A whole call in one fragment (the
/// gateway's form) is the same thing with nothing after it. A fragment with no `index` starts a
/// new call; a call that never got an `id` is given `call_<n>`, because OpenAI's clients key the
/// tool round-trip on the id and a `null` there breaks the loop — the id is the protocol's, not
/// the answer's.
#[derive(Default)]
struct ToolCallAssembler {
    calls: Vec<(u64, Value)>,
}

impl ToolCallAssembler {
    fn push(&mut self, fragment: Value) {
        let index = fragment.get("index").and_then(Value::as_u64).unwrap_or(self.calls.len() as u64);
        let position = match self.calls.iter().position(|(i, _)| *i == index) {
            Some(p) => p,
            None => {
                self.calls.push((index, json!({ "id": null, "type": "function", "function": { "name": "", "arguments": "" } })));
                self.calls.len() - 1
            }
        };
        let call = &mut self.calls[position].1;
        if let Some(id) = fragment.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            call["id"] = json!(id);
        }
        if let Some(kind) = fragment.get("type").and_then(Value::as_str) {
            call["type"] = json!(kind);
        }
        if let Some(function) = fragment.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                call["function"]["name"] = json!(name);
            }
            if let Some(more) = function.get("arguments").and_then(Value::as_str) {
                let so_far = call["function"]["arguments"].as_str().unwrap_or_default().to_string();
                call["function"]["arguments"] = json!(so_far + more);
            }
        }
    }

    fn finish(mut self) -> Vec<Value> {
        self.calls.sort_by_key(|(i, _)| *i);
        self.calls
            .into_iter()
            .enumerate()
            .map(|(n, (_, mut call))| {
                if call["id"].is_null() {
                    call["id"] = json!(format!("call_{n}"));
                }
                call
            })
            .collect()
    }
}

/// Collect the whole generation into one response.
async fn aggregate(
    mut stream: futures_util::stream::BoxStream<'static, Result<StreamEvent>>,
    model: String,
    chat: bool,
) -> Result<Response> {
    let mut text = String::new();
    let mut calls = ToolCallAssembler::default();
    let mut usage = Usage::default();
    let mut finish_reason = "stop".to_string();
    let mut misaka = None;
    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Delta(delta) => text.push_str(&delta),
            StreamEvent::ToolCallDelta(fragment) => calls.push(fragment),
            StreamEvent::Done { usage: u, finish_reason: r, misaka: m } => {
                usage = u;
                finish_reason = r;
                misaka = m;
            }
        }
    }
    let tool_calls = calls.finish();
    let finish_reason = finish_reason_with_tools(finish_reason, !tool_calls.is_empty());

    let id = completion_id(chat);
    let choice = if chat {
        // OpenAI's `content` is null on a message that only called tools.
        let content = if text.is_empty() && !tool_calls.is_empty() { Value::Null } else { json!(text) };
        let mut message = json!({ "role": "assistant", "content": content });
        if !tool_calls.is_empty() {
            message["tool_calls"] = json!(tool_calls);
        }
        json!({ "index": 0, "message": message, "finish_reason": finish_reason })
    } else {
        json!({ "index": 0, "text": text, "finish_reason": finish_reason })
    };
    let mut body = json!({
        "id": id,
        "object": if chat { "chat.completion" } else { "text_completion" },
        "created": now(),
        "model": model,
        "choices": [choice],
        "usage": usage,
    });
    if let Some(misaka) = misaka {
        body["misaka"] = misaka;
    }
    Ok(Json(body).into_response())
}

/// Stream the generation as server-sent events, in OpenAI's chunk shape.
fn sse_response(stream: futures_util::stream::BoxStream<'static, Result<StreamEvent>>, model: String, chat: bool) -> Response {
    let id = completion_id(chat);
    let created = now();
    let object = if chat { "chat.completion.chunk" } else { "text_completion" };

    // The role-only opening chunk. OpenAI sends one and some clients rely on it to open the
    // assistant message before any text arrives.
    let opener = if chat {
        Some(json!({
            "id": id, "object": object, "created": created, "model": model,
            "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": null }]
        }))
    } else {
        None
    };

    let mut any_tool_calls = false;
    let mut fragments_seen: u64 = 0;
    let events = futures_util::stream::iter(opener.map(|v| Ok(Event::default().data(v.to_string()))))
        .chain(stream.map(move |event| {
            let json = match event {
                Ok(StreamEvent::Delta(delta)) => {
                    let choice = if chat {
                        json!({ "index": 0, "delta": { "content": delta }, "finish_reason": null })
                    } else {
                        json!({ "index": 0, "text": delta, "finish_reason": null })
                    };
                    json!({ "id": id, "object": object, "created": created, "model": model, "choices": [choice] })
                }
                Ok(StreamEvent::ToolCallDelta(mut fragment)) => {
                    // A raw completion has no tool surface; a call there is a comment line, which
                    // every SSE client ignores, rather than text the client would show as prose.
                    if !chat {
                        return Ok::<Event, Infallible>(Event::default().comment("a tool call has no place on /v1/completions"));
                    }
                    any_tool_calls = true;
                    // OpenAI's streamed fragments carry an `index`; a whole call without one is the
                    // next call.
                    if fragment.get("index").is_none() {
                        fragment["index"] = json!(fragments_seen);
                    }
                    fragments_seen += 1;
                    json!({
                        "id": id, "object": object, "created": created, "model": model,
                        "choices": [{ "index": 0, "delta": { "tool_calls": [fragment] }, "finish_reason": null }]
                    })
                }
                Ok(StreamEvent::Done { usage, finish_reason, misaka }) => {
                    let finish_reason = finish_reason_with_tools(finish_reason, any_tool_calls);
                    let choice = if chat {
                        json!({ "index": 0, "delta": {}, "finish_reason": finish_reason })
                    } else {
                        json!({ "index": 0, "text": "", "finish_reason": finish_reason })
                    };
                    let mut chunk = json!({
                        "id": id, "object": object, "created": created, "model": model,
                        "choices": [choice], "usage": usage
                    });
                    if let Some(misaka) = misaka {
                        chunk["misaka"] = misaka;
                    }
                    chunk
                }
                // An error mid-stream cannot change the status code — the 200 and the headers are
                // long gone. OpenAI's own answer is an error object in the stream, so that is
                // what a client sees here too.
                Err(e) => json!({ "error": { "message": e.to_string(), "type": e.openai_type() } }),
            };
            Ok::<Event, Infallible>(Event::default().data(json.to_string()))
        }))
        .chain(futures_util::stream::iter([Ok(Event::default().data("[DONE]"))]));

    Sse::new(events).keep_alive(KeepAlive::default()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Result<ChatCompletionRequest> {
        ChatCompletionRequest::parse(body.as_bytes())
    }

    fn refusal(body: &str) -> String {
        match parse(body) {
            Err(Error::BadRequest { message }) => message,
            Err(other) => panic!("refused, but not as a bad request: {other}"),
            Ok(request) => panic!("expected a refusal, got {request:?}"),
        }
    }

    #[test]
    fn request_values_override_defaults_and_absent_ones_do_not() {
        let defaults = SamplingCommitment { temperature: 0.7, top_k: 40, max_tokens: 2048, ..Default::default() };
        let fields = SamplingFields { temperature: Some(0.1), stop: Some(StopField::One("</s>".into())), ..Default::default() };
        let (params, stop) = fields.resolve(defaults);
        assert_eq!(params.temperature, 0.1, "the request wins");
        assert_eq!(params.top_k, 40, "an absent field keeps the default");
        assert_eq!(params.max_tokens, 2048);
        assert_eq!(stop, vec!["</s>".to_string()]);
    }

    #[test]
    fn stop_parses_as_either_a_string_or_a_list() {
        let one: SamplingFields = serde_json::from_str("{\"stop\":\"###\"}").expect("string form");
        assert_eq!(one.stop.expect("stop").into_vec(), vec!["###".to_string()]);
        let many: SamplingFields = serde_json::from_str(r#"{"stop":["a","b"]}"#).expect("list form");
        assert_eq!(many.stop.expect("stop").into_vec(), vec!["a".to_string(), "b".to_string()]);
    }

    /// A chat request from a stock OpenAI client must parse, extra local fields and all — and
    /// the identity values an SDK sends by default must not be refused.
    #[test]
    fn an_openai_shaped_request_parses() {
        let body = r#"{
            "model": "Qwen3-4B-Q4_K_M",
            "messages": [{"role":"system","content":"be brief"},{"role":"user","content":"hi"}],
            "stream": true, "temperature": 0.2, "max_tokens": 128, "top_k": 20, "seed": 7,
            "n": 1, "logprobs": false, "stream_options": {"include_usage": true}, "stop": null
        }"#;
        let request = parse(body).expect("parses");
        assert_eq!(request.messages.len(), 2);
        assert!(request.stream);
        assert_eq!(request.sampling.top_k, Some(20));
        assert_eq!(request.sampling.seed, Some(7));
        assert!(request.notices.is_empty(), "identity values that ask for nothing are not even a notice: {:?}", request.notices);
    }

    #[test]
    fn completion_ids_carry_the_expected_prefix() {
        assert!(completion_id(true).starts_with("chatcmpl-"));
        assert!(completion_id(false).starts_with("cmpl-"));
    }

    /// The multimodal-shaped default every SDK sends: parts, flattened. A part that is not text
    /// is refused, and the refusal names the message, the part and the kind.
    #[test]
    fn content_parts_are_flattened_and_a_non_text_part_is_refused_by_name() {
        let request = parse(r#"{"messages":[{"role":"user","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}"#)
            .expect("parses");
        assert_eq!(request.messages[0].content, "a\nb");

        let message = refusal(
            r#"{"messages":[{"role":"system","content":"x"},
                {"role":"user","content":[{"type":"text","text":"what is this"},{"type":"image_url","image_url":{"url":"data:..."}}]}]}"#,
        );
        assert!(message.starts_with("messages[1].content[1]"), "{message}");
        assert!(message.contains("`image_url`"), "{message}");
    }

    #[test]
    fn n_other_than_one_is_refused_and_n_one_is_not() {
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"n":2}"#);
        assert!(message.contains("`n` must be 1") && message.contains("one claim"), "{message}");
        parse(r#"{"messages":[{"role":"user","content":"hi"}],"n":1}"#).expect("n: 1 asks for nothing");
    }

    #[test]
    fn logprobs_and_top_logprobs_are_refused_by_name() {
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"logprobs":true}"#);
        assert!(message.contains("`logprobs`") && message.contains("token ids"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"top_logprobs":5}"#);
        assert!(message.contains("`top_logprobs`"), "{message}");
        parse(r#"{"messages":[{"role":"user","content":"hi"}],"logprobs":false}"#).expect("false is the default");
    }

    #[test]
    fn legacy_functions_are_refused_by_name_and_point_at_tools() {
        for body in [
            r#"{"messages":[{"role":"user","content":"hi"}],"functions":[]}"#,
            r#"{"messages":[{"role":"user","content":"hi"}],"function_call":"auto"}"#,
        ] {
            let message = refusal(body);
            assert!(message.contains("legacy") && message.contains("`tools`"), "{message}");
        }
    }

    /// The rule the table implies: a field it does not name is refused, never dropped.
    #[test]
    fn an_unknown_field_is_refused_by_name() {
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"logit_bias":{"50256":-100}}"#);
        assert!(message.starts_with("`logit_bias` is not a field this surface serves"), "{message}");
        assert!(message.contains("messages"), "the fields served are listed: {message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"reasoning_effort":"high"}"#);
        assert!(message.starts_with("`reasoning_effort`"), "{message}");
    }

    #[test]
    fn stream_options_accepts_only_include_usage() {
        parse(r#"{"messages":[{"role":"user","content":"hi"}],"stream_options":{"include_usage":true}}"#).expect("served");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"stream_options":{"include_obfuscation":true}}"#);
        assert!(message.contains("`stream_options.include_obfuscation`"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"stream_options":{"include_usage":"yes"}}"#);
        assert!(message.contains("true or false"), "{message}");
    }

    #[test]
    fn max_completion_tokens_is_an_alias_and_a_disagreement_is_refused() {
        let request = parse(r#"{"messages":[{"role":"user","content":"hi"}],"max_completion_tokens":64}"#).expect("the alias alone");
        assert_eq!(request.sampling.max_tokens, Some(64));
        assert_eq!(request.notices["ignored_fields"], json!(["max_completion_tokens"]));
        let request =
            parse(r#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":64,"max_completion_tokens":64}"#).expect("agreeing");
        assert_eq!(request.sampling.max_tokens, Some(64));
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":64,"max_completion_tokens":128}"#);
        assert!(message.contains("`max_tokens` (64)") && message.contains("`max_completion_tokens` (128)"), "{message}");
    }

    #[test]
    fn response_format_is_checked_for_shape_only() {
        for format in
            [r#"{"type":"text"}"#, r#"{"type":"json_object"}"#, r#"{"type":"json_schema","json_schema":{"name":"a","schema":{}}}"#]
        {
            let body = format!(r#"{{"messages":[{{"role":"user","content":"hi"}}],"response_format":{format}}}"#);
            let request = parse(&body).expect("served");
            assert_eq!(request.response_format.expect("kept"), serde_json::from_str::<Value>(format).expect("json"));
        }
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"response_format":{"type":"xml"}}"#);
        assert!(message.contains("`response_format.type`") && message.contains("json_schema"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"response_format":{"type":"json_schema"}}"#);
        assert!(message.contains("needs a `json_schema` object"), "{message}");
    }

    /// OpenAI's no-effect fields are accepted, and every one of them is named in the answer.
    #[test]
    fn no_effect_fields_are_accepted_and_listed_in_ignored_fields() {
        let request = parse(
            r#"{"messages":[{"role":"user","content":"hi"}],"user":"u1","metadata":{"a":1},"store":true,"parallel_tool_calls":false}"#,
        )
        .expect("served");
        assert_eq!(request.notices["ignored_fields"], json!(["user", "metadata", "store", "parallel_tool_calls"]));
        let request = parse(r#"{"messages":[{"role":"user","content":"hi"}],"user":null}"#).expect("null is not sent");
        assert!(request.notices.is_empty());
    }

    /// `frequency_penalty: 0` is what a stock client sends and asks for nothing: accepted and
    /// listed with what was requested. Any other value is refused, and the refusal names the knob
    /// the record does commit to — the old alias turned this 0 into a repeat penalty of zero.
    #[test]
    fn identity_penalties_are_accepted_and_others_refused() {
        let request =
            parse(r#"{"messages":[{"role":"user","content":"hi"}],"frequency_penalty":0,"presence_penalty":0.0}"#).expect("identity");
        assert_eq!(request.notices["sampling"]["requested"]["frequency_penalty"], 0.0);
        assert_eq!(request.notices["sampling"]["requested"]["presence_penalty"], 0.0);
        assert_eq!(request.sampling.repeat_penalty, None, "no longer an alias of repeat_penalty");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"frequency_penalty":0.5}"#);
        assert!(message.contains("`frequency_penalty` 0.5") && message.contains("`repeat_penalty`"), "{message}");
        let request =
            parse(r#"{"messages":[{"role":"user","content":"hi"}],"repetition_penalty":1.2}"#).expect("the llama.cpp spelling");
        assert_eq!(request.sampling.repeat_penalty, Some(1.2));
    }

    #[test]
    fn the_misaka_extension_is_shape_checked() {
        let request =
            parse(r#"{"messages":[{"role":"user","content":"hi"}],"misaka":{"require_committed_format":true}}"#).expect("served");
        assert_eq!(request.misaka, Some(json!({"require_committed_format": true})));
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"misaka":{"privacy":"panel"}}"#);
        assert!(message.contains("`misaka.privacy`"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"misaka":{"require_committed_format":"yes"}}"#);
        assert!(message.contains("true or false"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"misaka":true}"#);
        assert!(message.contains("must be an object"), "{message}");
    }

    #[test]
    fn a_tool_turn_needs_its_call_id_and_an_unknown_role_is_refused() {
        let request = parse(
            r#"{"messages":[{"role":"user","content":"weather?"},
                {"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"w","arguments":"{}"}}]},
                {"role":"tool","tool_call_id":"c1","content":"sunny"}]}"#,
        )
        .expect("a whole round-trip parses");
        assert_eq!(request.messages[2].tool_call_id.as_deref(), Some("c1"));
        let message = refusal(r#"{"messages":[{"role":"tool","content":"sunny"}]}"#);
        assert!(message.contains("messages[0]") && message.contains("`tool_call_id`"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"developer","content":"be brief"}]}"#);
        assert!(message.contains("`developer`") && message.contains("`system`"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"function","name":"w","content":"sunny"}]}"#);
        assert!(message.contains("legacy role"), "{message}");
    }

    #[test]
    fn tools_and_tool_choice_are_shape_checked() {
        let request = parse(
            r#"{"messages":[{"role":"user","content":"hi"}],
                "tools":[{"type":"function","function":{"name":"w","parameters":{"type":"object"}}}],
                "tool_choice":{"type":"function","function":{"name":"w"}}}"#,
        )
        .expect("served");
        assert_eq!(request.tools.expect("kept")[0]["function"]["name"], "w");
        assert_eq!(request.tool_choice.expect("kept")["function"]["name"], "w");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"tools":[{"type":"retrieval"}]}"#);
        assert!(message.starts_with("tools[0]"), "{message}");
        let message = refusal(r#"{"messages":[{"role":"user","content":"hi"}],"tool_choice":"sometimes"}"#);
        assert!(message.contains("`tool_choice`"), "{message}");
    }

    /// A refusal is a 400 in OpenAI's own error shape, so an SDK's error handling reads it.
    #[test]
    fn a_refusal_is_a_400_in_openais_shape() {
        let err = parse("not json").expect_err("refused");
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.openai_type(), "invalid_request_error");
        assert!(err.to_string().contains("not JSON"), "{err}");
        let err = parse(r#"{"messages":[]}"#).expect_err("refused");
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    async fn body_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf-8")
    }

    fn a_run_with_a_tool_call() -> Vec<Result<StreamEvent>> {
        vec![
            Ok(StreamEvent::ToolCallDelta(
                json!({"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"ci"}}),
            )),
            Ok(StreamEvent::ToolCallDelta(json!({"index":0,"function":{"arguments":"ty\":\"Tokyo\"}"}}))),
            Ok(StreamEvent::Done {
                usage: Usage { prompt_tokens: 20, completion_tokens: 9, total_tokens: 29 },
                finish_reason: "stop".into(),
                misaka: Some(json!({"fp_claim_id": "d673", "jobs": [{"role": "answer"}]})),
            }),
        ]
    }

    /// Non-streaming: the fragments become one call in `message.tool_calls`, the content is null
    /// as OpenAI's is, the finish reason says `tool_calls`, and `misaka` rides the response.
    #[tokio::test]
    async fn aggregate_assembles_tool_calls_and_says_so() {
        let stream = futures_util::stream::iter(a_run_with_a_tool_call()).boxed();
        let body = body_json(aggregate(stream, "m".into(), true).await.expect("aggregates")).await;
        let choice = &body["choices"][0];
        assert_eq!(choice["finish_reason"], "tool_calls");
        assert_eq!(choice["message"]["content"], Value::Null);
        let call = &choice["message"]["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "lookup");
        assert_eq!(call["function"]["arguments"], "{\"city\":\"Tokyo\"}", "the fragments concatenate");
        assert!(call.get("index").is_none(), "the assembled call is OpenAI's non-streaming shape");
        assert_eq!(body["misaka"]["fp_claim_id"], "d673");
        assert_eq!(body["usage"]["completion_tokens"], 9);

        // A plain answer: text, `stop`, no tool_calls key, no misaka key.
        let plain = futures_util::stream::iter(vec![
            Ok(StreamEvent::Delta("hi".into())),
            Ok(StreamEvent::Done { usage: Usage::default(), finish_reason: "stop".into(), misaka: None }),
        ])
        .boxed();
        let body = body_json(aggregate(plain, "m".into(), true).await.expect("aggregates")).await;
        assert_eq!(body["choices"][0]["message"]["content"], "hi");
        assert!(body["choices"][0]["message"].get("tool_calls").is_none());
        assert!(body.get("misaka").is_none());
    }

    /// A whole call with no `index` and no `id` — the gateway's form — is still a client-usable
    /// call, and a `length` finish stays `length` whatever was being called.
    #[test]
    fn whole_calls_without_index_or_id_are_still_calls() {
        let mut calls = ToolCallAssembler::default();
        calls.push(json!({"type":"function","function":{"name":"a","arguments":"{}"}}));
        calls.push(json!({"type":"function","function":{"name":"b","arguments":"{}"}}));
        let calls = calls.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_0");
        assert_eq!(calls[1]["function"]["name"], "b");
        assert_eq!(finish_reason_with_tools("length".into(), true), "length");
        assert_eq!(finish_reason_with_tools("stop".into(), true), "tool_calls");
        assert_eq!(finish_reason_with_tools("stop".into(), false), "stop");
    }

    /// Streaming: the fragments ride `delta.tool_calls` as they came, the final chunk carries the
    /// finish reason, the usage and `misaka`, and the stream still ends with `[DONE]`.
    #[tokio::test]
    async fn the_final_chunk_carries_misaka_and_tool_call_fragments_ride_the_delta() {
        let stream = futures_util::stream::iter(a_run_with_a_tool_call()).boxed();
        let text = body_text(sse_response(stream, "m".into(), true)).await;
        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .filter(|p| *p != "[DONE]")
            .map(|p| serde_json::from_str(p).expect("a json chunk"))
            .collect();
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant", "the opener");
        assert_eq!(chunks[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "lookup");
        assert_eq!(chunks[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "ty\":\"Tokyo\"}");
        let last = chunks.last().expect("a final chunk");
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["total_tokens"], 29);
        assert_eq!(last["misaka"]["jobs"][0]["role"], "answer");
        assert!(text.trim_end().ends_with("data: [DONE]"), "{text}");
    }
}
