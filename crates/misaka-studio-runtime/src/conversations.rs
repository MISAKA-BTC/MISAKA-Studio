//! Conversations are the runtime's — ADR-0096 Decision 12.
//!
//! One JSON file per conversation at `<data_dir>/conversations/<id>.json`, in the UI's own
//! `Conversation` shape (`ui/src/lib/types.ts`), so the window can move from `localStorage` to
//! this store without a migration: every field the UI persists is a field here, spelled the way
//! the UI spells it, and a field the UI persists that this file does not name rides through in
//! `extra` untouched. `messages[].mining` — the badge that says a prompt was mined behind the
//! chat — is kept, which is the reason the shape is the UI's and not a tidier one.
//!
//! # Why files, and why one per conversation
//!
//! The chat history is the one thing in the Studio that is the person's and lives nowhere else.
//! A file per conversation is something they can copy, `jq`, back up and delete with tools that
//! are not this app — which is the point of moving it out of the WebView. Every write is whole-file
//! and lands by rename (a temp file beside the target, then `rename`), so a crash mid-write leaves
//! the previous conversation on disk, never half of the new one. The provenance log
//! (`records.rs`) is the other shape on purpose: append-only, per completion, hashes first.
//!
//! # Import
//!
//! [`ConversationStore::import`] reads a body by its shape, in this order:
//!
//! 1. the Studio's own export (`schema == "misaka-studio/conversations/v1"`), and the window's
//!    cache — zustand's persist envelope under `misaka-studio.session` (`{state: {conversations}}`)
//!    or a bare list of Studio-shaped conversations — with every field kept;
//! 2. OpenAI's account export, `conversations.json`: each conversation's `mapping` tree is walked
//!    from `current_node` up its `parent` chain, so an abandoned branch (a regenerated answer)
//!    stays abandoned; messages whose role is not `system`/`user`/`assistant` or whose content is
//!    not `text` are skipped by name and counted;
//! 3. a generic list, `[{title?, messages: [{role, content}]}]`.
//!
//! Every imported message names where it came from (`source`, in `extra`), an id that collides
//! with a conversation already here is imported under a new id — an import never overwrites —
//! and the report says how many were imported and what was skipped, by reason.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The schema name an export carries, and the only one `import` reads as one.
pub const EXPORT_SCHEMA: &str = "misaka-studio/conversations/v1";

/// `source` values written into an imported message's `extra`.
pub const SOURCE_STUDIO_EXPORT: &str = "studio-export";
pub const SOURCE_STUDIO_CACHE: &str = "studio-cache";
pub const SOURCE_OPENAI_EXPORT: &str = "openai-export";
pub const SOURCE_GENERIC: &str = "generic";

/// A conversation, field for field what the UI persists (`Conversation` in `ui/src/lib/types.ts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    /// Milliseconds since the epoch — `Date.now()` on the UI's side.
    pub created_at: u64,
    pub updated_at: u64,
    /// The UI writes an explicit `null`, so this is serialised even when absent.
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Anything the UI persists that this struct does not name. Preserved, never interpreted.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One turn — the UI's `ChatMessage`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    /// `system`, `user` or `assistant` from the UI. A string rather than an enum so a role this
    /// version does not know is stored rather than refused; imports filter, the store does not.
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<TurnStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mining: Option<MessageMining>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// How a completed turn was produced — the UI's `TurnStats`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStats {
    pub tokens_per_second: f64,
    pub completion_tokens: u64,
    pub prompt_tokens: u64,
    /// `number | null` in the UI and written as `null` when unknown, so it is always serialised.
    #[serde(default)]
    pub time_to_first_token_ms: Option<f64>,
    pub model: String,
    pub finish_reason: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a user message knows about its own mining — the UI's `MessageMining`.
///
/// The three optional fields are `Option<Option<_>>` because the UI writes them two ways: absent
/// (a message just queued) and `null` (the queue's word folded back, with nothing to say yet).
/// Both are legal on the UI's side and both come back exactly as written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageMining {
    pub job_id: String,
    pub status: String,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub error: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable", skip_serializing_if = "Option::is_none")]
    pub answer: Option<Option<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `null` becomes `Some(None)`; an absent key stays `None` through `#[serde(default)]`.
fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// What a list shows: everything but the messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub model_id: Option<String>,
    pub message_count: usize,
}

impl ConversationSummary {
    pub fn of(conversation: &Conversation) -> Self {
        ConversationSummary {
            id: conversation.id.clone(),
            title: conversation.title.clone(),
            created_at: conversation.created_at,
            updated_at: conversation.updated_at,
            model_id: conversation.model_id.clone(),
            message_count: conversation.messages.len(),
        }
    }
}

/// Everything, in one file a person can keep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub schema: String,
    pub exported_at: u64,
    pub conversations: Vec<Conversation>,
}

/// What an import did, and what it declined to do, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: Vec<Skipped>,
    /// The ids the imported conversations were stored under, in the order they arrived.
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub reason: String,
    pub count: usize,
}

/// Skip reasons, aggregated in the order they were first met.
#[derive(Default)]
struct Skips(Vec<Skipped>);

