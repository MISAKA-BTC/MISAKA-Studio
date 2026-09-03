//! `/api/v1/network` — participation in the MISAKA network, as an API.
//!
//! The shape mirrors the ladder: what the chain offers (`/classes` — the mining class list),
//! what this machine is doing about it (`/` — role, node status, activity), and the two verbs
//! that change that (`/node/start`, `/node/stop`). Everything a button does here is also a
//! visible command line, because a person putting a bonded key on the line must be able to
//! reproduce — and audit — what ran without this app.

use crate::node::NodeView;
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use misaka_studio_core::palw::{PalwArtifactSource, PalwClassStatus, TESTNET11_CLASSES, assess_classes};
use misaka_studio_core::settings::{NetworkRole, NodeNetwork};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(overview))
        .route("/classes", get(classes))
        .route("/classes/{name}/download", post(download_artifact))
        .route("/node/start", post(start_node))
        .route("/node/reset", post(reset_node))
        .route("/node/stop", post(stop_node))
        .route("/node/log", get(node_log))
        .route("/producer-key", post(producer_key))
}

/// The whole network picture in one response — what the UI's Network tab renders.
#[derive(Serialize)]
struct NetworkOverview {
    role: NetworkRole,
    network: NodeNetwork,
    node: NodeView,
    classes: Vec<PalwClassStatus>,
    /// True when this build of the Studio found a node binary it could launch.
    kaspad_found: bool,
    kaspad_path: String,
}

/// Scan the models directory for PALW artifacts (`.palwq36`, `.palwart`).
///
/// The same directory models live in, on purpose: it is the one place users already know, and
/// the GGUF scanner ignores these extensions so the two lists cannot contaminate each other.
async fn artifact_scan(state: &AppState) -> Vec<(String, String, u64)> {
    let dir = state.settings.read().await.models_dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else { return out };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.ends_with(".palwq36") || name.ends_with(".palwart")) {
                continue;
            }
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                out.push((entry.path().display().to_string(), name, meta.len()));
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

async fn overview(State(state): State<Arc<AppState>>) -> Result<Json<NetworkOverview>> {
    let settings = state.settings.read().await.clone();
    let node = state.node.view(&settings.node).await?;
    let artifacts = artifact_scan(&state).await;
    let classes = assess_classes(&artifacts, state.hardware.total_memory);
    let kaspad = crate::node::NodeManager::resolve_kaspad(settings.node.kaspad_path.as_ref());
    Ok(Json(NetworkOverview {
        role: settings.node.role,
        network: settings.node.network,
        node,
        classes,
        kaspad_found: kaspad.is_file(),
        kaspad_path: kaspad.display().to_string(),
    }))
}

async fn classes(State(state): State<Arc<AppState>>) -> Json<Vec<PalwClassStatus>> {
    let artifacts = artifact_scan(&state).await;
    Json(assess_classes(&artifacts, state.hardware.total_memory))
}

