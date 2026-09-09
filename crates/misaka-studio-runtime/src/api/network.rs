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
use misaka_studio_core::HardwareSnapshot;
use misaka_studio_core::hardware::AcceleratorKind;
use misaka_studio_core::palw;
use misaka_studio_core::palw::{PalwArtifactSource, PalwClassReadiness, PalwClassStatus, TESTNET11_CLASSES, assess_classes};
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
        .route("/blocks", get(produced_blocks))
        .route("/producer-key", post(producer_key))
        .route("/faucet", post(super::pool::faucet_for_address))
        .route("/model-request", get(model_request))
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
    // **Disarm before the node can succeed at it, not after.** A class registration is one
    // transaction; the flag that files it must not survive into a second start. Written back
    // ahead of the launch because a start that half-fails still ran the node, and a flag cleared
    // only on the success path is a flag that files a second registration after a crash.
    if crate::node::NodeManager::start_would_register_class(&node_settings) {
        let mut next = settings.clone();
        next.node.register_class = None;
        state.apply_settings(next).await?;
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

/// `GET /api/v1/network/blocks` — **the blocks this machine has produced**, newest first.
///
/// Its own route rather than a field on the overview: each row costs a `getBlock` against the
/// node, and the overview is polled every few seconds by every open tab. The explorer card asks
/// for this when it is looked at.
pub async fn produced_blocks(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>> {
    let settings = state.settings.read().await.node.clone();
    let blocks = state.node.produced_blocks(&settings, 50).await;
    Ok(Json(serde_json::json!({ "blocks": blocks })))
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
    // **Never mint a key while a node is running under the old one.** The seed IS the bond: a
    // supervised producer signs its attempts with the key it started with, and a bond registered
    // by that key cannot be signed for by any other. Minting here would leave the running node
    // producing under a key the settings no longer name, and the failure surfaces much later as
    // the node's own "the local signing key is not the one this bond registered" — which is
    // exactly what a rehearsal hit on 2026-09-04 after its data directory was recreated under a
    // still-running daemon.
    if state.node.is_supervising().await {
        return Err(Error::bad_request(
            "a node is running under the current producer key — stop it before minting another, or its bond becomes unsignable",
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
        "next": "start the node as a producer: it registers a bond under this key and prints the address to fund",
    })))
}

// --- Decision 13: the door for asking for a model -----------------------------------------------

/// Where a model request goes: the node repository's issue form (ADR-0096 Decision 13).
const MODEL_REQUEST_FORM: &str = "https://github.com/MISAKA-BTC/misakas/issues/new";
const MODEL_REQUEST_TEMPLATE: &str = "model-request.yml";

/// `GET /api/v1/network/model-request` — **the door for asking for a model** (ADR-0096 Decision 13).
///
/// The URL of the node repository's model-request issue form, prefilled with what this machine
/// knows about itself — RAM, accelerator, platform, the class artifacts on disk, the Studio's
/// version — and the fields exactly as they were put into the URL, so the UI can show a person
/// what it is about to send before a browser opens. The questions the form asks (weights, licence,
/// architecture, parameters, quantization, context, lanes, who converts, who bonds) are left
/// blank: they are the person's to answer. A request is public, buys nothing and moves no
/// consensus object — the day one becomes a line, its owner is whoever bonded it (ADR-0088).
async fn model_request(State(state): State<Arc<AppState>>) -> Json<ModelRequestDoor> {
    let artifacts = artifact_scan(&state).await;
    Json(model_request_door(&state.hardware, &artifacts))
}

#[derive(Debug, Serialize)]
pub struct ModelRequestDoor {
    /// The form, prefilled. Opening it is the browser's job, and the person's click.
    pub url: String,
    /// The query, field by field — the form's own ids, plus `title`.
    pub fields: ModelRequestFields,
    /// The facts behind `fields.machine`, structured.
    pub machine: MachineFacts,
}

/// The issue form's fields, by the ids `.github/ISSUE_TEMPLATE/model-request.yml` declares. Every
/// field is in the URL, empty ones included, so what the form shows is what this says.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequestFields {
    pub title: String,
    pub weights: String,
    pub license: String,
    pub architecture: String,
    pub params: String,
    pub quantization: String,
    pub context: String,
    pub lanes: String,
    pub converter: String,
    pub bonder: String,
    pub machine: String,
}