impl Skips {
    fn add(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        match self.0.iter_mut().find(|s| s.reason == reason) {
            Some(entry) => entry.count += 1,
            None => self.0.push(Skipped { reason, count: 1 }),
        }
    }
}

/// A conversation id is a file name, so it is the characters that cannot leave the directory:
/// `[A-Za-z0-9_-]{1,64}`. Both the UI's ids (base-36) and this module's (a v4 UUID, simple form)
/// pass; a separator, a dot or an empty string does not.
pub fn validate_id(id: &str) -> Result<()> {
    let well_formed = !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if well_formed { Ok(()) } else { Err(Error::bad_request(format!("'{id}' is not a conversation id ([A-Za-z0-9_-]{{1,64}})"))) }
}

fn fresh_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// One file per conversation under a directory; see the module docs.
pub struct ConversationStore {
    dir: PathBuf,
    /// Serialises writers. Readers go straight to the files: because every write lands by rename,
    /// a reader racing a writer sees the old file or the new one, never a partial.
    write: Mutex<()>,
}

impl ConversationStore {
    /// The directory is created on the first write, not here: a Studio that never chats should
    /// not leave an empty directory behind.
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(ConversationStore { dir, write: Mutex::new(()) })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    /// Every conversation, newest activity first. Unreadable files are skipped with a warning
    /// rather than hiding the readable ones; `get` on such an id says what is wrong with it.
    pub async fn list(&self) -> Result<Vec<ConversationSummary>> {
        Ok(self.load_all().await?.iter().map(ConversationSummary::of).collect())
    }

    pub async fn get(&self, id: &str) -> Result<Option<Conversation>> {
        read_conversation(&self.path_for(id)?).await
    }

    /// Upsert under the conversation's own id.
    pub async fn put(&self, conversation: &Conversation) -> Result<()> {
        self.put_as(&conversation.id, conversation).await
    }

    /// Upsert under `id`, refusing a conversation that names a different one — the guard behind
    /// `PUT /conversations/{id}`, kept here so no caller can store a file whose name and content
    /// disagree.
    pub async fn put_as(&self, id: &str, conversation: &Conversation) -> Result<()> {
        if conversation.id != id {
            return Err(Error::bad_request(format!("the conversation's id '{}' does not match '{id}'", conversation.id)));
        }
        let path = self.path_for(id)?;
        let bytes = serde_json::to_vec_pretty(conversation).map_err(|e| Error::io(path.display(), std::io::Error::other(e)))?;
        let _writer = self.write.lock().await;
        tokio::fs::create_dir_all(&self.dir).await.map_err(|e| Error::io(self.dir.display(), e))?;
        write_atomically(&path, &bytes).await
    }

    /// `Ok(false)` when there was nothing to delete.
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let path = self.path_for(id)?;
        let _writer = self.write.lock().await;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(path.display(), e)),
        }
    }

    pub async fn exists(&self, id: &str) -> Result<bool> {
        Ok(tokio::fs::try_exists(self.path_for(id)?).await.unwrap_or(false))
    }

    pub async fn export_all(&self) -> Result<Export> {
        Ok(Export { schema: EXPORT_SCHEMA.to_string(), exported_at: now_ms(), conversations: self.load_all().await? })
    }

    /// Import a body by its shape (see the module docs). Never overwrites: a conversation whose
    /// id is taken — or is not a valid id — is stored under a fresh one.
    pub async fn import(&self, body: Value) -> Result<ImportReport> {
        let now = now_ms();
        let mut skipped = Skips::default();
        let batch: Vec<Conversation> = match detect(&body)? {
            Shape::Studio { conversations, source } => {
                conversations.into_iter().filter_map(|v| studio_conversation(v, source, &mut skipped)).collect()
            }
            Shape::OpenAi(items) => items.iter().filter_map(|v| openai_conversation(v, now, &mut skipped)).collect(),
            Shape::Generic(items) => items.iter().filter_map(|v| generic_conversation(v, now, &mut skipped)).collect(),
        };

        let mut report = ImportReport { skipped: skipped.0, ..Default::default() };
        for mut conversation in batch {
            if validate_id(&conversation.id).is_err() || self.exists(&conversation.id).await? {
                conversation.id = fresh_id();
            }
            self.put(&conversation).await?;
            report.ids.push(conversation.id);
        }
        report.imported = report.ids.len();
        Ok(report)
    }

    /// Every readable conversation, sorted newest activity first (ties by id, so the order is a
    /// function of the files and not of the directory).
    async fn load_all(&self) -> Result<Vec<Conversation>> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(self.dir.display(), e)),
        };
        let mut out = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| Error::io(self.dir.display(), e))? {
            let path = entry.path();
            let Some(id) = conversation_id_of(&path) else { continue };
            match read_conversation(&path).await {
                Ok(Some(conversation)) if conversation.id == id => out.push(conversation),
                Ok(Some(conversation)) => {
                    tracing::warn!("skipping {}: it holds conversation '{}' under the name '{id}'", path.display(), conversation.id)
                }
                // Deleted between the listing and the read.
                Ok(None) => {}
                Err(e) => tracing::warn!("skipping an unreadable conversation file: {e}"),
            }
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }
}