/// Download a class artifact into the models directory, verified against the chain-pinned digest.
///
/// Only the classes whose artifact is published as a file (QWEN36) can be downloaded; a
/// convert-locally class answers 400 carrying the conversion command instead — an error that
/// tells the user the actual next step.
async fn download_artifact(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<crate::download::DownloadProgress>> {
    let spec = TESTNET11_CLASSES
        .iter()
        .find(|class| class.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| Error::bad_request(format!("no PALW class named '{name}'")))?;

    match &spec.artifact {
        PalwArtifactSource::Download { repo_path, sha256, size_bytes, hf_repo, .. } => {
            let settings = state.settings.read().await.clone();
            let catalog = state.catalog().await;
            let progress = state
                .downloads
                .start(
                    &catalog,
                    state.store.clone(),
                    settings.models_dir.clone(),
                    hf_repo.to_string(),
                    // The artifact is pinned by content digest, so `main` is safe here in a way
                    // it is not for models: a moved branch cannot change what verifies.
                    "main".to_string(),
                    // The path inside the repository — the download manager takes the basename
                    // for the destination, which is the name the class scan looks for.
                    repo_path.to_string(),
                    Some(sha256.to_string()),
                    Some(*size_bytes),
                    None,
                )
                .await?;
            Ok(Json(progress))
        }
        PalwArtifactSource::ConvertLocally { convert_command, source_repo, .. } => Err(Error::bad_request(format!(
            "{} has no published download — convert it locally from {source_repo}: `{convert_command}` (in the misakas repository), then place the output in the models directory",
            spec.name
        ))),
        PalwArtifactSource::DerivedFromSeed => {
            Err(Error::bad_request(format!("{} needs no artifact — every node derives it from a seed", spec.name)))
        }
    }
}

/// Restart the node after deleting a data directory that holds a different chain.
///
/// A separate verb from `/node/start`, not a flag on it: this one destroys the local chain, and a
/// caller cannot reach it by leaving a field unset. It refuses unless the node actually said the
/// data was stale — so it cannot be used as a general "wipe my node" button, and a user who clicks
/// it is answering the exact question the node asked.
async fn reset_node(State(state): State<Arc<AppState>>) -> Result<Json<NodeView>> {
    let settings = state.settings.read().await.clone();
    let view = state.node.view(&settings.node).await?;
    if !matches!(view.blocker, Some(crate::node::NodeBlocker::StaleChainData { .. })) {
        return Err(Error::bad_request(
            "this node did not report stale chain data — nothing here would delete a chain on a guess.              Start it normally and read what it says.",
        ));
    }
    let mut node_settings = settings.node.clone();
    if node_settings.class_artifact.is_none() {
        node_settings.class_artifact = default_class_artifact(&settings.models_dir).await;
    }
    Ok(Json(state.node.start_accepting_data_loss(&node_settings).await?))
}

#[derive(Deserialize)]
struct StartBody {
    /// Override the configured role for this launch, e.g. start as verifier while producer
    /// prerequisites are still being gathered.
    #[serde(default)]
    role: Option<NetworkRole>,
}

async fn start_node(State(state): State<Arc<AppState>>, body: Option<Json<StartBody>>) -> Result<Json<NodeView>> {
    let settings = state.settings.read().await.clone();
    let mut node_settings = settings.node.clone();
    if let Some(Json(StartBody { role: Some(role) })) = body {
        node_settings.role = role;
    }
    if node_settings.class_artifact.is_none() {
        node_settings.class_artifact = default_class_artifact(&settings.models_dir).await;
    }
    Ok(Json(state.node.start(&node_settings).await?))
}

/// The default class's artifact, when this machine holds it.
///
/// Resolved at launch instead of being written into the settings file on first run, because it is
/// a path *under the models directory*: pinning it once would keep naming the old directory the
/// moment someone moves their models, and the node would then refuse to produce over a file that
/// is sitting right where it should be. Left `None` when the file is absent or the wrong size, so
/// an empty setting still means "mine the floor" rather than "fail to start".
async fn default_class_artifact(models_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let spec = misaka_studio_core::palw::default_class();
    let PalwArtifactSource::Download { filename, size_bytes, .. } = &spec.artifact else { return None };
    let path = models_dir.join(filename);
    let meta = tokio::fs::metadata(&path).await.ok()?;
    (meta.len() == *size_bytes).then_some(path)
}

async fn stop_node(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    state.node.stop().await?;
    Ok(Json(serde_json::json!({ "stopped": true })))
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_log_limit")]
    limit: usize,
}

fn default_log_limit() -> usize {
    200
}

async fn node_log(State(state): State<Arc<AppState>>, Query(query): Query<LogQuery>) -> Json<Vec<String>> {
    Json(state.node.recent_log(query.limit.min(600)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use misaka_studio_core::palw::default_class;

    fn default_artifact_name() -> &'static str {
        match &default_class().artifact {
            PalwArtifactSource::Download { filename, .. } => filename,
            other => panic!("the default class must publish an artifact, got {other:?}"),
        }
    }

    fn default_artifact_size() -> u64 {
        match &default_class().artifact {
            PalwArtifactSource::Download { size_bytes, .. } => *size_bytes,
            other => panic!("the default class must publish an artifact, got {other:?}"),
        }
    }

    /// An empty models directory must not produce a path. Handing the node an artifact flag
    /// pointing at nothing would turn "mine the floor" into a node that refuses to start.
    #[tokio::test]
    async fn no_artifact_means_no_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(default_class_artifact(dir.path()).await, None);
    }

    /// The size is the check, not the name. A half-finished copy under the right filename is the
    /// case this exists for: the node would refuse it at startup, and having the Studio hand it
    /// over anyway costs the operator a sync to find out.
    #[tokio::test]
    async fn a_short_file_is_not_the_default_artifact() {
        let dir = tempfile::tempdir().expect("tempdir");
        tokio::fs::write(dir.path().join(default_artifact_name()), b"not the whole thing").await.expect("write");
        assert_eq!(default_class_artifact(dir.path()).await, None);
    }

    #[tokio::test]
    async fn a_full_sized_artifact_is_offered_by_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(default_artifact_name());
        // Sparse: the whole point is that this check reads metadata, not 1.7 GiB.
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(default_artifact_size()).expect("set_len");
        drop(file);

        assert_eq!(default_class_artifact(dir.path()).await, Some(path));
    }
}

/// `POST /api/v1/network/producer-key` — mint the producer's ML-DSA-87 seed on this machine.
///
/// A bonded producer is a key: the seed derives the verification key a bond is registered under,
/// signs every attempt, and — since the node derives the pay address from it — is the address
/// rewards land at. The Studio writes 32 bytes from the OS random source as hex into a 0600 file
/// under the data directory (the node refuses any looser mode) and points `node.producer_key_path`
/// at it. It never reads the file back and never returns the seed: the response names the path,
/// and the address appears in the node's own log once it starts. Refuses to overwrite an existing
/// seed — a producer key that is replaced silently is a bond that can no longer sign.
async fn producer_key(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let settings = state.settings.read().await.clone();
    let path = state.data_dir.join("producer.seed");
    if path.exists() {
        return Err(Error::bad_request(format!(
            "a producer seed already exists at {} — remove it yourself if you mean to replace the key",
            path.display()
        )));
    }
    // Two v4 UUIDs are 32 bytes from the OS random source (`getrandom`), which is the same well
    // `misaka key gen` draws from; the version/variant nibbles cost 6 bits of the 256, which is
    // why this is not a UUID and is not shown as one.
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let hex = hex::encode(seed);
    std::fs::create_dir_all(&state.data_dir).map_err(|e| Error::io(state.data_dir.display(), e))?;
    std::fs::write(&path, format!("{hex}\n")).map_err(|e| Error::io(path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| Error::io(path.display(), e))?;
    }
    let mut new = settings.clone();
    new.node.producer_key_path = Some(path.clone());
    state.apply_settings(new).await?;
    Ok(Json(serde_json::json!({
        "producer_key_path": path.display().to_string(),
        "next": "start the node as a producer: it registers a bond under this key and prints the address to fund",
    })))
}
