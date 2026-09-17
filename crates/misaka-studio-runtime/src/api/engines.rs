//! `/api/v1/engines`: what `llama-server` this Studio would run, what it can drive, and the
//! builds that could replace it.
//!
//! One document answers the question the Settings page has to put in front of a person whose
//! model is slow: *is the GPU being used, and if not, whose fault is it?* The hardware probe's
//! card, the engine's own device list and the verdict that follows from the two are side by
//! side, with the install that fixes it underneath.

use crate::Result;
use crate::backend::devices::{EngineDevice, first_gpu, probe_program};
use crate::backend::llamacpp::{ProgramSource, resolve_program_with_source};
use crate::engines::{EngineFlavor, InstallStatus, InstalledEngine, flavors_for, recommend};
use crate::state::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(engines)).route("/install", get(install_status).post(install))
}

/// What the resolved engine can do, in one word the UI colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The engine lists a device it can put layers on.
    Gpu,
    /// The engine runs and lists no GPU device: a CPU-only build, or no driver for its backend.
    CpuOnly,
    /// The engine runs but is too old to list its devices.
    Unknown,
    /// No engine could be run.
    Missing,
}

#[derive(Serialize)]
struct FlavorView {
    id: &'static str,
    label: &'static str,
    accelerator: &'static str,
    requires: &'static str,
    /// Bytes to download, main asset and companions together, when the release was reachable.
    download_bytes: Option<u64>,
    recommended: bool,
}

#[derive(Serialize)]
struct ReleaseView {
    tag: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct EnginesView {
    /// The binary a load would run, and how it was found.
    program: String,
    source: ProgramSource,
    version: Option<String>,
    /// Why it could not be run, when it could not.
    error: Option<String>,
    devices: Option<Vec<EngineDevice>>,
    verdict: Verdict,
    /// One sentence the page can show as the verdict.
    summary: String,
    /// The accelerator the hardware probe found, if any — the card the engine may be ignoring.
    hardware_gpu: Option<String>,
    flavors: Vec<FlavorView>,
    /// Why the recommended flavour is the recommended one.
    recommendation: Option<String>,
    release: ReleaseView,
    install: InstallStatus,
    installed: Vec<InstalledEngine>,
}

async fn engines(State(state): State<Arc<AppState>>) -> Json<EnginesView> {
    let settings = state.settings.read().await.clone();
    let (program, source) = resolve_program_with_source(settings.backend.llama_server_path.clone());
    let probe = probe_program(&program).await;

    let hardware_gpu = state
        .hardware
        .accelerators
        .iter()
        .find(|a| a.kind != misaka_studio_core::hardware::AcceleratorKind::Cpu)
        .map(|a| a.name.clone());

    let (verdict, summary) = match (&probe.banner, probe.devices.as_deref()) {
        (None, _) => (
            Verdict::Missing,
            format!("No llama-server could be run ({}). Install one below.", probe.error.as_deref().unwrap_or("not found")),
        ),
        (Some(_), Some(list)) => match first_gpu(list) {
            Some(gpu) => (
                Verdict::Gpu,
                format!(
                    "GPU ready: {}{}. Layers you offload go there.",
                    gpu.label(),
                    gpu.free_mib.map(|m| format!(", {:.1} GB free", m as f64 / 1024.0)).unwrap_or_default()
                ),
            ),
            None => (
                Verdict::CpuOnly,
                match &hardware_gpu {
                    Some(card) => format!(
                        "This llama-server lists no GPU device, so every model runs on the CPU — the {card} in this machine is idle. Install a build that drives it below."
                    ),
                    None => "This llama-server lists no GPU device, so every model runs on the CPU. If this machine has a GPU the Studio could not detect, a Vulkan build below will find it.".to_string(),
                },
            ),
        },
        (Some(_), None) => (
            Verdict::Unknown,
            "This llama-server is too old to list its devices, so whether it uses the GPU cannot be verified. Install a current build below.".to_string(),
        ),
    };

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let recommended = recommend(&state.hardware, os, arch);
    let (release, listing) = match state.engines.release(None).await {
        Ok(listing) => (ReleaseView { tag: Some(listing.tag.clone()), error: None }, Some(listing)),
        Err(e) => (ReleaseView { tag: None, error: Some(e.to_string()) }, None),
    };
    let size_of = |flavor: &EngineFlavor| -> Option<u64> {
        let listing = listing.as_ref()?;
        let main = listing.asset(&flavor.asset.replace("{tag}", &listing.tag))?.size;
        let companions = flavor
            .companions
            .iter()
            .map(|c| listing.asset(&c.replace("{tag}", &listing.tag)).map(|a| a.size))
            .collect::<Option<Vec<_>>>()?;
        Some(main + companions.iter().sum::<u64>())
    };
    let flavors = flavors_for(os, arch)
        .into_iter()
        .map(|f| FlavorView {
            id: f.id,
            label: f.label,
            accelerator: f.accelerator,
            requires: f.requires,
            download_bytes: size_of(f),
            recommended: recommended.as_ref().is_some_and(|(r, _)| r.id == f.id),
        })
        .collect();

    Json(EnginesView {
        program: program.display().to_string(),
        source,
        version: probe.version.clone(),
        error: probe.error.clone(),
        devices: probe.devices.clone(),
        verdict,
        summary,
        hardware_gpu,
        flavors,
        recommendation: recommended.map(|(_, why)| why),
        release,
        install: state.engines.status().await,
        installed: state.engines.installed().await,
    })
}

async fn install_status(State(state): State<Arc<AppState>>) -> Json<InstallStatus> {
    Json(state.engines.status().await)
}

#[derive(Deserialize)]
struct InstallRequest {
    flavor: String,
    /// A `b<build>` tag to pin, instead of the release upstream's latest points at.
    #[serde(default)]
    tag: Option<String>,
}

async fn install(State(state): State<Arc<AppState>>, Json(body): Json<InstallRequest>) -> Result<Json<InstallStatus>> {
    let status = state.engines.install(state.clone(), &body.flavor, body.tag).await?;
    Ok(Json(status))
}
