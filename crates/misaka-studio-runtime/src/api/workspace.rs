//! `/api/v1/workspace/…` — the coding agent's file and process API.
//!
//! The logic is in [`crate::workspace`]; this module is the HTTP shape. **Who may call it at all**
//! is decided in front of it, by the stricter of the two guards in [`super::guard`]: these routes
//! read any file in a folder, overwrite it, and run shell commands, so they answer only a runtime
//! bound to loopback, a peer on this machine, and the Studio's own window — never a page the
//! person happened to have open, and never an origin merely listed in `server.cors_origins`.

use crate::state::AppState;
use crate::workspace::{self, SearchRequest, Workspace, git};
use crate::{Error, Result};
use axum::Json;
use axum::extract::{FromRequest, Query, Request};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::convert::Infallible;
use std::sync::Arc;

pub fn router() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route("/browse", get(browse))
        .route("/open", post(open))
        .route("/tree", post(tree))
        .route("/files", post(files))
        .route("/read", post(read))
        .route("/search", post(search))
        .route("/write", post(write))
        .route("/edit", post(edit))
        .route("/remove", post(remove))
        .route("/exec", post(exec))
        .route("/exec/stream", post(exec_stream))
        .route("/git/status", post(git_status))
        .route("/git/diff", post(git_diff))
}

// ---------------------------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------------------------

/// `Json<T>`, with a malformed body answered in the API's own error shape — a model's tool call
/// that left out `old_string` gets "missing field `old_string`" as a sentence it can act on.
pub struct Body<T>(pub T);

impl<S, T> FromRequest<S> for Body<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Error;

    async fn from_request(request: Request, state: &S) -> std::result::Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Body(value)),
            Err(rejection) => Err(Error::bad_request(rejection.body_text())),
        }
    }
}

/// Numbers and booleans as a model writes them: `3`, `3.0` or `"3"`; `true` or `"true"`.
mod lenient {
    use serde::Deserialize;
    use serde::de::{Deserializer, Error};
    use serde_json::Value;

    pub fn usize_opt<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<usize>, D::Error> {
        let value = Option::<Value>::deserialize(deserializer)?;
        let number = match &value {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64)),
            Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
            Some(_) => None,
        };
        match number {
            Some(n) => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
            None => Err(D::Error::custom(format!("expected a non-negative whole number, got {}", value.unwrap_or(Value::Null)))),
        }
    }

    pub fn bool_opt<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
        let value = Option::<Value>::deserialize(deserializer)?;
        match &value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(Value::Number(n)) if n.as_u64() == Some(1) => Ok(Some(true)),
            Some(Value::Number(n)) if n.as_u64() == Some(0) => Ok(Some(false)),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(Some(true)),
                "false" | "0" | "no" => Ok(Some(false)),
                _ => Err(D::Error::custom(format!("expected true or false, got {s:?}"))),
            },
            Some(other) => Err(D::Error::custom(format!("expected true or false, got {other}"))),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub hidden: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenBody {
    pub root: String,
}

#[derive(Debug, Deserialize)]
pub struct TreeBody {
    pub root: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub depth: Option<usize>,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct FilesBody {
    pub root: String,
    #[serde(default)]
    pub query: String,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ReadBody {
    pub root: String,
    pub path: String,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub start_line: Option<usize>,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub end_line: Option<usize>,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SearchBody {
    pub root: String,
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub glob: Option<String>,
    #[serde(default, deserialize_with = "lenient::bool_opt")]
    pub regex: Option<bool>,
    #[serde(default, deserialize_with = "lenient::bool_opt")]
    pub case_insensitive: Option<bool>,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct WriteBody {
    pub root: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct EditBody {
    pub root: String,
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default, deserialize_with = "lenient::bool_opt")]
    pub replace_all: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveBody {
    pub root: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct ExecBody {
    pub root: String,
    pub command: String,
    #[serde(default, deserialize_with = "lenient::usize_opt")]
    pub timeout_secs: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct GitDiffBody {
    pub root: String,
    /// One file or folder; absent, `null` or empty for the whole root.
    #[serde(default)]
    pub path: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Handlers. Each one opens the workspace and does its work on the blocking pool: a canonicalize
// or a directory walk is a syscall that can wait on a slow disk, a network mount, or — on macOS —
// a privacy prompt the person has not answered yet.
// ---------------------------------------------------------------------------------------------

async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|e| Error::io("workspace", std::io::Error::other(e.to_string())))?
}

fn parse_flag(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
}

async fn browse(
    query: std::result::Result<Query<BrowseQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<workspace::BrowseResponse>> {
    let Query(query) = query.map_err(|e| Error::bad_request(e.body_text()))?;
    let hidden = parse_flag(query.hidden.as_deref());
    blocking(move || workspace::browse(query.path.as_deref(), hidden)).await.map(Json)
}

async fn open(Body(body): Body<OpenBody>) -> Result<Json<workspace::OpenResponse>> {
    blocking(move || workspace::open(&body.root)).await.map(Json)
}

async fn tree(Body(body): Body<TreeBody>) -> Result<Json<workspace::TreeResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::tree(&root, body.path.as_deref(), body.depth, body.limit)
    })
    .await
    .map(Json)
}

async fn files(Body(body): Body<FilesBody>) -> Result<Json<workspace::FilesResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::files(&root, &body.query, body.limit)
    })
    .await
    .map(Json)
}