/// The id a file in the store directory stands for, or `None` for anything that is not a
/// conversation file — a temp file mid-write, a stray, a name no id could have produced.
fn conversation_id_of(path: &Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    validate_id(stem).ok().map(|()| stem.to_string())
}

async fn read_conversation(path: &Path) -> Result<Option<Conversation>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path.display(), e)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| Error::io(path.display(), std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

/// Write beside the target, then rename over it. Whichever step fails, the temp file is removed:
/// a `.json.tmp` that outlives its write is a file the next listing has to explain.
async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    if let Err(e) = tokio::fs::write(&temp, bytes).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::io(temp.display(), e));
    }
    if let Err(e) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(Error::io(path.display(), e));
    }
    Ok(())
}

// --- import ---------------------------------------------------------------------------------

enum Shape {
    Studio { conversations: Vec<Value>, source: &'static str },
    OpenAi(Vec<Value>),
    Generic(Vec<Value>),
}

fn is_studio_conversation(value: &Value) -> bool {
    Conversation::deserialize(value).is_ok()
}

fn is_openai_conversation(value: &Value) -> bool {
    value.get("mapping").is_some_and(Value::is_object)
}

fn detect(body: &Value) -> Result<Shape> {
    match body {
        Value::Object(map) => {
            if let Some(schema) = map.get("schema").and_then(Value::as_str) {
                if schema != EXPORT_SCHEMA {
                    return Err(Error::bad_request(format!("unsupported schema '{schema}' — this Studio reads '{EXPORT_SCHEMA}'")));
                }
                let conversations = map
                    .get("conversations")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| Error::bad_request("a Studio export carries a 'conversations' list"))?;
                return Ok(Shape::Studio { conversations, source: SOURCE_STUDIO_EXPORT });
            }
            // The window's own cache: zustand's persist envelope, `{state: {conversations}, version}`.
            if let Some(conversations) = map.get("state").and_then(|s| s.get("conversations")).and_then(Value::as_array) {
                return Ok(Shape::Studio { conversations: conversations.clone(), source: SOURCE_STUDIO_CACHE });
            }
            if is_openai_conversation(body) {
                return Ok(Shape::OpenAi(vec![body.clone()]));
            }
            if is_studio_conversation(body) {
                return Ok(Shape::Studio { conversations: vec![body.clone()], source: SOURCE_STUDIO_CACHE });
            }
            if map.get("messages").is_some_and(Value::is_array) {
                return Ok(Shape::Generic(vec![body.clone()]));
            }
        }
        Value::Array(items) => {
            if items.iter().any(is_openai_conversation) {
                return Ok(Shape::OpenAi(items.clone()));
            }
            // A bare list of the UI's own conversations is Studio-shaped, and reading it as the
            // generic list would silently drop `mining`, `stats` and every id.
            if !items.is_empty() && items.iter().all(is_studio_conversation) {
                return Ok(Shape::Studio { conversations: items.clone(), source: SOURCE_STUDIO_CACHE });
            }
            return Ok(Shape::Generic(items.clone()));
        }
        _ => {}
    }
    Err(Error::bad_request(format!(
        "unrecognised import: expected a Studio export ({{\"schema\": \"{EXPORT_SCHEMA}\", \"conversations\": [...]}}), \
         OpenAI's conversations.json (a list of conversations with a 'mapping' tree), \
         or a list of {{title?, messages: [{{role, content}}]}}"
    )))
}

fn is_chat_role(role: &str) -> bool {
    matches!(role, "system" | "user" | "assistant")
}

fn role_reason(role: &str) -> String {
    format!("role {}", if role.is_empty() { "<none>" } else { role })
}

fn imported_message(role: &str, content: String, source: &'static str) -> Message {
    let mut extra = Map::new();
    extra.insert("source".to_string(), Value::String(source.to_string()));
    Message { id: fresh_id(), role: role.to_string(), content, streaming: None, error: None, stats: None, mining: None, extra }
}

/// The UI's rule for a title: the first user turn, whitespace collapsed, cut at 48 characters.
fn derive_title(messages: &[Message]) -> String {
    let first = messages.iter().find(|m| m.role == "user").or_else(|| messages.iter().find(|m| m.role != "system"));
    let Some(message) = first else { return "Imported chat".to_string() };
    let clean = message.content.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return "Imported chat".to_string();
    }
    match clean.char_indices().nth(48) {
        Some((cut, _)) => format!("{}…", &clean[..cut]),
        None => clean,
    }
}

fn seconds_to_ms(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_f64).filter(|s| s.is_finite() && *s >= 0.0).map(|s| (s * 1000.0).round() as u64)
}

fn title_of(value: &Value, messages: &[Message]) -> String {
    value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .unwrap_or_else(|| derive_title(messages))
}

/// A Studio-shaped conversation: kept whole. Messages keep their ids (the mining queue names
/// them) and gain a `source` only when they have none, so a re-import of an export that itself
/// came from OpenAI still says so.
fn studio_conversation(value: Value, source: &'static str, skipped: &mut Skips) -> Option<Conversation> {
    let mut conversation: Conversation = match serde_json::from_value(value) {
        Ok(conversation) => conversation,
        Err(e) => {
            tracing::debug!("skipping an unreadable conversation in an import: {e}");
            skipped.add("unreadable conversation");
            return None;
        }
    };
    for message in &mut conversation.messages {
        if message.id.is_empty() {
            message.id = fresh_id();
        }
        message.extra.entry("source".to_string()).or_insert_with(|| Value::String(source.to_string()));
    }
    Some(conversation)
}

