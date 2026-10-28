//! `/api/v1/network` — participation in the MISAKA network, as an API.
//!
//! The Studio runs no node. What the chain offers (`/classes`), what its public nodes report (`/`), and the node-free
//! miner (`/remote-miner/*`). Everything a button does here is also a visible command line, because a person putting a
//! bonded key on the line must be able to reproduce — and audit — what ran without this app.

use crate::node::NodeStatus;
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use misaka_studio_core::palw;
use misaka_studio_core::palw::{
    PalwArtifactSource, PalwClassReadiness, PalwClassStatus, assess_classes, classes_for, read_artifact_header,
};
use misaka_studio_core::settings::NodeNetwork;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(overview))
        .route("/classes", get(classes))
        .route("/classes/{name}/download", post(download_artifact))
        .route("/published", get(published))
        .route("/published/{name}/download", post(download_published))
        .route("/prompt-stack", get(prompt_stack_view))
        .route("/prompt-stack/preflight", get(prompt_stack_preflight))
        .route("/prompt-stack/start", post(prompt_stack_start))
        .route("/prompt-stack/stop", post(prompt_stack_stop))
        .route("/remote-miner", get(remote_miner_view))
        .route("/remote-miner/preflight", get(remote_miner_preflight))
        .route("/remote-miner/checkpoint", get(remote_miner_checkpoint))
        .route("/remote-miner/start", post(remote_miner_start))
        .route("/remote-miner/stop", post(remote_miner_stop))
        .route("/remote-miner/log", get(remote_miner_log))
        .route("/producer-key", post(producer_key))
}

/// The whole network picture in one response — what the UI's Network tab renders.
#[derive(Serialize)]
struct NetworkOverview {
    network: NodeNetwork,
    /// The chain as each public node reports it — one node's word apiece (`UNVERIFIED_REMOTE_STATE`).
    public_nodes: Vec<NodeStatus>,
    classes: Vec<PalwClassStatus>,
}

/// Scan the models directory for PALW artifacts.
///
/// The same directory models live in, on purpose: it is the one place users already know. What
/// counts as an artifact is [`palw::is_artifact_filename`], the same question the model scan and
/// the load gate ask — the model list does show artifacts, and it is the gate, not this scan, that
/// keeps one from reaching an inference engine.
async fn artifact_scan(state: &AppState) -> Vec<(String, String, u64)> {
    let dir = state.settings.read().await.models_dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&dir) else { return out };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !palw::is_artifact_filename(&name) {
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
    let public_nodes = crate::node::public_status(settings.node.network).await;
    let artifacts = artifact_scan(&state).await;
    let classes =
        with_artifact_headers(assess_classes(state.settings.read().await.node.network, &artifacts, state.hardware.total_memory));
    Ok(Json(NetworkOverview { network: settings.node.network, public_nodes, classes }))
}

/// Read each present artifact's header (64 bytes apiece), so a class card can say what the file on
/// disk can hold beside what the class was registered at.
fn with_artifact_headers(mut statuses: Vec<PalwClassStatus>) -> Vec<PalwClassStatus> {
    for status in &mut statuses {
        let path = match &status.readiness {
            PalwClassReadiness::ArtifactPresent { path, .. } | PalwClassReadiness::ArtifactMismatch { path, .. } => path,
            _ => continue,
        };
        status.artifact_header = read_artifact_header(std::path::Path::new(path));
    }
    statuses
}

async fn classes(State(state): State<Arc<AppState>>) -> Json<Vec<PalwClassStatus>> {
    let artifacts = artifact_scan(&state).await;
    Json(with_artifact_headers(assess_classes(state.settings.read().await.node.network, &artifacts, state.hardware.total_memory)))
}

