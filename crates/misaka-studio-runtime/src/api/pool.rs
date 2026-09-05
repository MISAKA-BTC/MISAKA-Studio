//! `/api/v1/network/pool` — mining through a hosted producer, for a machine that runs no node.
//!
//! The pool (misakascan.com/pool) rents out producer *slots*: a real `kaspad --palw-produce`
//! on the pool host, with its own seed and its own bond. Joining creates the slot; funding the
//! slot's address is the entire remaining ask — the slot registers its bond by itself and mines.
//!
//! Two things this module is careful to say out loud, because the convenience hides them:
//!
//! * **The slot's seed lives on the pool host.** That is not a leak, it is the deal — "mine
//!   without a node" means someone else's node holds the key that signs your blocks and owns
//!   your rewards. The join response carries the seed exactly once; we write it to a 0600 file
//!   in the data directory so the user is never *only* trusting the pool, and we name that file
//!   in every status response.
//! * **This Studio is a client of the pool, not its operator.** Status is whatever the pool's
//!   own API answers, passed through — inventing a friendlier shape here would let the Studio
//!   claim things about a remote producer it cannot see.

use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

/// The operator's public pool. A different one is a `url` away — the API is three routes.
const DEFAULT_POOL_URL: &str = "https://misakascan.com/pool";

/// `POST /api/v1/network/faucet` `{ "address": "misakatest:…" }` — the faucet at the pool's origin,
/// for an address that is not a slot's: the own-node path's, which the node prints at start.
/// Same pass-through as [`faucet`]: the faucet's answer is the answer.
pub async fn faucet_for_address(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    let address = body
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|a| a.contains(':'))
        .ok_or_else(|| Error::bad_request("faucet needs an address"))?;
    let url = state.settings.read().await.node.pool_url.clone().unwrap_or_else(|| DEFAULT_POOL_URL.to_string());
    let origin = pool_origin(&url);
    let response = http()
        .post(format!("{origin}/faucet/v1/claim"))
        .json(&serde_json::json!({ "address": address }))
        .send()
        .await
        .map_err(|e| Error::bad_request(format!("the faucet did not answer: {e}")))?;
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let why = body.get("error").and_then(|v| v.as_str()).unwrap_or("no reason given");
        return Err(Error::bad_request(format!("the faucet refused ({status}): {why}")));
    }
    Ok(Json(body))
}

/// `https://host/pool` → `https://host` — the faucet is the pool origin's sibling.
fn pool_origin(url: &str) -> String {
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[after_scheme..].find('/') {
        Some(slash) => url[..after_scheme + slash].to_string(),
        None => url.to_string(),
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(status))
        .route("/join", post(join))
        .route("/leave", post(leave))
        .route("/faucet", post(faucet))
        .route("/fp/enable", post(fp_enable))
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("misaka-studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client builds")
}

fn seed_path(state: &AppState, slot_id: &str) -> std::path::PathBuf {
    state.data_dir.join(format!("pool-{slot_id}.seed"))
}

async fn pool_get(url: &str, token: Option<&str>) -> Result<serde_json::Value> {
    let mut request = http().get(url);
    if let Some(token) = token {
        request = request.header("x-pool-token", token);
    }
    let response = request.send().await.map_err(|e| Error::bad_request(format!("the pool did not answer: {url}: {e}")))?;
    let status = response.status();
    let body: serde_json::Value =
        response.json().await.map_err(|e| Error::bad_request(format!("the pool's answer was not JSON: {url}: {e}")))?;
    if !status.is_success() {
        let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unexplained");
        return Err(Error::bad_request(format!("the pool refused ({status}): {msg}")));
    }
    Ok(body)
}

