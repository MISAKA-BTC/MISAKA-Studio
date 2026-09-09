//! `/api/v1/conversations` — the chat history as an API (ADR-0096 Decision 12).
//!
//! ```text
//!   GET    /            the list, newest activity first
//!   GET    /export      everything, as one document a person can keep
//!   POST   /import      a Studio export, OpenAI's conversations.json, or a generic list
//!   GET    /{id}
//!   PUT    /{id}        upsert; the body's id must be the path's
//!   DELETE /{id}
//! ```
//!
//! The UI writes through `PUT` on every change and keeps `localStorage` as a cache for the
//! window; the store (`crate::conversations`) is the truth. `export` and `import` are what make
//! the history the person's: a file to copy out, and a way in from another app.

use crate::conversations::{Conversation, ConversationSummary, Export, ImportReport};
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;

/// OpenAI's `conversations.json` runs to tens of megabytes after a year of use, and one long chat
/// can pass axum's 2 MiB default on its own. Per request, not per store.
const BODY_LIMIT: usize = 256 * 1024 * 1024;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list))
        .route("/export", get(export))
        .route("/import", post(import))
        .route("/{id}", get(get_one).put(put_one).delete(delete_one))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

async fn list(State(state): State<Arc<AppState>>) -> Result<Json<Vec<ConversationSummary>>> {
    Ok(Json(state.conversations.list().await?))
}

async fn get_one(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<Conversation>> {
    state.conversations.get(&id).await?.map(Json).ok_or_else(|| not_found(&id))
}

/// Upsert. The path names the file and the body names itself; the store refuses the two
/// disagreeing, so a client cannot write conversation A's content under B's name.
async fn put_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(conversation): Json<Conversation>,
) -> Result<Json<ConversationSummary>> {
    state.conversations.put_as(&id, &conversation).await?;
    Ok(Json(ConversationSummary::of(&conversation)))
}

async fn delete_one(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<serde_json::Value>> {
    if !state.conversations.delete(&id).await? {
        return Err(not_found(&id));
    }
    Ok(Json(serde_json::json!({ "deleted": id })))
}

async fn export(State(state): State<Arc<AppState>>) -> Result<Json<Export>> {
    Ok(Json(state.conversations.export_all().await?))
}

async fn import(State(state): State<Arc<AppState>>, Json(body): Json<serde_json::Value>) -> Result<Json<ImportReport>> {
    Ok(Json(state.conversations.import(body).await?))
}

/// The crate's spelling for a record that is not there (`/records/{id}` says it the same way).
fn not_found(id: &str) -> Error {
    Error::bad_request(format!("no conversation with id '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
    use serde_json::json;

    async fn studio() -> (Arc<AppState>, tempfile::TempDir) {
        let data = tempfile::tempdir().expect("tempdir");
        let settings = Settings {
            models_dir: data.path().join("models"),
            backend: BackendSettings { kind: BackendKind::Mock, ..Default::default() },
            ..Default::default()
        };
        std::fs::create_dir_all(&settings.models_dir).expect("models dir");
        let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
        (state, data)
    }

    fn conversation(id: &str) -> Conversation {
        serde_json::from_value(json!({ "id": id, "title": "t", "createdAt": 1, "updatedAt": 2, "modelId": null, "messages": [] }))
            .expect("parses")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_store_lives_under_the_data_directory_and_the_routes_reach_it() {
        let (state, data) = studio().await;
        assert_eq!(state.conversations.dir(), data.path().join("conversations"));

        let summary = put_one(State(state.clone()), Path("abc".into()), Json(conversation("abc"))).await.expect("put").0;
        assert_eq!(summary.id, "abc");
        assert!(data.path().join("conversations/abc.json").is_file());
        assert_eq!(list(State(state.clone())).await.expect("list").0.len(), 1);
        assert_eq!(get_one(State(state.clone()), Path("abc".into())).await.expect("get").0.id, "abc");
        assert_eq!(export(State(state.clone())).await.expect("export").0.conversations.len(), 1);

        let body = json!([{ "messages": [{ "role": "user", "content": "hi" }] }]);
        let report = import(State(state.clone()), Json(body)).await.expect("import").0;
        assert_eq!(report.imported, 1);
        assert_eq!(list(State(state.clone())).await.expect("list").0.len(), 2);

        let deleted = delete_one(State(state.clone()), Path("abc".into())).await.expect("delete").0;
        assert_eq!(deleted, json!({ "deleted": "abc" }));
        assert!(!data.path().join("conversations/abc.json").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_whose_id_is_not_the_path_is_refused_and_a_missing_id_is_named() {
        let (state, _data) = studio().await;
        let err = put_one(State(state.clone()), Path("abc".into()), Json(conversation("xyz"))).await.expect_err("mismatch");
        assert!(matches!(err, Error::BadRequest { .. }), "{err}");
        assert!(state.conversations.list().await.expect("list").is_empty(), "nothing is stored under either id");

        let err = get_one(State(state.clone()), Path("abc".into())).await.expect_err("missing");
        assert!(err.to_string().contains("abc"), "{err}");
        let err = delete_one(State(state.clone()), Path("abc".into())).await.expect_err("missing");
        assert!(err.to_string().contains("abc"), "{err}");
        let err = get_one(State(state), Path("../etc/passwd".into())).await.expect_err("not an id");
        assert!(matches!(err, Error::BadRequest { .. }), "{err}");
    }
}
