//! `/api/v1/components` — every binary and artifact the Studio can spawn or map, held to the
//! components manifest (ADR-0096 Decision 10), and installed from it.
//!
//! The table is `crate::components::report`; this module decides when the manifest is fetched
//! (`components.auto_check`, or `?check=1`), whether the expensive checks run (`?verify=1`
//! hashes every found file and asks every binary for `--version`), and hands an install to the
//! download manager, which verifies the bytes against the row's digest and size the way it
//! verifies a model.

use crate::components::{ComponentId, Finding, InstallSource, Manifest, check_spawnable_ids, install_plan, load_manifest, report};
use crate::download::DownloadProgress;
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(list)).route("/{id}/install", post(install))
}

#[derive(Deserialize)]
struct ListQuery {
    /// Hash every found file and ask every binary for `--version`. Off by default: a class
    /// artifact is 34 GiB, and a listing must not cost a minute of disk.
    #[serde(default)]
    verify: Option<String>,
    /// Fetch the manifest even when `components.auto_check` is off.
    #[serde(default)]
    check: Option<String>,
}

fn flag(value: &Option<String>) -> bool {
    matches!(value.as_deref(), Some("1") | Some("true") | Some("yes"))
}

/// The manifest as this request saw it.
#[derive(Clone, Debug, Serialize)]
pub struct ManifestStatus {
    /// `components.manifest`, as configured.
    pub source: Option<String>,
    pub release: Option<String>,
    pub network: Option<String>,
    pub loaded: bool,
    /// Why it is not loaded, when it is not: unset, switched off, unreachable, or refused by the
    /// validator (every finding, one per line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The cross-repository check over the loaded manifest (ADR-0096 invariant 10).
    pub findings: Vec<Finding>,
}

#[derive(Serialize)]
pub struct ComponentsView {
    pub components: Vec<crate::components::ComponentReport>,
    pub manifest: ManifestStatus,
    /// The triple this runtime was built for, so a row's `platform` can be read against it.
    pub host_platform: &'static str,
}

/// Load the configured manifest, or say why not. `force` is a person asking (`?check=1`, an
/// install), which overrides `auto_check` — the setting is about what the Studio does on its
/// own, not about refusing a request.
pub async fn load_configured_manifest(state: &AppState, force: bool) -> (Option<Manifest>, ManifestStatus) {
    let settings = state.settings.read().await.clone();
    let mut status = ManifestStatus {
        source: settings.components.manifest.clone(),
        release: None,
        network: None,
        loaded: false,
        error: None,
        findings: Vec::new(),
    };
    let Some(source) = settings.components.manifest.clone() else {
        status.error = Some("components.manifest is not set; everything found on disk is `not-in-manifest`".into());
        return (None, status);
    };
    if !settings.components.auto_check && !force {
        status.error = Some("components.auto_check is off; pass ?check=1 to read the manifest for this request".into());
        return (None, status);
    }
    let catalog = state.catalog().await;
    match load_manifest(&source, &catalog).await {
        Ok(manifest) => {
            status.release = Some(manifest.release.clone());
            status.network = Some(manifest.network.clone());
            status.loaded = true;
            status.findings = check_spawnable_ids(&manifest);
            (Some(manifest), status)
        }
        Err(e) => {
            status.error = Some(e.to_string());
            (None, status)
        }
    }
}

async fn list(State(state): State<Arc<AppState>>, Query(query): Query<ListQuery>) -> Json<ComponentsView> {
    let (manifest, status) = load_configured_manifest(&state, flag(&query.check)).await;
    let settings = state.settings.read().await.clone();
    let components = report(&settings, manifest.as_ref(), flag(&query.verify)).await;
    Json(ComponentsView { components, manifest: status, host_platform: crate::components::HOST_PLATFORM })
}

/// Start installing one component from its manifest row.
///
/// Refusals come back as 400 with the sentence from `install_plan` — a retired id, a row for
/// another platform, an archive member (not extracted yet, by design; see `crate::components`).
/// The download itself is a resource: watch `/api/v1/downloads/stream`, cancel it under
/// `/api/v1/downloads/{id}`, like any model download.
async fn install(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Result<Json<DownloadProgress>> {
    let component = ComponentId::parse(&id).ok_or_else(|| Error::bad_request(format!("no component is called '{id}'")))?;
    let (manifest, status) = load_configured_manifest(&state, true).await;
    let Some(manifest) = manifest else {
        return Err(Error::bad_request(format!(
            "no manifest to install from: {}",
            status.error.unwrap_or_else(|| "components.manifest is not set".into())
        )));
    };
    let row =
        manifest.row(&id).ok_or_else(|| Error::bad_request(format!("the manifest ({}) has no row for {id}", manifest.release)))?;
    let settings = state.settings.read().await.clone();
    let plan = install_plan(&component, row, &settings).map_err(Error::bad_request)?;
    let catalog = state.catalog().await;
    let store = component.is_artifact().then(|| state.store.clone());
    let progress = match plan.source {
        // An artifact from the hub takes the path the class download already takes: the same
        // sidecar, the same rescan, the same `main` revision pinned by the digest.
        InstallSource::Hub { repo, path } if component.is_artifact() => {
            state
                .downloads
                .start(
                    &catalog,
                    state.store.clone(),
                    settings.models_dir.clone(),
                    repo,
                    "main".into(),
                    path,
                    Some(plan.sha256),
                    Some(plan.size),
                    None,
                )
                .await?
        }
        InstallSource::Hub { repo, path } => {
            let url = catalog.download_url(&repo, "main", &path);
            state.downloads.start_component(url, plan.destination, plan.sha256, plan.size, plan.executable, store).await?
        }
        InstallSource::Https(url) => {
            state.downloads.start_component(url, plan.destination, plan.sha256, plan.size, plan.executable, store).await?
        }
    };
    Ok(Json(progress))
}