/// What the Network tab renders: not joined (with the default URL to offer), or the slot's
/// live status as the pool tells it.
async fn status(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let node = state.settings.read().await.node.clone();
    let (Some(url), Some(slot_id), Some(token)) = (&node.pool_url, &node.pool_slot_id, &node.pool_slot_token) else {
        return Ok(Json(serde_json::json!({ "joined": false, "default_url": DEFAULT_POOL_URL })));
    };
    let mut body = pool_get(&format!("{url}/v1/slots/{slot_id}"), Some(token)).await?;
    // Best effort, and separate: a pool too old to know about the lane still answers the slot
    // route, and the panel should show what it does know rather than nothing.
    let fp = pool_get(&format!("{url}/v1/slots/{slot_id}/fp"), Some(token)).await.ok();
    if let Some(map) = body.as_object_mut() {
        map.insert("fp".into(), fp.unwrap_or(serde_json::Value::Null));
        map.insert("joined".into(), true.into());
        map.insert("pool_url".into(), url.clone().into());
        map.insert("seed_path".into(), seed_path(&state, slot_id).display().to_string().into());
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
struct JoinBody {
    #[serde(default)]
    url: Option<String>,
    /// `floor` (the default) or `fp`.
    ///
    /// It decides how large a bond the slot registers, and a bond's size is fixed the moment it
    /// registers — so this is not a setting that can be changed afterwards, it is which slot you
    /// are asking for. A floor slot mines the lottery; an `fp` slot mines what you type.
    #[serde(default)]
    mode: Option<String>,
}

async fn join(State(state): State<Arc<AppState>>, body: Option<Json<JoinBody>>) -> Result<Json<serde_json::Value>> {
    let settings = state.settings.read().await.clone();
    if settings.node.pool_slot_id.is_some() {
        return Err(Error::bad_request("already joined a pool slot — leave it first if you want a fresh one"));
    }
    let (asked_url, mode) = body.map(|Json(b)| (b.url, b.mode)).unwrap_or((None, None));
    let mode = mode.unwrap_or_else(|| "floor".to_string());
    let url = asked_url.or(settings.node.pool_url.clone()).unwrap_or_else(|| DEFAULT_POOL_URL.to_string());
    let url = url.trim_end_matches('/').to_string();
    if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")) {
        // A slot seed travels back over this connection once. Plaintext across a network is not
        // a place a key may transit, so anything unencrypted must be loopback.
        return Err(Error::bad_request("a pool URL must be https:// (or loopback http:// for development)"));
    }

    let response = http()
        .post(format!("{url}/v1/slots"))
        .json(&serde_json::json!({ "mode": mode }))
        .send()
        .await
        .map_err(|e| Error::bad_request(format!("the pool did not answer: {url}: {e}")))?;
    let status = response.status();
    let mut body: serde_json::Value =
        response.json().await.map_err(|e| Error::bad_request(format!("the pool's answer was not JSON: {e}")))?;
    if !status.is_success() {
        let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unexplained");
        return Err(Error::bad_request(format!("the pool refused the join ({status}): {msg}")));
    }

    let slot_id = body
        .get("slot_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::bad_request("the pool's join answer names no slot_id"))?
        .to_string();
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::bad_request("the pool's join answer carries no token"))?
        .to_string();

    // The seed's one transit ends here: into a 0600 file beside the settings, and out of the
    // response the UI sees. The pool holds its copy either way; ours is what makes the rewards
    // recoverable without the pool's cooperation.
    if let Some(seed) = body.get("seed_hex").and_then(|v| v.as_str()) {
        let path = seed_path(&state, &slot_id);
        std::fs::write(&path, format!("{seed}\n")).map_err(|e| Error::io(path.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        body.as_object_mut().map(|m| m.remove("seed_hex"));
        body.as_object_mut().map(|m| m.insert("seed_path".into(), path.display().to_string().into()));
    }

    let mut new = settings.clone();
    new.node.pool_url = Some(url.clone());
    new.node.pool_slot_id = Some(slot_id.clone());
    new.node.pool_slot_token = Some(token);
    if mode == "fp" {
        // **Joining for prompt mining is joining, not joining plus a setup step.** The slot's own
        // gateway is where this app's chat has to go for a chat to be that slot's work, so the
        // engine and its address are set here rather than left as two settings a person is
        // expected to find. Nothing mines until the slot is funded and its lane enabled; what this
        // decides is only where the chat goes when it is.
        new.backend.kind = misaka_studio_core::settings::BackendKind::Gateway;
        new.node.palw_gateway_url = Some(format!("{url}/v1/slots/{slot_id}/fp"));
        // And the chat does not wait for the lane. The slot mines every prompt from a queue behind
        // the chat, which answers from the engine that can answer now; when no local engine can
        // run the loaded model the runtime keeps the chat inline and says so in the panel.
        new.node.mining_mode = misaka_studio_core::settings::MiningMode::Background;
    }
    state.apply_settings(new).await?;

    Ok(Json(body))
}

/// **Turn on the slot's free-prompt lane.**
///
/// Separate from joining because it cannot happen at join time: the lane needs the slot's bond,
/// and the bond needs funding and a block. The pool refuses with a reason of its own — an unfunded
/// slot, a bond too small to carry a claim — and that reason is passed through rather than
/// translated, because it is about the chain and not about this app.
async fn fp_enable(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let node = state.settings.read().await.node.clone();
    let (Some(url), Some(slot_id), Some(token)) = (&node.pool_url, &node.pool_slot_id, &node.pool_slot_token) else {
        return Err(Error::bad_request("no pool slot — join one first"));
    };
    let response = http()
        .post(format!("{url}/v1/slots/{slot_id}/fp/enable"))
        .header("x-pool-token", token)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| Error::bad_request(format!("the pool did not answer: {e}")))?;
    let status = response.status();
    let body: serde_json::Value =
        response.json().await.map_err(|e| Error::bad_request(format!("the pool's answer was not JSON: {e}")))?;
    if !status.is_success() {
        let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unexplained");
        return Err(Error::bad_request(format!("the pool could not enable the lane: {msg}")));
    }
    Ok(Json(body))
}

/// Forget the slot. The pool keeps running it (its bond and claims are on-chain facts a client
/// cannot retract), and the seed file stays — deleting key material is not something an HTTP
/// endpoint gets to decide.
async fn leave(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let settings = state.settings.read().await.clone();
    let slot = settings.node.pool_slot_id.clone();
    let mut new = settings;
    new.node.pool_url = None;
    new.node.pool_slot_id = None;
    new.node.pool_slot_token = None;
    state.apply_settings(new).await?;
    Ok(Json(serde_json::json!({
        "left": slot,
        "note": "the slot itself keeps running on the pool host, and the seed file was kept — only this Studio forgot it"
    })))
}

/// Ask the faucet at the pool's own origin to fund the slot.
///
/// The misakascan deployment serves both from one host — `…/pool` and `…/faucet` — and its
/// faucet's grant (12 MSK) is sized to cover exactly one bond: the KIP-0009 relay floor puts
/// the smallest carryable collateral near 8.34M sompi, so a smaller grant would leave a slot
/// that can never register and a "get funds from the faucet" line that is a lie. The faucet's
/// own rules pass through untouched — one grant per address ever, one per source per day —
/// because restating limits we do not enforce is how docs drift from behaviour.
async fn faucet(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let node = state.settings.read().await.node.clone();
    let (Some(url), Some(slot_id), Some(token)) = (&node.pool_url, &node.pool_slot_id, &node.pool_slot_token) else {
        return Err(Error::bad_request("no pool slot to fund — join the pool first"));
    };

    // The slot's address is the pool's fact, not a stored copy that could go stale.
    let status = pool_get(&format!("{url}/v1/slots/{slot_id}"), Some(token)).await?;
    let address =
        status.get("address").and_then(|v| v.as_str()).ok_or_else(|| Error::bad_request("the pool's status names no slot address"))?;

    // `https://host/pool` → `https://host` — the faucet is the pool origin's sibling.
    let origin = {
        let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
        match url[after_scheme..].find('/') {
            Some(slash) => &url[..after_scheme + slash],
            None => url.as_str(),
        }
    };

    let response = http()
        .post(format!("{origin}/faucet/v1/claim"))
        .json(&serde_json::json!({ "address": address }))
        .send()
        .await
        .map_err(|e| Error::bad_request(format!("the faucet did not answer: {origin}: {e}")))?;
    let status_code = response.status();
    let body: serde_json::Value =
        response.json().await.map_err(|e| Error::bad_request(format!("the faucet's answer was not JSON: {e}")))?;
    if !status_code.is_success() {
        let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unexplained");
        return Err(Error::bad_request(format!("the faucet refused ({status_code}): {msg}")));
    }
    Ok(Json(body))
}