/// Download a class artifact into the models directory, verified against the chain-pinned digest.
///
/// Only classes whose artifact is published as a file can be downloaded; a convert-locally
/// class (QWEN36-2M until that file is published) answers 400 carrying the conversion command
/// instead — an error that tells the user the actual next step.
async fn download_artifact(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<crate::download::DownloadProgress>> {
    let spec = classes_for(state.settings.read().await.node.network)
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
            // The class's pinned tokenizer, beside the artifact: the free-prompt worker reads both, and a chat load is no
            // longer the only thing that asks for it. Verified against its pinned digest like the artifact.
            if let (Some(pin), Some(name)) = (spec.tokenizer, spec.tokenizer_filename()) {
                let url = catalog.download_url(pin.hf_repo, "main", pin.repo_path);
                if let Err(e) = state
                    .downloads
                    .fetch(
                        format!("{}/{}", pin.hf_repo, name),
                        pin.hf_repo.to_string(),
                        name.clone(),
                        format!("{} tokenizer", spec.name),
                        url,
                        settings.models_dir.join(&name),
                        Some(pin.sha256.to_string()),
                        Some(pin.size_bytes),
                    )
                    .await
                {
                    tracing::warn!(class = spec.name, "could not fetch the class tokenizer: {e}");
                }
            }
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

/// The default class's artifact in the models directory, when it is there whole — what the free-prompt worker is given.
///
/// Resolved at launch instead of being written into the settings file, because it is a path *under the models directory*:
/// pinning it once would keep naming the old directory the moment someone moves their models. `None` when the file is
/// absent or the wrong size.
pub(crate) async fn default_class_artifact(models_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let spec = misaka_studio_core::palw::default_class();
    let PalwArtifactSource::Download { filename, size_bytes, .. } = &spec.artifact else { return None };
    let path = models_dir.join(filename);
    let meta = tokio::fs::metadata(&path).await.ok()?;
    (meta.len() == *size_bytes).then_some(path)
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_log_limit")]
    limit: usize,
}

fn default_log_limit() -> usize {
    200
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
    // **Never mint a key while the node-free miner signs under the old one**: the seed IS the bond, and a running miner
    // would keep signing under a key the settings no longer name.
    if state.remote_miner.view(&settings.node, &state.data_dir).await.running {
        return Err(Error::bad_request(
            "the node-free miner is running under the current key — stop it before minting another, or its bond becomes unsignable",
        ));
    }
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
        "next": "register a bond under this key without a node (`misaka bond register-export / sign / submit`), then fund the key's address for carrier fees",
    })))
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

    /// An empty models directory must not produce a path.
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

/// `GET /api/v1/network/remote-miner` — the node-free miner: running or not, the command the settings would run, and what
/// its event lines said.
async fn remote_miner_view(State(state): State<Arc<AppState>>) -> Json<crate::remote_miner::RemoteMinerView> {
    let settings = state.settings.read().await.node.clone();
    Json(state.remote_miner.view(&settings, &state.data_dir).await)
}

/// `GET /api/v1/network/remote-miner/preflight` — the published nodes read through their JSON twins, the binaries found or
/// not, and every value the command still lacks. Local observations of remote nodes, not proofs.
async fn remote_miner_preflight(State(state): State<Arc<AppState>>) -> Json<crate::remote_miner::Preflight> {
    let settings = state.settings.read().await.node.clone();
    Json(crate::remote_miner::preflight(&settings, &state.data_dir).await)
}

/// `GET /api/v1/network/remote-miner/checkpoint` — a checkpoint read from one node and confirmed by another. A suggestion:
/// nothing is written; the user saves it into `node.remote.checkpoint` after comparing it.
async fn remote_miner_checkpoint(State(state): State<Arc<AppState>>) -> Result<Json<crate::remote_miner::CheckpointSuggestion>> {
    let settings = state.settings.read().await.node.clone();
    Ok(Json(crate::remote_miner::suggest_checkpoint(&settings).await?))
}

async fn remote_miner_start(State(state): State<Arc<AppState>>) -> Result<Json<crate::remote_miner::RemoteMinerView>> {
    let settings = state.settings.read().await.node.clone();
    Ok(Json(state.remote_miner.start(&settings, &state.data_dir).await?))
}

async fn remote_miner_stop(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    state.remote_miner.stop().await?;
    Ok(Json(serde_json::json!({ "stopped": true })))
}

async fn remote_miner_log(State(state): State<Arc<AppState>>, Query(query): Query<LogQuery>) -> Json<Vec<String>> {
    Json(state.remote_miner.recent_log(query.limit.min(600)))
}