async fn read(Body(body): Body<ReadBody>) -> Result<Json<workspace::ReadResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::read(&root, &body.path, body.start_line, body.end_line, body.max_bytes)
    })
    .await
    .map(Json)
}

async fn search(Body(body): Body<SearchBody>) -> Result<Json<workspace::SearchResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        let request = SearchRequest {
            pattern: body.pattern,
            path: body.path,
            glob: body.glob,
            regex: body.regex.unwrap_or(true),
            case_insensitive: body.case_insensitive.unwrap_or(false),
            max_results: body.max_results,
        };
        workspace::search(&root, &request)
    })
    .await
    .map(Json)
}

async fn write(Body(body): Body<WriteBody>) -> Result<Json<workspace::WriteResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::write(&root, &body.path, body.content)
    })
    .await
    .map(Json)
}

async fn edit(Body(body): Body<EditBody>) -> Result<Json<workspace::EditResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::edit(&root, &body.path, &body.old_string, &body.new_string, body.replace_all.unwrap_or(false))
    })
    .await
    .map(Json)
}

async fn remove(Body(body): Body<RemoveBody>) -> Result<Json<workspace::RemoveResponse>> {
    blocking(move || {
        let root = Workspace::open(&body.root)?;
        workspace::remove(&root, &body.path)
    })
    .await
    .map(Json)
}

async fn exec(Body(body): Body<ExecBody>) -> Result<Json<workspace::ExecResponse>> {
    let root_path = body.root.clone();
    let root = blocking(move || Workspace::open(&root_path)).await?;
    let timeout = body.timeout_secs.map(|s| s as u64);
    workspace::exec(&root, &body.command, timeout).await.map(Json)
}

/// The terminal: server-sent events, one JSON [`workspace::ExecEvent`] per `data:` line. A bad
/// root or an empty command is still a plain 400; once the stream has started, a command that
/// cannot be started is an `error` event. The client closing the connection stops the command.
async fn exec_stream(Body(body): Body<ExecBody>) -> Result<Response> {
    let root_path = body.root.clone();
    let root = blocking(move || Workspace::open(&root_path)).await?;
    let timeout = body.timeout_secs.map(|s| s as u64);
    let events = workspace::exec_stream(&root, &body.command, timeout)?;
    // Strings and numbers always serialize; a JSON string never holds a raw newline, so each
    // event is exactly one `data:` line.
    let frames = events.map(|event| Ok::<Event, Infallible>(Event::default().data(serde_json::to_string(&event).unwrap_or_default())));
    Ok(Sse::new(frames).keep_alive(KeepAlive::default()).into_response())
}

async fn git_status(Body(body): Body<OpenBody>) -> Result<Json<git::GitStatus>> {
    let root = blocking(move || Workspace::open(&body.root)).await?;
    git::status(&root).await.map(Json)
}

async fn git_diff(Body(body): Body<GitDiffBody>) -> Result<Json<git::GitDiff>> {
    let (root, path) = blocking(move || {
        let root = Workspace::open(&body.root)?;
        let path = match body.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(path) => Some(root.resolve(path)?.rel),
            None => None,
        };
        Ok((root, path))
    })
    .await?;
    git::diff(&root, path.as_deref()).await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_shaped_numbers_and_flags_parse() {
        let tree: TreeBody = serde_json::from_str(r#"{"root":"/x","depth":"3","limit":50.0}"#).unwrap();
        assert_eq!((tree.depth, tree.limit), (Some(3), Some(50)));
        let search: SearchBody = serde_json::from_str(r#"{"root":"/x","pattern":"a","regex":"false","case_insensitive":1}"#).unwrap();
        assert_eq!((search.regex, search.case_insensitive), (Some(false), Some(true)));
        assert!(serde_json::from_str::<TreeBody>(r#"{"root":"/x","depth":-1}"#).is_err());
        let edit: EditBody = serde_json::from_str(r#"{"root":"/x","path":"a","old_string":"a","new_string":"b"}"#).unwrap();
        assert_eq!(edit.replace_all, None);
    }
}