/// One conversation of OpenAI's `conversations.json`.
fn openai_conversation(value: &Value, now: u64, skipped: &mut Skips) -> Option<Conversation> {
    let Some(mapping) = value.get("mapping").and_then(Value::as_object) else {
        skipped.add("unreadable conversation");
        return None;
    };
    let mut messages = Vec::new();
    for node_id in openai_chain(mapping, value.get("current_node").and_then(Value::as_str)) {
        // A node without a message is structure (the root), not a turn: nothing to count.
        let Some(message) = mapping.get(&node_id).and_then(|n| n.get("message")).filter(|m| !m.is_null()) else { continue };
        if let Some(message) = openai_message(message, skipped) {
            messages.push(message);
        }
    }
    if messages.is_empty() {
        skipped.add("conversation with no messages");
        return None;
    }
    let created_at = seconds_to_ms(value.get("create_time")).unwrap_or(now);
    let updated_at = seconds_to_ms(value.get("update_time")).unwrap_or(created_at);
    let title = title_of(value, &messages);
    Some(Conversation { id: fresh_id(), title, created_at, updated_at, model_id: None, messages, extra: Map::new() })
}

/// The node ids from the root to the conversation's current leaf, in reading order.
///
/// `current_node` is what the person last saw; walking its `parent` chain is what leaves a
/// regenerated answer's abandoned sibling out. Without a usable `current_node` the walk starts at
/// the root and follows each node's last child — the branch the app itself shows by default.
fn openai_chain(mapping: &Map<String, Value>, current: Option<&str>) -> Vec<String> {
    let mut at = current.filter(|id| mapping.contains_key(*id)).map(String::from).or_else(|| openai_default_leaf(mapping));
    let mut chain = Vec::new();
    while let Some(id) = at {
        // A parent chain longer than the map is a cycle, and a cycle is not a conversation.
        if chain.len() > mapping.len() {
            break;
        }
        at = mapping.get(&id).and_then(|n| n.get("parent")).and_then(Value::as_str).map(String::from);
        chain.push(id);
    }
    chain.reverse();
    chain
}

fn openai_default_leaf(mapping: &Map<String, Value>) -> Option<String> {
    let (root, _) = mapping.iter().find(|(_, node)| node.get("parent").is_none_or(Value::is_null))?;
    let mut at = root.clone();
    for _ in 0..=mapping.len() {
        let Some(child) = mapping.get(&at).and_then(|n| n.get("children")).and_then(Value::as_array).and_then(|c| c.last()) else {
            break;
        };
        match child.as_str() {
            Some(child) if mapping.contains_key(child) => at = child.to_string(),
            _ => break,
        }
    }
    Some(at)
}

fn openai_message(message: &Value, skipped: &mut Skips) -> Option<Message> {
    let role = message.get("author").and_then(|a| a.get("role")).and_then(Value::as_str).unwrap_or("");
    if !is_chat_role(role) {
        skipped.add(role_reason(role));
        return None;
    }
    let content = message.get("content");
    let content_type = content.and_then(|c| c.get("content_type")).and_then(Value::as_str).unwrap_or("<none>");
    if content_type != "text" {
        skipped.add(format!("non-text content_type {content_type}"));
        return None;
    }
    let mut text = String::new();
    for part in content.and_then(|c| c.get("parts")).and_then(Value::as_array).into_iter().flatten() {
        match part.as_str() {
            Some(part) => text.push_str(part),
            None => skipped.add("non-text part"),
        }
    }
    if text.trim().is_empty() {
        skipped.add("empty message");
        return None;
    }
    Some(imported_message(role, text, SOURCE_OPENAI_EXPORT))
}

/// One conversation of the generic list.
fn generic_conversation(value: &Value, now: u64, skipped: &mut Skips) -> Option<Conversation> {
    let Some(items) = value.get("messages").and_then(Value::as_array) else {
        skipped.add("unreadable conversation");
        return None;
    };
    let messages: Vec<Message> = items.iter().filter_map(|m| generic_message(m, skipped)).collect();
    if messages.is_empty() {
        skipped.add("conversation with no messages");
        return None;
    }
    let created_at = value.get("createdAt").and_then(Value::as_u64).unwrap_or(now);
    let updated_at = value.get("updatedAt").and_then(Value::as_u64).unwrap_or(created_at);
    let title = title_of(value, &messages);
    let model_id = value.get("modelId").and_then(Value::as_str).map(String::from);
    Some(Conversation { id: fresh_id(), title, created_at, updated_at, model_id, messages, extra: Map::new() })
}