/// `GET /api/v1/network/prompt-stack` — the node-free prompt-mining stack: the gateway and the rail, the commands the
/// settings would run, the carriers the rail built and both logs.
async fn prompt_stack_view(State(state): State<Arc<AppState>>) -> Json<crate::fp_stack::StackView> {
    let settings = state.settings.read().await.clone();
    let artifact = default_class_artifact(&settings.models_dir).await;
    Json(state.prompt_stack.view(&settings.node, &state.data_dir, artifact).await)
}

/// `GET /api/v1/network/prompt-stack/preflight` — binaries, artifact, tokenizer, key, the public node and the evidence
/// provider, each checked from here.
async fn prompt_stack_preflight(State(state): State<Arc<AppState>>) -> Json<crate::fp_stack::StackPreflight> {
    let settings = state.settings.read().await.clone();
    let artifact = default_class_artifact(&settings.models_dir).await;
    Json(crate::fp_stack::preflight(&settings.node, &state.data_dir, artifact).await)
}

async fn prompt_stack_start(State(state): State<Arc<AppState>>) -> Result<Json<crate::fp_stack::StackView>> {
    let settings = state.settings.read().await.clone();
    let artifact = default_class_artifact(&settings.models_dir).await;
    let view = state.prompt_stack.start(&settings.node, &state.data_dir, artifact).await?;
    // **The chat answers through the gateway it just started**: the answer IS the work. Pointed only when nothing else is
    // configured, so a gateway the person chose by hand is never replaced behind their back.
    if settings.node.palw_gateway_url.is_none() {
        let mut next = settings.clone();
        next.node.palw_gateway_url = Some(crate::fp_stack::gateway_url(&settings.node));
        state.apply_settings(next).await?;
    }
    Ok(Json(view))
}

async fn prompt_stack_stop(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    state.prompt_stack.stop().await?;
    // Undo the pointing above: a chat sent to a stopped gateway is a chat that fails.
    let settings = state.settings.read().await.clone();
    let own = format!("http://{}", settings.node.free_prompt.listen);
    if settings.node.palw_gateway_url.as_deref() == Some(own.as_str()) {
        let mut next = settings.clone();
        next.node.palw_gateway_url = None;
        state.apply_settings(next).await?;
    }
    Ok(Json(serde_json::json!({ "stopped": true })))
}

/// One published PALWART3 model and whether all of it is on disk.
#[derive(Serialize)]
struct PublishedView {
    model: &'static palw::PublishedPalwart,
    present: bool,
    /// Where the index lands; `palwart verify` reads its segments beside it.
    index_path: String,
    total_bytes: u64,
    /// Always false today: none of these is a class of the live network, so none mines.
    registered: bool,
}

/// `GET /api/v1/network/published` — PALW models published as PALWART3 that this network has not registered.
async fn published(State(state): State<Arc<AppState>>) -> Json<Vec<PublishedView>> {
    let dir = state.settings.read().await.models_dir.clone();
    Json(
        palw::PUBLISHED_PALWART3
            .iter()
            .map(|m| PublishedView {
                model: m,
                present: palw::published_present(m, &dir),
                index_path: dir.join(m.index.filename()).display().to_string(),
                total_bytes: m.total_bytes(),
                registered: false,
            })
            .collect(),
    )
}

/// `POST /api/v1/network/published/{name}/download` — fetch the index and every segment into the models directory, each
/// pinned to the repository commit and verified against its SHA-256. Returns the index's progress; the segments appear
/// in the download list beside it.
async fn download_published(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<crate::download::DownloadProgress>>> {
    let model = palw::PUBLISHED_PALWART3
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| Error::bad_request(format!("no published PALW model named '{name}'")))?;
    let dir = state.settings.read().await.models_dir.clone();
    let catalog = state.catalog().await;
    let mut out = Vec::new();
    for file in model.files() {
        let url = catalog.download_url(model.hf_repo, model.revision, file.repo_path);
        out.push(
            state
                .downloads
                .fetch(
                    format!("{}/{}", model.hf_repo, file.repo_path),
                    model.hf_repo.to_string(),
                    file.repo_path.to_string(),
                    model.name.to_string(),
                    url,
                    dir.join(file.filename()),
                    Some(file.sha256.to_string()),
                    Some(file.size_bytes),
                )
                .await?,
        );
    }
    Ok(Json(out))
}
