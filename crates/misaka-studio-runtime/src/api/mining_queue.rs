//! **The mining queue over HTTP** — what the Chat tab enqueues into, what the badges read from.
//!
//! See `crate::mining_queue` for why this exists. The API is deliberately small: list, enqueue,
//! drop, retry, and the mode switch. Everything about a job is on the job itself, including the
//! lane's own words for a refusal — this layer translates nothing.

use crate::mining_queue::{Counts, MiningJob};
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use misaka_studio_core::settings::MiningMode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list).post(enqueue))
        .route("/mode", put(set_mode))
        .route("/{id}", axum::routing::delete(remove))
        .route("/{id}/retry", post(retry))
}

#[derive(Clone, Debug, Serialize)]
pub struct MiningQueueView {
    /// The setting, as written.
    pub mode: MiningMode,
    /// Whether `Background` can actually be honoured right now: a pool slot with a gateway is
    /// configured AND an engine other than that gateway can answer the chat. When this is false
    /// the chat still mines inline, and the UI must not enqueue on top of that — one prompt would
    /// be mined twice.
    pub background_available: bool,
    /// Why not, when it is not. Sentences, for the panel.
    pub background_blocker: Option<String>,
    pub gateway_url: Option<String>,
    pub counts: Counts,
    pub jobs: Vec<MiningJob>,
}

async fn view(state: &AppState) -> MiningQueueView {
    let settings = state.settings.read().await.clone();
    let gateway_url = settings.node.palw_gateway_url.clone();
    let (background_available, background_blocker) = match (&gateway_url, state.local_engine_for_loaded_model().await) {
        (None, _) => (false, Some("no pool slot with a prompt-mining gateway is configured — join one from the Network tab".to_string())),
        (Some(_), Err(why)) => (false, Some(why)),
        (Some(_), Ok(())) => (true, None),
    };
    MiningQueueView {
        mode: settings.node.mining_mode,
        background_available,
        background_blocker,
        gateway_url,
        counts: state.mining.counts().await,
        jobs: state.mining.list().await,
    }
}

async fn list(State(state): State<Arc<AppState>>) -> Json<MiningQueueView> {
    Json(view(&state).await)
}

#[derive(Debug, Deserialize)]
pub struct EnqueueBody {
    pub prompt: String,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
}

async fn enqueue(State(state): State<Arc<AppState>>, Json(body): Json<EnqueueBody>) -> Result<Json<MiningJob>> {
    let prompt = body.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(Error::bad_request("a prompt with no text is not a job"));
    }
    let settings = state.settings.read().await.clone();
    let Some(gateway_url) = settings.node.palw_gateway_url.clone() else {
        return Err(Error::bad_request("no prompt-mining gateway is configured — join a pool slot for prompt mining first"));
    };
    let system = settings.generation.system_prompt.trim().to_string();
    let job = state
        .mining
        .enqueue(prompt, (!system.is_empty()).then_some(system), body.conversation_id, body.message_id, gateway_url)
        .await;
    Ok(Json(job))
}

async fn remove(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<serde_json::Value>> {
    if state.mining.remove(&id).await {
        Ok(Json(serde_json::json!({ "removed": id })))
    } else {
        Err(Error::bad_request("no such queued job — a running job finishes on its own, a finished one is history"))
    }
}

async fn retry(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<serde_json::Value>> {
    if state.mining.retry(&id).await {
        Ok(Json(serde_json::json!({ "requeued": id })))
    } else {
        Err(Error::bad_request("only a refused or failed job can be retried"))
    }
}

#[derive(Debug, Deserialize)]
pub struct ModeBody {
    pub mode: MiningMode,
}

async fn set_mode(State(state): State<Arc<AppState>>, Json(body): Json<ModeBody>) -> Result<Json<MiningQueueView>> {
    // Switching the mode moves the chat between engines, and an engine change unloads the model.
    // The person flipped one switch; they should not also have to find the model list again. So
    // whatever was loaded is loaded back — into the engine the new mode picks for it.
    let loaded = state.loaded().await.map(|l| l.model.id.clone());
    let mut new = state.settings.read().await.clone();
    new.node.mining_mode = body.mode;
    state.apply_settings(new).await?;
    if let Some(model_id) = loaded
        && state.loaded().await.is_none()
        && let Err(e) = state.load(&model_id, None).await
    {
        // The mode did change; only the reload did not. Said in the log and left to the model
        // list, where the reason (an engine not installed, a file gone) is shown with its remedy.
        tracing::warn!(model = %model_id, "reload after the mining-mode switch failed: {e}");
    }
    Ok(Json(view(&state).await))
}