impl ModelRequestFields {
    /// In the form's order, `title` first — the order the URL carries them.
    pub fn pairs(&self) -> [(&'static str, &str); 11] {
        [
            ("title", &self.title),
            ("weights", &self.weights),
            ("license", &self.license),
            ("architecture", &self.architecture),
            ("params", &self.params),
            ("quantization", &self.quantization),
            ("context", &self.context),
            ("lanes", &self.lanes),
            ("converter", &self.converter),
            ("bonder", &self.bonder),
            ("machine", &self.machine),
        ]
    }
}

/// What the machine knows about itself, as the request carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineFacts {
    pub ram_gib: f64,
    /// `name (kind, memory)` of the first accelerator that is not the CPU, or `none`.
    pub accelerator: String,
    /// `os/arch`, as the hardware probe spells them.
    pub platform: String,
    /// The class artifacts in the models directory, each with the class that claims it.
    pub classes_on_disk: Vec<String>,
    pub studio_version: String,
}

impl MachineFacts {
    /// The one text blob the form's `machine` field holds.
    pub fn as_text(&self) -> String {
        let classes = if self.classes_on_disk.is_empty() { "none".to_string() } else { self.classes_on_disk.join("; ") };
        format!(
            "RAM: {:.1} GiB\nAccelerator: {}\nPlatform: {}\nClasses on disk: {}\nStudio: {}",
            self.ram_gib, self.accelerator, self.platform, classes, self.studio_version
        )
    }
}

fn model_request_door(hardware: &HardwareSnapshot, artifacts: &[(String, String, u64)]) -> ModelRequestDoor {
    let machine = machine_facts(hardware, artifacts);
    let fields = ModelRequestFields { title: "Model request: ".to_string(), machine: machine.as_text(), ..Default::default() };
    ModelRequestDoor { url: model_request_url(&fields), fields, machine }
}

fn gib(bytes: u64) -> f64 {
    (bytes as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0
}

fn accelerator_label(kind: &AcceleratorKind) -> &'static str {
    match kind {
        AcceleratorKind::AppleUnified => "unified memory",
        AcceleratorKind::Cuda => "CUDA",
        AcceleratorKind::Rocm => "ROCm",
        AcceleratorKind::Vulkan => "Vulkan",
        AcceleratorKind::Cpu => "CPU",
    }
}

fn file_name(path: &str) -> &str {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path)
}