/// `{role, content}`, where `content` is a string or — as the chat API spells a multimodal turn —
/// a list of parts of which the `text` ones are kept.
fn generic_message(value: &Value, skipped: &mut Skips) -> Option<Message> {
    let role = value.get("role").and_then(Value::as_str).unwrap_or("");
    if !is_chat_role(role) {
        skipped.add(role_reason(role));
        return None;
    }
    let text = match value.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    Value::String(part) => text.push_str(part),
                    Value::Object(part) if part.get("type").and_then(Value::as_str) == Some("text") => {
                        text.push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                    _ => skipped.add("non-text part"),
                }
            }
            text
        }
        None | Some(Value::Null) => String::new(),
        Some(_) => {
            skipped.add("non-text content");
            return None;
        }
    };
    if text.trim().is_empty() {
        skipped.add("empty message");
        return None;
    }
    Some(imported_message(role, text, SOURCE_GENERIC))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(id: &str, role: &str, content: &str) -> Message {
        Message {
            id: id.into(),
            role: role.into(),
            content: content.into(),
            streaming: None,
            error: None,
            stats: None,
            mining: None,
            extra: Map::new(),
        }
    }

    fn conversation(id: &str, updated_at: u64) -> Conversation {
        Conversation {
            id: id.into(),
            title: format!("chat {id}"),
            created_at: 1_700_000_000_000,
            updated_at,
            model_id: Some("qwen2.5-1.5b-a16".into()),
            messages: vec![message("m1", "user", "hello"), message("m2", "assistant", "hi")],
            extra: Map::new(),
        }
    }

    fn store() -> (Arc<ConversationStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (ConversationStore::new(dir.path().join("conversations")), dir)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = match std::fs::read_dir(dir) {
            Ok(entries) => entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    fn skipped(report: &ImportReport, reason: &str) -> usize {
        report.skipped.iter().find(|s| s.reason == reason).map(|s| s.count).unwrap_or(0)
    }

    /// Numbers by value: JSON has one zero, and JS spells it `0` where serde spells an f64 `0.0`.
    fn canonical(value: Value) -> Value {
        match value {
            Value::Number(n) => json!(n.as_f64().expect("finite")),
            Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
            Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, canonical(v))).collect()),
            other => other,
        }
    }

    /// What the UI persists for a conversation today, verbatim — the mining badge with an explicit
    /// `null`, a queued badge with nothing but its job, a finished turn's stats, a failed one's error.
    fn ui_persisted_conversation() -> Value {
        json!({
            "id": "k3j5h2f9lmn0abc",
            "title": "Explain the a16 class",
            "createdAt": 1757500000000u64,
            "updatedAt": 1757500123456u64,
            "modelId": "qwen2.5-1.5b-a16",
            "messages": [
                {
                    "id": "m1", "role": "user", "content": "Explain the a16 class",
                    "mining": { "jobId": "job-1", "status": "committed", "claimId": "594abbb7", "error": null, "answer": "The a16 class is…" }
                },
                {
                    "id": "m2", "role": "assistant", "content": "The a16 class is…", "streaming": false,
                    "stats": { "tokensPerSecond": 8.5, "completionTokens": 42, "promptTokens": 12, "timeToFirstTokenMs": 310.25,
                               "model": "qwen2.5-1.5b-a16", "finishReason": "stop" }
                },
                { "id": "m3", "role": "user", "content": "again", "mining": { "jobId": "job-2", "status": "queued" } },
                {
                    "id": "m4", "role": "assistant", "content": "", "streaming": false, "error": "engine died",
                    "stats": { "tokensPerSecond": 0, "completionTokens": 0, "promptTokens": 12, "timeToFirstTokenMs": null,
                               "model": "qwen2.5-1.5b-a16", "finishReason": "error" }
                }
            ]
        })
    }

    #[tokio::test]
    async fn put_get_list_delete_round_trip() {
        let (store, _dir) = store();
        assert!(store.list().await.expect("list").is_empty(), "an empty store lists nothing, without a directory");

        let older = conversation("older", 1_700_000_001_000);
        let newer = conversation("newer", 1_700_000_002_000);
        store.put(&older).await.expect("put");
        store.put(&newer).await.expect("put");

        assert_eq!(store.get("older").await.expect("get"), Some(older.clone()));
        let listed = store.list().await.expect("list");
        assert_eq!(listed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["newer", "older"], "newest activity first");
        assert_eq!(listed[0].message_count, 2);
        assert_eq!(listed[0].title, "chat newer");
        assert_eq!(listed[0].model_id.as_deref(), Some("qwen2.5-1.5b-a16"));

        // Upsert: the same id again replaces, it does not duplicate.
        let mut edited = older.clone();
        edited.title = "renamed".into();
        edited.updated_at = 1_700_000_003_000;
        store.put(&edited).await.expect("put");
        assert_eq!(store.list().await.expect("list").len(), 2);
        assert_eq!(store.get("older").await.expect("get").map(|c| c.title), Some("renamed".into()));

        assert!(store.delete("older").await.expect("delete"));
        assert!(!store.delete("older").await.expect("delete"), "a second delete finds nothing");
        assert_eq!(store.get("older").await.expect("get"), None);
        assert_eq!(store.list().await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn a_write_leaves_only_the_conversation_behind() {
        let (store, _dir) = store();
        store.put(&conversation("abc", 1)).await.expect("put");
        assert_eq!(files_in(store.dir()), ["abc.json"], "no temp file survives a write");

        // A write that cannot land: the target name is taken by a directory, so the rename fails.
        std::fs::create_dir_all(store.dir().join("blocked.json")).expect("mkdir");
        let err = store.put(&conversation("blocked", 1)).await.expect_err("the rename cannot succeed");
        assert!(matches!(err, Error::Io { .. }), "got {err}");
        assert_eq!(files_in(store.dir()), ["abc.json", "blocked.json"], "the failed write's temp file was removed");
    }

    #[tokio::test]
    async fn ids_that_could_leave_the_directory_are_refused() {
        let (store, _dir) = store();
        for bad in ["", "../x", "a/b", "a\\b", "a.json", "a b", "ü", ".", "..", &"x".repeat(65)] {
            assert!(validate_id(bad).is_err(), "{bad:?} must be refused");
            let mut conversation = conversation("ok", 1);
            conversation.id = bad.to_string();
            assert!(store.put(&conversation).await.is_err(), "put {bad:?}");
            assert!(store.get(bad).await.is_err(), "get {bad:?}");
            assert!(store.delete(bad).await.is_err(), "delete {bad:?}");
        }
        for good in ["a", "k3j5h2f9lmn0abc", "3f2a-9b_c", &"x".repeat(64), &fresh_id()] {
            assert!(validate_id(good).is_ok(), "{good:?} is an id");
        }
        assert!(!store.dir().exists(), "nothing was written by a refused id");
    }

    #[tokio::test]
    async fn put_refuses_an_id_mismatch() {
        let (store, _dir) = store();
        let err = store.put_as("path-id", &conversation("body-id", 1)).await.expect_err("mismatch");
        assert!(err.to_string().contains("body-id") && err.to_string().contains("path-id"), "names both: {err}");
        assert_eq!(store.get("path-id").await.expect("get"), None);
        assert_eq!(store.get("body-id").await.expect("get"), None);
    }

    /// The migration-free claim, asserted on the UI's own JSON: what goes in is what comes out,
    /// key for key and null for null — the queued badge stays without a `claimId`, the folded one
    /// keeps its explicit `null`, and the stored file is the same document. Numbers are compared
    /// by value: JSON has one zero, and JS spells it `0` where serde spells an f64 `0.0`.
    #[tokio::test]
    async fn the_ui_shape_survives_field_for_field() {
        let original = ui_persisted_conversation();
        let parsed: Conversation = serde_json::from_value(original.clone()).expect("the UI's shape parses");
        assert_eq!(canonical(serde_json::to_value(&parsed).expect("serialise")), canonical(original.clone()));

        let queued = parsed.messages[2].mining.as_ref().expect("mining");
        assert_eq!(queued.claim_id, None, "absent stays absent");
        let folded = parsed.messages[0].mining.as_ref().expect("mining");
        assert_eq!(folded.error, Some(None), "null stays null");
        assert_eq!(folded.claim_id, Some(Some("594abbb7".into())));

        let (store, _dir) = store();
        store.put(&parsed).await.expect("put");
        let on_disk: Value =
            serde_json::from_slice(&std::fs::read(store.dir().join("k3j5h2f9lmn0abc.json")).expect("read")).expect("json");
        assert_eq!(canonical(on_disk), canonical(original));
    }

    #[tokio::test]
    async fn unknown_fields_survive_a_round_trip() {
        let mut original = ui_persisted_conversation();
        original["pinned"] = json!(true);
        original["messages"][0]["reactions"] = json!(["+1"]);
        original["messages"][0]["mining"]["lane"] = json!("fp");
        original["messages"][1]["stats"]["seed"] = json!(7);

        let (store, _dir) = store();
        let parsed: Conversation = serde_json::from_value(original.clone()).expect("parses");
        assert_eq!(parsed.extra.get("pinned"), Some(&json!(true)));
        store.put(&parsed).await.expect("put");
        let back = store.get("k3j5h2f9lmn0abc").await.expect("get").expect("stored");
        assert_eq!(canonical(serde_json::to_value(&back).expect("serialise")), canonical(original));
    }

    #[tokio::test]
    async fn an_unreadable_file_is_skipped_by_list_and_named_by_get() {
        let (store, _dir) = store();
        store.put(&conversation("good", 1)).await.expect("put");
        std::fs::write(store.dir().join("bad.json"), b"{ not json").expect("write");
        std::fs::write(store.dir().join("stray.txt"), b"nothing").expect("write");
        std::fs::write(store.dir().join("leftover.json.tmp"), b"{}").expect("write");

        let listed = store.list().await.expect("list");
        assert_eq!(listed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["good"]);
        let err = store.get("bad").await.expect_err("an unreadable file is an error on direct access");
        assert!(err.to_string().contains("bad.json"), "names the file: {err}");
    }

    #[tokio::test]
    async fn export_then_import_is_the_identity_up_to_source() {
        let (a, _dir_a) = store();
        a.put(&serde_json::from_value(ui_persisted_conversation()).expect("parses")).await.expect("put");
        a.put(&conversation("second", 1_700_000_009_000)).await.expect("put");
        let export = a.export_all().await.expect("export");
        assert_eq!(export.schema, EXPORT_SCHEMA);
        assert_eq!(export.conversations.len(), 2);

        // A JSON round trip, as a file on disk would be.
        let body: Value = serde_json::from_str(&serde_json::to_string(&export).expect("json")).expect("json");
        let (b, _dir_b) = store();
        let report = b.import(body.clone()).await.expect("import");
        assert_eq!(report.imported, 2);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(report.ids, vec!["k3j5h2f9lmn0abc".to_string(), "second".to_string()], "ids are kept when free, newest first");

        let mut reimported = b.export_all().await.expect("export").conversations;
        for conversation in &mut reimported {
            for message in &mut conversation.messages {
                assert_eq!(message.extra.remove("source"), Some(json!(SOURCE_STUDIO_EXPORT)));
            }
        }
        assert_eq!(reimported, export.conversations);

        // Into the store that already holds them: new ids, and the originals untouched.
        let again = a.import(body).await.expect("import");
        assert_eq!(again.imported, 2);
        assert!(again.ids.iter().all(|id| id != "second" && id != "k3j5h2f9lmn0abc"), "{:?}", again.ids);
        assert_eq!(a.list().await.expect("list").len(), 4);
        assert_eq!(a.get("second").await.expect("get"), Some(conversation("second", 1_700_000_009_000)));
    }

    /// OpenAI's `conversations.json`, two conversations: one whose user turn was answered twice
    /// (the first answer abandoned by a regenerate — the walk from `current_node` must leave it
    /// out) and one with an image turn and a `tool` node, both skipped by name.
    fn openai_export() -> Value {
        json!([
            {
                "title": "Hello there",
                "create_time": 1700000000.5,
                "update_time": 1700000100,
                "current_node": "a1",
                "mapping": {
                    "root": { "id": "root", "message": null, "parent": null, "children": ["sys"] },
                    "sys": {
                        "id": "sys", "parent": "root", "children": ["u1"],
                        "message": { "id": "sys", "author": { "role": "system" }, "create_time": null,
                                     "content": { "content_type": "text", "parts": [""] } }
                    },
                    "u1": {
                        "id": "u1", "parent": "sys", "children": ["a1-old", "a1"],
                        "message": { "id": "u1", "author": { "role": "user" }, "create_time": 1700000001.0,
                                     "content": { "content_type": "text", "parts": ["Hello there, what is 2+2?"] } }
                    },
                    "a1-old": {
                        "id": "a1-old", "parent": "u1", "children": [],
                        "message": { "id": "a1-old", "author": { "role": "assistant" }, "create_time": 1700000002.0,
                                     "content": { "content_type": "text", "parts": ["5"] } }
                    },
                    "a1": {
                        "id": "a1", "parent": "u1", "children": [],
                        "message": { "id": "a1", "author": { "role": "assistant" }, "create_time": 1700000003.0,
                                     "content": { "content_type": "text", "parts": ["4"] } }
                    }
                }
            },
            {
                "title": null,
                "create_time": 1700001000,
                "update_time": null,
                "current_node": "a",
                "mapping": {
                    "r": { "id": "r", "message": null, "parent": null, "children": ["u"] },
                    "u": {
                        "id": "u", "parent": "r", "children": ["t"],
                        "message": { "id": "u", "author": { "role": "user" },
                                     "content": { "content_type": "multimodal_text",
                                                  "parts": [ { "content_type": "image_asset_pointer", "asset_pointer": "file-service://file-1" },
                                                             "What is this?" ] } }
                    },
                    "t": {
                        "id": "t", "parent": "u", "children": ["a"],
                        "message": { "id": "t", "author": { "role": "tool", "name": "dalle.text2im" },
                                     "content": { "content_type": "text", "parts": ["{\"caption\": \"a cat\"}"] } }
                    },
                    "a": {
                        "id": "a", "parent": "t", "children": [],
                        "message": { "id": "a", "author": { "role": "assistant" },
                                     "content": { "content_type": "text", "parts": ["It is a cat."] } }
                    }
                }
            }
        ])
    }

    /// ADR-0096 invariant 12.
    #[tokio::test]
    async fn import_walks_the_openai_export_along_current_node() {
        let (store, _dir) = store();
        let report = store.import(openai_export()).await.expect("import");
        assert_eq!(report.imported, 2);
        assert_eq!(report.ids.len(), 2);
        assert_eq!(
            report.skipped,
            vec![
                Skipped { reason: "empty message".into(), count: 1 },
                Skipped { reason: "non-text content_type multimodal_text".into(), count: 1 },
                Skipped { reason: "role tool".into(), count: 1 },
            ]
        );

        let first = store.get(&report.ids[0]).await.expect("get").expect("stored");
        assert_eq!(first.title, "Hello there");
        assert_eq!(first.created_at, 1_700_000_000_500);
        assert_eq!(first.updated_at, 1_700_000_100_000);
        assert_eq!(first.model_id, None);
        let turns: Vec<(&str, &str)> = first.messages.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
        assert_eq!(turns, [("user", "Hello there, what is 2+2?"), ("assistant", "4")], "the abandoned '5' is not on the path");
        for message in &first.messages {
            assert_eq!(message.extra.get("source"), Some(&json!(SOURCE_OPENAI_EXPORT)));
            assert_eq!(message.id.len(), 32, "a v4 uuid, simple form: {}", message.id);
        }

        let second = store.get(&report.ids[1]).await.expect("get").expect("stored");
        assert_eq!(second.title, "It is a cat.", "no title in the export: derived from the first kept turn");
        assert_eq!(second.created_at, 1_700_001_000_000);
        assert_eq!(second.updated_at, second.created_at, "no update_time: the creation time");
        assert_eq!(second.messages.len(), 1);
        assert_eq!(second.messages[0].role, "assistant");
    }

    #[tokio::test]
    async fn an_openai_conversation_without_a_current_node_follows_the_last_children() {
        let mapping = json!({
            "root": { "id": "root", "message": null, "parent": null, "children": ["u"] },
            "u": { "id": "u", "parent": "root", "children": ["a-old", "a"],
                   "message": { "author": { "role": "user" }, "content": { "content_type": "text", "parts": ["q"] } } },
            "a-old": { "id": "a-old", "parent": "u", "children": [],
                       "message": { "author": { "role": "assistant" }, "content": { "content_type": "text", "parts": ["old"] } } },
            "a": { "id": "a", "parent": "u", "children": [],
                   "message": { "author": { "role": "assistant" }, "content": { "content_type": "text", "parts": ["new"] } } }
        });
        let chain = openai_chain(mapping.as_object().expect("object"), None);
        assert_eq!(chain, ["root", "u", "a"]);
        let chain = openai_chain(mapping.as_object().expect("object"), Some("a-old"));
        assert_eq!(chain, ["root", "u", "a-old"], "an explicit current node wins");
    }

    #[tokio::test]
    async fn import_accepts_a_generic_list() {
        let (store, _dir) = store();
        let body = json!([
            {
                "title": "  From   another app ",
                "messages": [
                    { "role": "system", "content": "Be brief." },
                    { "role": "user", "content": "hi" },
                    { "role": "tool", "content": "ignored" },
                    { "role": "assistant", "content": [ { "type": "text", "text": "hello" }, { "type": "image_url", "image_url": { "url": "x" } } ] },
                    { "role": "user", "content": "   " }
                ]
            },
            { "messages": [ { "role": "user", "content": "a question whose first forty-eight characters become the title of this chat" } ] },
            { "messages": [] },
            "not a conversation"
        ]);
        let report = store.import(body).await.expect("import");
        assert_eq!(report.imported, 2);
        assert_eq!(skipped(&report, "role tool"), 1);
        assert_eq!(skipped(&report, "non-text part"), 1);
        assert_eq!(skipped(&report, "empty message"), 1);
        assert_eq!(skipped(&report, "conversation with no messages"), 1);
        assert_eq!(skipped(&report, "unreadable conversation"), 1);

        let first = store.get(&report.ids[0]).await.expect("get").expect("stored");
        assert_eq!(first.title, "From   another app", "a given title is trimmed, not rewritten");
        let turns: Vec<(&str, &str)> = first.messages.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
        assert_eq!(turns, [("system", "Be brief."), ("user", "hi"), ("assistant", "hello")]);
        assert!(first.messages.iter().all(|m| m.extra.get("source") == Some(&json!(SOURCE_GENERIC))));

        let second = store.get(&report.ids[1]).await.expect("get").expect("stored");
        assert_eq!(second.title, "a question whose first forty-eight characters be…", "48 characters, then the ellipsis");
    }

    #[tokio::test]
    async fn the_windows_own_cache_is_imported_whole() {
        let (cache, _dir) = store();
        // zustand's persist envelope, as `localStorage['misaka-studio.session']` holds it.
        let envelope = json!({ "state": { "conversations": [ui_persisted_conversation()], "activeConversationId": null, "view": "chat" }, "version": 1 });
        let report = cache.import(envelope).await.expect("import");
        assert_eq!(report.ids, vec!["k3j5h2f9lmn0abc".to_string()], "the UI's id is kept");
        let stored = cache.get("k3j5h2f9lmn0abc").await.expect("get").expect("stored");
        assert!(stored.messages[0].mining.is_some(), "the mining badge is kept");
        assert!(stored.messages.iter().all(|m| m.extra.get("source") == Some(&json!(SOURCE_STUDIO_CACHE))));

        // A bare list of the UI's conversations is Studio-shaped too — not the generic list.
        let (other, _dir_other) = store();
        let report = other.import(json!([ui_persisted_conversation()])).await.expect("import");
        assert_eq!(report.imported, 1);
        assert!(other.get("k3j5h2f9lmn0abc").await.expect("get").expect("stored").messages[1].stats.is_some());
    }

    #[tokio::test]
    async fn an_unrecognised_shape_is_refused_by_name() {
        let (store, _dir) = store();
        for body in [json!({ "foo": 1 }), json!(42), json!("text"), json!({ "conversations": [] })] {
            let err = store.import(body.clone()).await.expect_err("refused");
            assert!(matches!(err, Error::BadRequest { .. }), "{body}: {err}");
            assert!(err.to_string().contains("conversations.json"), "names what it reads: {err}");
        }
        let err = store.import(json!({ "schema": "misaka-studio/conversations/v9", "conversations": [] })).await.expect_err("refused");
        assert!(err.to_string().contains("v9"), "names the schema it saw: {err}");
        assert!(!store.dir().exists(), "a refused import writes nothing");

        assert_eq!(store.import(json!([])).await.expect("empty").imported, 0);
    }
}