fn machine_facts(hardware: &HardwareSnapshot, artifacts: &[(String, String, u64)]) -> MachineFacts {
    let accelerator = hardware
        .accelerators
        .iter()
        .find(|a| a.kind != AcceleratorKind::Cpu)
        .map(|a| match a.total_memory {
            Some(bytes) => format!("{} ({}, {:.1} GiB)", a.name, accelerator_label(&a.kind), gib(bytes)),
            None => format!("{} ({})", a.name, accelerator_label(&a.kind)),
        })
        .unwrap_or_else(|| "none".to_string());

    // The same assessment the Network tab shows, so the request names the class the Studio
    // recognises the file as — and says so when the file is one the registry does not know.
    let mut classes_on_disk = Vec::new();
    let mut claimed = std::collections::HashSet::new();
    for status in assess_classes(artifacts, hardware.total_memory) {
        match &status.readiness {
            PalwClassReadiness::ArtifactPresent { path, size_bytes, .. } => {
                claimed.insert(path.clone());
                classes_on_disk.push(format!("{}: {} ({:.1} GiB)", status.spec.name, file_name(path), gib(*size_bytes)));
            }
            PalwClassReadiness::ArtifactMismatch { path, size_bytes, expected_bytes } => {
                claimed.insert(path.clone());
                classes_on_disk.push(format!(
                    "{}: {} ({:.1} GiB on disk, {:.1} GiB expected — size mismatch)",
                    status.spec.name,
                    file_name(path),
                    gib(*size_bytes),
                    gib(*expected_bytes)
                ));
            }
            PalwClassReadiness::ReadyBuiltIn | PalwClassReadiness::ArtifactMissing { .. } => {}
        }
    }
    for (path, name, size) in artifacts {
        if !claimed.contains(path) {
            classes_on_disk.push(format!("{name} ({:.1} GiB, no registered class)", gib(*size)));
        }
    }

    MachineFacts {
        ram_gib: gib(hardware.total_memory),
        accelerator,
        platform: format!("{}/{}", hardware.os, hardware.arch),
        classes_on_disk,
        studio_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// The form's URL with every field in the query. GitHub prefills an issue form from query
/// parameters named by the form's field ids, `title` and `template` included.
fn model_request_url(fields: &ModelRequestFields) -> String {
    let mut url = format!("{MODEL_REQUEST_FORM}?template={}", percent_encode(MODEL_REQUEST_TEMPLATE));
    for (key, value) in fields.pairs() {
        url.push('&');
        url.push_str(key);
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    url
}

/// RFC 3986 percent-encoding of everything outside the unreserved set. Hand-rolled because the
/// runtime has no direct dependency on a URL crate, and the whole of what it needs is this: a byte
/// is either one of `A-Z a-z 0-9 - _ . ~` or it is `%XX`.
fn percent_encode(input: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod model_request_tests {
    use super::*;
    use misaka_studio_core::hardware::Accelerator;
    use misaka_studio_core::palw::default_class;
    use std::collections::BTreeMap;

    const GIB: u64 = 1 << 30;

    fn hardware(accelerators: Vec<Accelerator>) -> HardwareSnapshot {
        HardwareSnapshot {
            os: "macOS 15.6".into(),
            arch: "aarch64".into(),
            cpu_name: "Apple M2".into(),
            physical_cores: Some(8),
            logical_cores: 8,
            total_memory: 24 * GIB,
            available_memory: 8 * GIB,
            accelerators,
        }
    }

    fn accelerator(kind: AcceleratorKind) -> Accelerator {
        Accelerator {
            kind,
            name: "Apple M2".into(),
            total_memory: Some(24 * GIB),
            free_memory: None,
            usable_memory: Some(18 * GIB),
            driver: None,
            index: 0,
        }
    }

    fn percent_decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("two hex digits");
                    out.push(u8::from_str_radix(hex, 16).expect("hex"));
                    i += 3;
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                byte => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8(out).expect("utf-8")
    }

    fn query_of(url: &str) -> (String, BTreeMap<String, String>) {
        let (base, query) = url.split_once('?').expect("a query");
        let pairs = query
            .split('&')
            .map(|kv| {
                let (key, value) = kv.split_once('=').expect("key=value");
                (percent_decode(key), percent_decode(value))
            })
            .collect();
        (base.to_string(), pairs)
    }

    #[test]
    fn the_url_parses_back_to_the_fields() {
        let (filename, size) = match &default_class().artifact {
            PalwArtifactSource::Download { filename, size_bytes, .. } => (filename.to_string(), *size_bytes),
            other => panic!("the default class must publish an artifact, got {other:?}"),
        };
        let artifacts = vec![
            (format!("/models/{filename}"), filename.clone(), size),
            ("/models/mystery.palwx".into(), "mystery.palwx".into(), 3 * GIB),
        ];
        let door = model_request_door(&hardware(vec![accelerator(AcceleratorKind::AppleUnified)]), &artifacts);

        assert!(door.url.is_ascii() && !door.url.contains(char::is_whitespace), "{}", door.url);
        let (base, query) = query_of(&door.url);
        assert_eq!(base, MODEL_REQUEST_FORM);
        assert_eq!(query.get("template").map(String::as_str), Some(MODEL_REQUEST_TEMPLATE));
        for (key, value) in door.fields.pairs() {
            assert_eq!(query.get(key).map(String::as_str), Some(value), "field {key}");
        }
        assert_eq!(query.len(), 12, "template, title and the ten form ids: {query:?}");

        assert_eq!(door.fields.title, "Model request: ");
        assert!(door.fields.weights.is_empty() && door.fields.bonder.is_empty(), "the person's questions are left blank");
        let machine = &door.fields.machine;
        assert!(machine.contains("RAM: 24.0 GiB\n"), "{machine}");
        assert!(machine.contains("Accelerator: Apple M2 (unified memory, 24.0 GiB)\n"), "{machine}");
        assert!(machine.contains("Platform: macOS 15.6/aarch64\n"), "{machine}");
        assert!(machine.contains(&format!("{}: {filename} (", default_class().name)), "{machine}");
        assert!(machine.contains("mystery.palwx (3.0 GiB, no registered class)"), "{machine}");
        assert!(machine.ends_with(&format!("Studio: {}", env!("CARGO_PKG_VERSION"))), "{machine}");
        assert_eq!(door.machine.classes_on_disk.len(), 2);
        assert_eq!(door.machine.ram_gib, 24.0);
    }

    #[test]
    fn a_machine_with_nothing_says_none() {
        let door = model_request_door(&hardware(vec![accelerator(AcceleratorKind::Cpu)]), &[]);
        assert_eq!(door.machine.accelerator, "none", "the CPU is not an accelerator");
        assert!(door.machine.classes_on_disk.is_empty());
        assert!(door.fields.machine.contains("Accelerator: none\n"), "{}", door.fields.machine);
        assert!(door.fields.machine.contains("Classes on disk: none\n"), "{}", door.fields.machine);
    }

    #[test]
    fn percent_encoding_covers_the_reserved_set() {
        assert_eq!(percent_encode("a b&c=d/é\n~-_."), "a%20b%26c%3Dd%2F%C3%A9%0A~-_.");
        assert_eq!(percent_encode(""), "");
        let text = "RAM: 24.0 GiB\nAccelerator: none 100% #1 ?q=1+1";
        assert_eq!(percent_decode(&percent_encode(text)), text);
    }
}
