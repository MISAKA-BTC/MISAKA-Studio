//! Getting an engine that can use the GPU — from inside the app.
//!
//! The Studio does not bundle `llama-server`, and "install llama.cpp so it is on PATH" sends a
//! person to a package repository, where the build is CPU-only far more often than not. A tester
//! then chats at CPU speed under a model bar that could have said `28/28 on GPU`, and reports that
//! the Studio "does not use the GPU". It never did; nothing told it how.
//!
//! Upstream publishes a build per accelerator with every release — `macos-arm64` (Metal),
//! `win-cuda-12.4-x64`, `ubuntu-vulkan-x64`, `win-rocm-10.0-x64` and so on — with a SHA-256 for
//! each asset in the release API. This module picks the one for this machine, downloads it
//! through the same resumable, verified pipeline models use, unpacks it under the data directory,
//! asks the binary what it can drive, and points `backend.llama_server_path` at it. The last step
//! is what makes it real: after it, "GPU offload: Auto" plans for a device the engine actually has.
//!
//! # What is not guessed
//!
//! * **Which release.** `releases/latest` is upstream's versioned release, and it carries a
//!   `nightly-tag.txt` naming the `b<build>` release whose assets it means. That pointer is
//!   followed; when there is none, the newest `b<build>` release is taken. A pinned tag can be
//!   given instead.
//! * **Whether it worked.** The installed binary is run (`--version`, `--list-devices`) before
//!   the setting changes. A tarball that unpacks and does not run is a failure with the binary's
//!   own words in it, not a setting pointing at a broken file.
//! * **Whether it uses the GPU.** The device list the binary gives is reported as the outcome. A
//!   Vulkan build on a machine with no Vulkan driver lists nothing, and says so.
//!
//! The Windows CUDA builds need the CUDA runtime DLLs beside them (upstream ships those as a
//! separate `cudart-…` archive), so a flavour may name companions; they are unpacked into the
//! binary's directory. On Linux the CUDA runtime is a system install (`libcudart`), which the
//! Vulkan build sidesteps — hence Vulkan is the default there for every card.

use crate::backend::devices::{EngineDevice, EngineProbe, probe_program};
use crate::backend::llamacpp::engine_file_name;
use crate::download::DownloadStatus;
use crate::state::AppState;
use crate::{Error, Result};
use misaka_studio_core::HardwareSnapshot;
use misaka_studio_core::hardware::AcceleratorKind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// Overrides the release API base (`https://api.github.com/repos/ggml-org/llama.cpp`). For tests
/// and for mirrors.
pub const RELEASES_API_ENV: &str = "MISAKA_STUDIO_LLAMACPP_RELEASES_API";
const DEFAULT_RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp";
/// How long a fetched release listing is reused before asking again.
const LISTING_TTL: Duration = Duration::from_secs(600);

/// One upstream build variant, as published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EngineFlavor {
    /// Stable id, also the directory name: `ubuntu-vulkan-x64`.
    pub id: &'static str,
    pub label: &'static str,
    /// The accelerator tag this build drives: `metal`, `cuda`, `vulkan`, `rocm`, `opencl`, `cpu`.
    pub accelerator: &'static str,
    /// The asset name, with `{tag}` for the release tag.
    pub asset: &'static str,
    /// Archives unpacked beside the binary — the CUDA runtime for a Windows CUDA build.
    pub companions: &'static [&'static str],
    /// What the machine must already have for this build to find its device.
    pub requires: &'static str,
}

const MACOS_ARM64: EngineFlavor = EngineFlavor {
    id: "macos-arm64",
    label: "Apple Silicon — Metal",
    accelerator: "metal",
    asset: "llama-{tag}-bin-macos-arm64.tar.gz",
    companions: &[],
    requires: "macOS on Apple Silicon. Metal is built in.",
};
const MACOS_X64: EngineFlavor = EngineFlavor {
    id: "macos-x64",
    label: "Intel Mac — CPU",
    accelerator: "cpu",
    asset: "llama-{tag}-bin-macos-x64.tar.gz",
    companions: &[],
    requires: "Nothing. Upstream publishes no GPU build for Intel Macs.",
};
const UBUNTU_VULKAN_X64: EngineFlavor = EngineFlavor {
    id: "ubuntu-vulkan-x64",
    label: "Vulkan — any GPU (NVIDIA, AMD, Intel)",
    accelerator: "vulkan",
    asset: "llama-{tag}-bin-ubuntu-vulkan-x64.tar.gz",
    companions: &[],
    requires: "A Vulkan driver: the vendor's, or `mesa-vulkan-drivers`. Without one it runs on the CPU and says so.",
};
const UBUNTU_CUDA_12_X64: EngineFlavor = EngineFlavor {
    id: "ubuntu-cuda-12.8-x64",
    label: "NVIDIA — CUDA 12.8",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-ubuntu-cuda-12.8-x64.tar.gz",
    companions: &["cudart-llama-{tag}-bin-ubuntu-cuda-12.8-x64.tar.gz"],
    requires: "An NVIDIA driver of 525 or newer. The CUDA runtime comes with the download (large).",
};
const UBUNTU_CUDA_13_X64: EngineFlavor = EngineFlavor {
    id: "ubuntu-cuda-13.3-x64",
    label: "NVIDIA — CUDA 13.3",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-ubuntu-cuda-13.3-x64.tar.gz",
    companions: &["cudart-llama-{tag}-bin-ubuntu-cuda-13.3-x64.tar.gz"],
    requires: "An NVIDIA driver of 580 or newer. The CUDA runtime comes with the download (large).",
};
const UBUNTU_ROCM_X64: EngineFlavor = EngineFlavor {
    id: "ubuntu-rocm-10.0-x64",
    label: "AMD — ROCm 10.0",
    accelerator: "rocm",
    asset: "llama-{tag}-bin-ubuntu-rocm-10.0-x64.tar.gz",
    companions: &[],
    requires: "ROCm 10 installed, with a supported Radeon card. Otherwise the Vulkan build.",
};
const UBUNTU_CPU_X64: EngineFlavor = EngineFlavor {
    id: "ubuntu-x64",
    label: "CPU only",
    accelerator: "cpu",
    asset: "llama-{tag}-bin-ubuntu-x64.tar.gz",
    companions: &[],
    requires: "Nothing.",
};
const UBUNTU_VULKAN_ARM64: EngineFlavor = EngineFlavor {
    id: "ubuntu-vulkan-arm64",
    label: "Vulkan — any GPU",
    accelerator: "vulkan",
    asset: "llama-{tag}-bin-ubuntu-vulkan-arm64.tar.gz",
    companions: &[],
    requires: "A Vulkan driver.",
};
const UBUNTU_CUDA_13_ARM64: EngineFlavor = EngineFlavor {
    id: "ubuntu-cuda-13.3-arm64",
    label: "NVIDIA — CUDA 13.3 (Jetson, DGX Spark)",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-ubuntu-cuda-13.3-arm64.tar.gz",
    companions: &["cudart-llama-{tag}-bin-ubuntu-cuda-13.3-arm64.tar.gz"],
    requires: "An NVIDIA driver of 580 or newer.",
};
const UBUNTU_CPU_ARM64: EngineFlavor = EngineFlavor {
    id: "ubuntu-arm64",
    label: "CPU only",
    accelerator: "cpu",
    asset: "llama-{tag}-bin-ubuntu-arm64.tar.gz",
    companions: &[],
    requires: "Nothing.",
};
const WIN_CUDA_12_X64: EngineFlavor = EngineFlavor {
    id: "win-cuda-12.4-x64",
    label: "NVIDIA — CUDA 12.4",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-win-cuda-12.4-x64.zip",
    companions: &["cudart-llama-bin-win-cuda-12.4-x64.zip"],
    requires: "An NVIDIA driver of 527 or newer. The CUDA runtime DLLs come with the download (large).",
};
const WIN_CUDA_13_X64: EngineFlavor = EngineFlavor {
    id: "win-cuda-13.4-x64",
    label: "NVIDIA — CUDA 13.4",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-win-cuda-13.4-x64.zip",
    companions: &["cudart-llama-bin-win-cuda-13.4-x64.zip"],
    requires: "An NVIDIA driver of 580 or newer. The CUDA runtime DLLs come with the download (large).",
};
const WIN_VULKAN_X64: EngineFlavor = EngineFlavor {
    id: "win-vulkan-x64",
    label: "Vulkan — any GPU (NVIDIA, AMD, Intel)",
    accelerator: "vulkan",
    asset: "llama-{tag}-bin-win-vulkan-x64.zip",
    companions: &[],
    requires: "A current graphics driver; every vendor's installs Vulkan. Without one it runs on the CPU and says so.",
};
const WIN_ROCM_X64: EngineFlavor = EngineFlavor {
    id: "win-rocm-10.0-x64",
    label: "AMD — ROCm 10.0",
    accelerator: "rocm",
    asset: "llama-{tag}-bin-win-rocm-10.0-x64.zip",
    companions: &[],
    requires: "AMD's HIP SDK 10 and a supported Radeon card. Otherwise the Vulkan build.",
};
const WIN_CPU_X64: EngineFlavor = EngineFlavor {
    id: "win-cpu-x64",
    label: "CPU only",
    accelerator: "cpu",
    asset: "llama-{tag}-bin-win-cpu-x64.zip",
    companions: &[],
    requires: "Nothing.",
};
const WIN_CUDA_13_ARM64: EngineFlavor = EngineFlavor {
    id: "win-cuda-13.4-arm64",
    label: "NVIDIA — CUDA 13.4",
    accelerator: "cuda",
    asset: "llama-{tag}-bin-win-cuda-13.4-arm64.zip",
    companions: &["cudart-llama-bin-win-cuda-13.4-arm64.zip"],
    requires: "An NVIDIA driver of 580 or newer.",
};
const WIN_OPENCL_ADRENO_ARM64: EngineFlavor = EngineFlavor {
    id: "win-opencl-adreno-arm64",
    label: "Qualcomm Adreno — OpenCL",
    accelerator: "opencl",
    asset: "llama-{tag}-bin-win-opencl-adreno-arm64.zip",
    companions: &[],
    requires: "A Snapdragon machine with Qualcomm's OpenCL driver.",
};
const WIN_CPU_ARM64: EngineFlavor = EngineFlavor {
    id: "win-cpu-arm64",
    label: "CPU only",
    accelerator: "cpu",
    asset: "llama-{tag}-bin-win-cpu-arm64.zip",
    companions: &[],
    requires: "Nothing.",
};

/// The builds upstream publishes for a platform, GPU options first. Empty for a platform it does
/// not publish for.
pub fn flavors_for(os: &str, arch: &str) -> Vec<&'static EngineFlavor> {
    match (os, arch) {
        ("macos", "aarch64") => vec![&MACOS_ARM64],
        ("macos", "x86_64") => vec![&MACOS_X64],
        ("linux", "x86_64") => vec![&UBUNTU_VULKAN_X64, &UBUNTU_CUDA_12_X64, &UBUNTU_CUDA_13_X64, &UBUNTU_ROCM_X64, &UBUNTU_CPU_X64],
        ("linux", "aarch64") => vec![&UBUNTU_VULKAN_ARM64, &UBUNTU_CUDA_13_ARM64, &UBUNTU_CPU_ARM64],
        ("windows", "x86_64") => vec![&WIN_CUDA_12_X64, &WIN_CUDA_13_X64, &WIN_VULKAN_X64, &WIN_ROCM_X64, &WIN_CPU_X64],
        ("windows", "aarch64") => vec![&WIN_CUDA_13_ARM64, &WIN_OPENCL_ADRENO_ARM64, &WIN_CPU_ARM64],
        _ => Vec::new(),
    }
}

pub fn flavor(id: &str) -> Option<&'static EngineFlavor> {
    flavors_for(std::env::consts::OS, std::env::consts::ARCH).into_iter().find(|f| f.id == id)
}

/// The build to suggest for this machine, and the sentence that says why.
///
/// NVIDIA with a driver new enough gets CUDA (13 from driver 580, else 12); AMD with ROCm's tool
/// present gets ROCm; everything else on Linux and Windows gets Vulkan, which finds cards the
/// vendor tools cannot see and costs nothing when there is none. Apple Silicon has one build.
pub fn recommend(hardware: &HardwareSnapshot, os: &str, arch: &str) -> Option<(&'static EngineFlavor, String)> {
    let flavors = flavors_for(os, arch);
    let pick = |id: &str| flavors.iter().copied().find(|f| f.id == id);
    let nvidia = hardware.accelerators.iter().find(|a| a.kind == AcceleratorKind::Cuda);
    let amd_rocm = hardware.accelerators.iter().find(|a| a.kind == AcceleratorKind::Rocm);

    if os == "macos" {
        let f = flavors.first().copied()?;
        let why = if arch == "aarch64" {
            "Apple Silicon: Metal drives the unified memory pool.".to_string()
        } else {
            "An Intel Mac: upstream publishes a CPU build only.".to_string()
        };
        return Some((f, why));
    }
    if let Some(card) = nvidia {
        let major = card.driver.as_deref().and_then(|d| d.split('.').next()).and_then(|m| m.parse::<u32>().ok());
        let (want, generation) = match major {
            Some(m) if m >= 580 => {
                (["ubuntu-cuda-13.3-x64", "win-cuda-13.4-x64", "ubuntu-cuda-13.3-arm64", "win-cuda-13.4-arm64"], "13")
            }
            _ => (["ubuntu-cuda-12.8-x64", "win-cuda-12.4-x64", "ubuntu-cuda-13.3-arm64", "win-cuda-13.4-arm64"], "12"),
        };
        if let Some(f) = want.iter().find_map(|id| pick(id)) {
            let driver = card.driver.as_deref().unwrap_or("unknown");
            return Some((
                f,
                format!("{} found by nvidia-smi (driver {driver}): CUDA {generation} is the fastest build for it.", card.name),
            ));
        }
    }
    if let Some(card) = amd_rocm
        && let Some(f) = ["ubuntu-rocm-10.0-x64", "win-rocm-10.0-x64"].iter().find_map(|id| pick(id))
    {
        return Some((f, format!("{} found by rocm-smi: the ROCm build drives it directly.", card.name)));
    }
    let f = flavors.iter().copied().find(|f| f.accelerator == "vulkan").or_else(|| flavors.first().copied())?;
    let why = match hardware.accelerators.iter().find(|a| a.kind != AcceleratorKind::Cpu) {
        Some(card) => format!("{}: the Vulkan build drives it without a vendor SDK.", card.name),
        None => "No GPU was detected by nvidia-smi or rocm-smi. The Vulkan build finds cards those tools cannot see — integrated graphics included — and runs on the CPU when there is none.".to_string(),
    };
    Some((f, why))
}

// ---------------------------------------------------------------------------
// The release listing.
// ---------------------------------------------------------------------------

/// One downloadable file of a release, as the release API describes it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    /// `sha256:<hex>`, when the API publishes one.
    #[serde(default)]
    pub digest: Option<String>,
}

impl ReleaseAsset {
    pub fn sha256(&self) -> Option<String> {
        self.digest.as_deref()?.strip_prefix("sha256:").map(|h| h.to_ascii_lowercase())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// A `b<build>` release: the tag and everything under it.
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseListing {
    pub tag: String,
    pub assets: Vec<ReleaseAsset>,
    #[serde(skip)]
    fetched_at: Option<Instant>,
}

impl ReleaseListing {
    pub fn asset(&self, name: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn is_build_tag(tag: &str) -> bool {
    tag.len() > 1 && tag.starts_with('b') && tag[1..].chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Installing.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallState {
    Idle,
    Resolving,
    Downloading,
    Extracting,
    Verifying,
    Done,
    Failed,
}

impl InstallState {
    pub fn is_active(self) -> bool {
        matches!(self, InstallState::Resolving | InstallState::Downloading | InstallState::Extracting | InstallState::Verifying)
    }
}

/// What the current (or last) installation is doing. Polled by the Settings page.
#[derive(Clone, Debug, Serialize)]
pub struct InstallStatus {
    pub state: InstallState,
    pub flavor: Option<String>,
    pub tag: Option<String>,
    /// One line for the UI: "downloading llama-b11009-bin-macos-arm64.tar.gz".
    pub detail: String,
    /// The ids the Downloads panel shows, so a person can watch or cancel there.
    pub download_ids: Vec<String>,
    pub installed: Option<InstalledEngine>,
    pub error: Option<String>,
    pub started_ms: Option<u64>,
}

impl InstallStatus {
    fn idle() -> Self {
        InstallStatus {
            state: InstallState::Idle,
            flavor: None,
            tag: None,
            detail: String::new(),
            download_ids: Vec::new(),
            installed: None,
            error: None,
            started_ms: None,
        }
    }
}

/// An engine this module installed, as its manifest records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledEngine {
    /// The `llama-server` binary.
    pub path: PathBuf,
    pub tag: String,
    pub flavor: String,
    pub accelerator: String,
    pub version: Option<String>,
    pub installed_at_unix: u64,
    /// The device list the binary gave when installed. What "GPU build" turned out to mean here.
    #[serde(default)]
    pub devices: Option<Vec<EngineDevice>>,
}

const MANIFEST: &str = "misaka-engine.json";

pub struct EngineInstaller {
    http: reqwest::Client,
    api_base: String,
    /// `<data dir>/engines/llama.cpp`.
    root: PathBuf,
    status: RwLock<InstallStatus>,
    listing: RwLock<Option<ReleaseListing>>,
}

impl EngineInstaller {
    pub fn new(root: PathBuf) -> Self {
        EngineInstaller {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(60))
                .user_agent(concat!("misaka-studio/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client builds"),
            api_base: std::env::var(RELEASES_API_ENV)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| DEFAULT_RELEASES_API.to_string()),
            root,
            status: RwLock::new(InstallStatus::idle()),
            listing: RwLock::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn status(&self) -> InstallStatus {
        self.status.read().await.clone()
    }

    /// The release the next install would use, from a short-lived cache.
    pub async fn release(&self, pinned: Option<&str>) -> Result<ReleaseListing> {
        if pinned.is_none()
            && let Some(cached) = self.listing.read().await.as_ref()
            && cached.fetched_at.is_some_and(|t| t.elapsed() < LISTING_TTL)
        {
            return Ok(cached.clone());
        }
        let listing = self.fetch_release(pinned).await?;
        if pinned.is_none() {
            *self.listing.write().await = Some(listing.clone());
        }
        Ok(listing)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = self
            .http
            .get(url)
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| Error::Download { message: format!("{url}: {e}") })?;
        if !response.status().is_success() {
            let status = response.status();
            let hint = if status.as_u16() == 403 {
                " — GitHub's unauthenticated rate limit is 60 requests an hour; try again later"
            } else {
                ""
            };
            return Err(Error::Download { message: format!("{url} returned {status}{hint}") });
        }
        response.json::<T>().await.map_err(|e| Error::Download { message: format!("{url}: not a release listing: {e}") })
    }

    async fn fetch_release(&self, pinned: Option<&str>) -> Result<ReleaseListing> {
        let tag = match pinned {
            Some(tag) => tag.to_string(),
            None => self.resolve_tag().await?,
        };
        let release: Release = self.get_json(&format!("{}/releases/tags/{tag}", self.api_base)).await?;
        Ok(ReleaseListing { tag: release.tag_name, assets: release.assets, fetched_at: Some(Instant::now()) })
    }

    /// The `b<build>` tag upstream's latest release points at.
    async fn resolve_tag(&self) -> Result<String> {
        let latest: Release = self.get_json(&format!("{}/releases/latest", self.api_base)).await?;
        if let Some(pointer) = latest.assets.iter().find(|a| a.name == "nightly-tag.txt") {
            let text = self
                .http
                .get(&pointer.browser_download_url)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| Error::Download { message: format!("{}: {e}", pointer.browser_download_url) })?
                .text()
                .await
                .map_err(|e| Error::Download { message: format!("{}: {e}", pointer.browser_download_url) })?;
            let tag = text.trim().to_string();
            if is_build_tag(&tag) {
                return Ok(tag);
            }
        }
        if is_build_tag(&latest.tag_name) {
            return Ok(latest.tag_name);
        }
        let recent: Vec<Release> = self.get_json(&format!("{}/releases?per_page=20", self.api_base)).await?;
        recent
            .into_iter()
            .map(|r| r.tag_name)
            .find(|t| is_build_tag(t))
            .ok_or_else(|| Error::Download { message: "no b<build> release found in upstream's recent releases".into() })
    }

    /// The engines installed under the root, newest first.
    pub async fn installed(&self) -> Vec<InstalledEngine> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || scan_installed(&root)).await.unwrap_or_default()
    }

    /// Start installing `flavor_id` at `tag` (or the release the pointer names). Returns at once;
    /// progress is in [`Self::status`] and, for the bytes, the download list.
    pub async fn install(self: &Arc<Self>, app: Arc<AppState>, flavor_id: &str, tag: Option<String>) -> Result<InstallStatus> {
        let flavor = flavor(flavor_id).ok_or_else(|| {
            Error::bad_request(format!(
                "no llama.cpp build called '{flavor_id}' is published for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
        {
            let mut status = self.status.write().await;
            if status.state.is_active() {
                return Err(Error::bad_request(format!("an engine install is already running ({})", status.detail)));
            }
            *status = InstallStatus {
                state: InstallState::Resolving,
                flavor: Some(flavor.id.to_string()),
                tag: tag.clone(),
                detail: "finding the current release".into(),
                download_ids: Vec::new(),
                installed: None,
                error: None,
                started_ms: Some(now_ms()),
            };
        }
        let installer = self.clone();
        tokio::spawn(async move {
            match installer.run(app, flavor, tag).await {
                Ok(installed) => {
                    installer
                        .set(|s| {
                            s.state = InstallState::Done;
                            s.detail = format!("installed {} at {}", installed.tag, installed.path.display());
                            s.installed = Some(installed);
                        })
                        .await
                }
                Err(Error::Cancelled) => {
                    installer
                        .set(|s| {
                            s.state = InstallState::Failed;
                            s.detail = "cancelled".into();
                            s.error = Some("the download was cancelled".into());
                        })
                        .await
                }
                Err(e) => {
                    tracing::warn!("engine install failed: {e}");
                    installer
                        .set(|s| {
                            s.state = InstallState::Failed;
                            s.detail = "failed".into();
                            s.error = Some(e.to_string());
                        })
                        .await
                }
            }
        });
        Ok(self.status().await)
    }

    async fn set(&self, f: impl FnOnce(&mut InstallStatus)) {
        f(&mut *self.status.write().await);
    }

    async fn run(&self, app: Arc<AppState>, flavor: &'static EngineFlavor, tag: Option<String>) -> Result<InstalledEngine> {
        let listing = self.release(tag.as_deref()).await?;
        self.set(|s| s.tag = Some(listing.tag.clone())).await;

        let main_name = flavor.asset.replace("{tag}", &listing.tag);
        let mut names = vec![main_name.clone()];
        names.extend(flavor.companions.iter().map(|c| c.replace("{tag}", &listing.tag)));
        let assets: Vec<&ReleaseAsset> = names
            .iter()
            .map(|name| {
                listing.asset(name).ok_or_else(|| Error::Download {
                    message: format!("release {} has no asset named {name}; upstream may have renamed its builds", listing.tag),
                })
            })
            .collect::<Result<_>>()?;

        // 1. The bytes, through the model download pipeline: resumable, verified, visible.
        let downloads_dir = self.root.join("downloads");
        tokio::fs::create_dir_all(&downloads_dir).await.map_err(|e| Error::io(downloads_dir.display(), e))?;
        let mut archives = Vec::new();
        for asset in &assets {
            let destination = downloads_dir.join(&asset.name);
            let id = format!("ggml-org/llama.cpp/{}/{}", listing.tag, asset.name);
            self.set(|s| {
                s.state = InstallState::Downloading;
                s.detail = format!("downloading {}", asset.name);
                s.download_ids.push(id.clone());
            })
            .await;
            app.downloads
                .fetch(
                    id.clone(),
                    "ggml-org/llama.cpp".to_string(),
                    asset.name.clone(),
                    format!("llama.cpp {} ({})", listing.tag, flavor.label),
                    asset.browser_download_url.clone(),
                    destination.clone(),
                    asset.sha256(),
                    Some(asset.size),
                )
                .await?;
            let finished = app.downloads.wait(&id).await;
            match finished.as_ref().map(|p| p.status) {
                Some(DownloadStatus::Completed) => {}
                Some(DownloadStatus::Cancelled) => return Err(Error::Cancelled),
                Some(DownloadStatus::Failed) => {
                    return Err(Error::Download {
                        message: finished.and_then(|p| p.error).unwrap_or_else(|| "download failed".into()),
                    });
                }
                _ => return Err(Error::Download { message: format!("the download of {} ended without a result", asset.name) }),
            }
            archives.push(destination);
        }

        // 2. Unpack into a directory named for what it is. A previous attempt's directory is
        //    replaced whole: half an engine beside a whole one is two engines' worth of confusion.
        let dest = self.root.join(format!("{}-{}", listing.tag, flavor.id));
        self.set(|s| {
            s.state = InstallState::Extracting;
            s.detail = format!("unpacking into {}", dest.display());
        })
        .await;
        let binary = {
            let dest = dest.clone();
            let archives = archives.clone();
            tokio::task::spawn_blocking(move || unpack_engine(&archives, &dest))
                .await
                .map_err(|e| Error::Download { message: format!("unpacking did not run: {e}") })??
        };

        // 3. Does it run, and what can it drive? Before the setting moves.
        self.set(|s| {
            s.state = InstallState::Verifying;
            s.detail = format!("asking {} what it can drive", binary.display());
        })
        .await;
        let probe: EngineProbe = probe_program(&binary).await;
        if !probe.runs() {
            return Err(Error::Engine {
                backend: "llamacpp",
                message: format!(
                    "the installed llama-server does not run: {}. The build may need a library this machine lacks; the file is at {}",
                    probe.error.as_deref().unwrap_or("no output"),
                    binary.display()
                ),
            });
        }
        let installed = InstalledEngine {
            path: binary.clone(),
            tag: listing.tag.clone(),
            flavor: flavor.id.to_string(),
            accelerator: flavor.accelerator.to_string(),
            version: probe.version.clone(),
            installed_at_unix: now_ms() / 1000,
            devices: probe.devices.clone(),
        };
        let manifest = serde_json::to_string_pretty(&installed).map_err(|e| Error::Download { message: e.to_string() })?;
        let manifest_path = dest.join(MANIFEST);
        tokio::fs::write(&manifest_path, manifest).await.map_err(|e| Error::io(manifest_path.display(), e))?;

        // 4. Point the Studio at it. `apply_settings` unloads whatever the old engine held and
        //    rebuilds the backend, so the next load is this binary's.
        let mut settings = app.settings.read().await.clone();
        settings.backend.llama_server_path = Some(binary.clone());
        app.apply_settings(settings).await?;

        // 5. The archives have done their job; the CUDA runtime one is 400 MB.
        for archive in archives {
            let _ = tokio::fs::remove_file(&archive).await;
        }
        for id in self.status().await.download_ids {
            let _ = app.downloads.forget(&id).await;
        }
        tracing::info!(path = %binary.display(), tag = %listing.tag, flavor = flavor.id, gpu = probe.has_gpu(), "engine installed");
        Ok(installed)
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Every manifest under the root, newest install first.
fn scan_installed(root: &Path) -> Vec<InstalledEngine> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<InstalledEngine> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| std::fs::read_to_string(e.path().join(MANIFEST)).ok())
        .filter_map(|text| serde_json::from_str::<InstalledEngine>(&text).ok())
        .filter(|engine| engine.path.is_file())
        .collect();
    found.sort_by_key(|engine| std::cmp::Reverse(engine.installed_at_unix));
    found
}

// ---------------------------------------------------------------------------
// Archives.
// ---------------------------------------------------------------------------

/// Unpack the main archive into `dest`, then each companion beside the binary it contains.
/// Returns the binary. Synchronous: it is called from `spawn_blocking`.
pub fn unpack_engine(archives: &[PathBuf], dest: &Path) -> Result<PathBuf> {
    let (main, companions) = archives.split_first().ok_or_else(|| Error::Download { message: "nothing to unpack".into() })?;
    if dest.exists() {
        std::fs::remove_dir_all(dest).map_err(|e| Error::io(dest.display(), e))?;
    }
    std::fs::create_dir_all(dest).map_err(|e| Error::io(dest.display(), e))?;
    extract_archive(main, dest)?;
    let binary = find_file(dest, engine_file_name(), 5)
        .ok_or_else(|| Error::Download { message: format!("{} contains no {}", main.display(), engine_file_name()) })?;
    let bin_dir = binary.parent().ok_or_else(|| Error::Download { message: "the binary has no directory".into() })?.to_path_buf();
    for (i, companion) in companions.iter().enumerate() {
        // Companions are flattened beside the binary whatever their own layout: a DLL is found
        // by the executable's directory, not by the archive's.
        let staging = dest.join(format!(".companion-{i}"));
        std::fs::create_dir_all(&staging).map_err(|e| Error::io(staging.display(), e))?;
        extract_archive(companion, &staging)?;
        move_files_into(&staging, &bin_dir)?;
        let _ = std::fs::remove_dir_all(&staging);
    }
    make_executable(&binary)?;
    Ok(binary)
}

/// `.tar.gz`/`.tgz` or `.zip`, by name. Both extractors refuse entries that would land outside
/// `into` — an archive is somebody else's bytes.
pub fn extract_archive(archive: &Path, into: &Path) -> Result<()> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let file = std::fs::File::open(archive).map_err(|e| Error::io(archive.display(), e))?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
        let mut tar = tar::Archive::new(decoder);
        tar.set_preserve_permissions(true);
        tar.set_overwrite(true);
        // `unpack` skips entries that would escape `into` rather than writing them.
        tar.unpack(into).map_err(|e| Error::Download { message: format!("{}: {e}", archive.display()) })?;
        Ok(())
    } else if name.ends_with(".zip") {
        extract_zip(file, archive, into)
    } else {
        Err(Error::Download { message: format!("{}: not a .tar.gz or .zip", archive.display()) })
    }
}

fn extract_zip(file: std::fs::File, archive: &Path, into: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(file).map_err(|e| Error::Download { message: format!("{}: {e}", archive.display()) })?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| Error::Download { message: format!("{}: {e}", archive.display()) })?;
        // `enclosed_name` is `None` for `..`, absolute paths and drive letters.
        let Some(relative) = entry.enclosed_name() else {
            tracing::warn!("skipping {:?} in {}: it would land outside the target", entry.name(), archive.display());
            continue;
        };
        let target = into.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| Error::io(target.display(), e))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| Error::io(target.display(), e))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| Error::io(target.display(), e))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

/// The first file called `name` under `root`, breadth-first, no deeper than `depth`.
pub fn find_file(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..=depth {
        let mut next = Vec::new();
        for dir in &level {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_file() && path.file_name().is_some_and(|n| n == name) {
                    return Some(path);
                }
                if path.is_dir() {
                    next.push(path);
                }
            }
        }
        level = next;
        if level.is_empty() {
            break;
        }
    }
    None
}

/// Move every regular file under `from` (any depth) into `into`, replacing what is there.
fn move_files_into(from: &Path, into: &Path) -> Result<()> {
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| Error::io(dir.display(), e))?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && let Some(name) = path.file_name()
            {
                let target = into.join(name);
                if std::fs::rename(&path, &target).is_err() {
                    std::fs::copy(&path, &target).map_err(|e| Error::io(target.display(), e))?;
                }
            }
        }
    }
    Ok(())
}

fn make_executable(binary: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(binary).map_err(|e| Error::io(binary.display(), e))?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        std::fs::set_permissions(binary, perms).map_err(|e| Error::io(binary.display(), e))?;
    }
    let _ = binary;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use misaka_studio_core::hardware::Accelerator;

    fn machine(accelerators: Vec<Accelerator>) -> HardwareSnapshot {
        HardwareSnapshot {
            os: "test".into(),
            arch: "x86_64".into(),
            cpu_name: "cpu".into(),
            physical_cores: Some(8),
            logical_cores: 16,
            total_memory: 16 << 30,
            available_memory: 8 << 30,
            accelerators,
        }
    }

    fn card(kind: AcceleratorKind, name: &str, driver: Option<&str>) -> Accelerator {
        Accelerator {
            kind,
            name: name.into(),
            total_memory: Some(12 << 30),
            free_memory: Some(11 << 30),
            usable_memory: Some(10 << 30),
            driver: driver.map(str::to_string),
            index: 0,
        }
    }

    /// Every platform the Studio ships on has a build; Linux and Windows have a GPU build AND a
    /// CPU one, because "install the GPU build" must never be the only door.
    #[test]
    fn every_shipping_platform_has_builds_and_the_gpu_ones_come_first() {
        for (os, arch) in
            [("macos", "aarch64"), ("linux", "x86_64"), ("linux", "aarch64"), ("windows", "x86_64"), ("windows", "aarch64")]
        {
            let flavors = flavors_for(os, arch);
            assert!(!flavors.is_empty(), "{os}/{arch}");
            assert_ne!(flavors[0].accelerator, "cpu", "{os}/{arch}: the first option is a GPU build");
            if os != "macos" {
                assert!(flavors.iter().any(|f| f.accelerator == "cpu"), "{os}/{arch} has a CPU build");
            }
            for f in &flavors {
                assert!(f.asset.contains("{tag}"), "{}: the asset name takes the release tag", f.id);
                let ext = if os == "windows" { ".zip" } else { ".tar.gz" };
                assert!(f.asset.ends_with(ext), "{}: upstream ships {ext} for {os}", f.id);
            }
        }
        assert!(flavors_for("freebsd", "x86_64").is_empty());
    }

    /// The asset names, against the listing upstream published on 2026-09-16 (b11009). If upstream
    /// renames a build, this is the test that goes red before a user does.
    #[test]
    fn asset_names_match_what_upstream_publishes() {
        let published = [
            "llama-b11009-bin-macos-arm64.tar.gz",
            "llama-b11009-bin-macos-x64.tar.gz",
            "llama-b11009-bin-ubuntu-vulkan-x64.tar.gz",
            "llama-b11009-bin-ubuntu-cuda-12.8-x64.tar.gz",
            "cudart-llama-b11009-bin-ubuntu-cuda-12.8-x64.tar.gz",
            "llama-b11009-bin-ubuntu-cuda-13.3-x64.tar.gz",
            "cudart-llama-b11009-bin-ubuntu-cuda-13.3-x64.tar.gz",
            "llama-b11009-bin-ubuntu-rocm-10.0-x64.tar.gz",
            "llama-b11009-bin-ubuntu-x64.tar.gz",
            "llama-b11009-bin-ubuntu-vulkan-arm64.tar.gz",
            "llama-b11009-bin-ubuntu-cuda-13.3-arm64.tar.gz",
            "cudart-llama-b11009-bin-ubuntu-cuda-13.3-arm64.tar.gz",
            "llama-b11009-bin-ubuntu-arm64.tar.gz",
            "llama-b11009-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "llama-b11009-bin-win-cuda-13.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-x64.zip",
            "llama-b11009-bin-win-vulkan-x64.zip",
            "llama-b11009-bin-win-rocm-10.0-x64.zip",
            "llama-b11009-bin-win-cpu-x64.zip",
            "llama-b11009-bin-win-cuda-13.4-arm64.zip",
            "cudart-llama-bin-win-cuda-13.4-arm64.zip",
            "llama-b11009-bin-win-opencl-adreno-arm64.zip",
            "llama-b11009-bin-win-cpu-arm64.zip",
        ];
        for (os, arch) in [
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            for f in flavors_for(os, arch) {
                let main = f.asset.replace("{tag}", "b11009");
                assert!(published.contains(&main.as_str()), "{main} is not a published asset");
                for c in f.companions {
                    let name = c.replace("{tag}", "b11009");
                    assert!(published.contains(&name.as_str()), "{name} is not a published asset");
                }
            }
        }
    }

    #[test]
    fn the_recommendation_follows_the_card_and_its_driver() {
        let (f, why) =
            recommend(&machine(vec![card(AcceleratorKind::Cuda, "NVIDIA GeForce RTX 3060", Some("550.54.14"))]), "linux", "x86_64")
                .unwrap();
        assert_eq!(f.id, "ubuntu-cuda-12.8-x64", "{why}");
        assert!(why.contains("RTX 3060") && why.contains("550.54.14"), "{why}");

        let (f, _) = recommend(&machine(vec![card(AcceleratorKind::Cuda, "RTX 5090", Some("581.10"))]), "windows", "x86_64").unwrap();
        assert_eq!(f.id, "win-cuda-13.4-x64", "a 580+ driver takes CUDA 13");

        let (f, _) = recommend(&machine(vec![card(AcceleratorKind::Rocm, "Radeon RX 7900 XTX", None)]), "linux", "x86_64").unwrap();
        assert_eq!(f.id, "ubuntu-rocm-10.0-x64");

        // The tester's Ubuntu box: nothing found by the vendor tools. Vulkan, with the reason.
        let (f, why) = recommend(&machine(vec![]), "linux", "x86_64").unwrap();
        assert_eq!(f.id, "ubuntu-vulkan-x64");
        assert!(why.contains("No GPU was detected"), "{why}");

        let (f, _) = recommend(&machine(vec![]), "macos", "aarch64").unwrap();
        assert_eq!(f.id, "macos-arm64");
        assert!(recommend(&machine(vec![]), "plan9", "mips").is_none());
    }

    #[test]
    fn a_build_tag_is_b_and_digits() {
        assert!(is_build_tag("b11009"));
        assert!(!is_build_tag("v0.4.1"));
        assert!(!is_build_tag("b"));
        assert!(!is_build_tag("build-1"));
    }

    #[test]
    fn a_digest_is_read_from_the_api_shape() {
        let asset = ReleaseAsset { name: "x".into(), size: 1, browser_download_url: "u".into(), digest: Some("sha256:ABCDEF".into()) };
        assert_eq!(asset.sha256().as_deref(), Some("abcdef"));
        let none = ReleaseAsset { name: "x".into(), size: 1, browser_download_url: "u".into(), digest: None };
        assert_eq!(none.sha256(), None);
    }

    /// A tarball in upstream's layout (`build/bin/llama-server` plus libraries) unpacks, the
    /// binary is found under it, and it is executable afterwards whatever the archive said.
    #[test]
    fn a_tarball_in_upstreams_layout_yields_an_executable_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("llama-btest-bin-test.tar.gz");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut tar = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            let body = b"#!/bin/sh\necho hi\n";
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, format!("build/bin/{}", engine_file_name()), &body[..]).unwrap();
            let mut lib = tar::Header::new_gnu();
            lib.set_size(3);
            lib.set_mode(0o644);
            lib.set_cksum();
            tar.append_data(&mut lib, "build/bin/libllama.so", &b"lib"[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dest = dir.path().join("engine");
        let binary = unpack_engine(&[archive], &dest).unwrap();
        assert_eq!(binary, dest.join("build/bin").join(engine_file_name()));
        assert!(binary.parent().unwrap().join("libllama.so").is_file(), "the library is beside it");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(std::fs::metadata(&binary).unwrap().permissions().mode() & 0o111, 0, "executable");
        }
    }

    /// A zip with the binary at its root (upstream's Windows layout) and a companion whose files
    /// must end up beside the binary. An entry that tries to climb out is dropped, not written.
    #[test]
    fn a_zip_unpacks_flat_companions_land_beside_the_binary_and_traversal_is_refused() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("llama-btest-bin-win.zip");
        {
            let file = std::fs::File::create(&main).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(engine_file_name(), options).unwrap();
            zip.write_all(b"MZ").unwrap();
            zip.start_file("ggml.dll", options).unwrap();
            zip.write_all(b"dll").unwrap();
            zip.start_file("../escape.txt", options).unwrap();
            zip.write_all(b"no").unwrap();
            zip.finish().unwrap();
        }
        let companion = dir.path().join("cudart.zip");
        {
            let file = std::fs::File::create(&companion).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("nested/cudart64_12.dll", options).unwrap();
            zip.write_all(b"cuda").unwrap();
            zip.finish().unwrap();
        }
        let dest = dir.path().join("engine");
        let binary = unpack_engine(&[main, companion], &dest).unwrap();
        assert_eq!(binary, dest.join(engine_file_name()));
        assert!(dest.join("ggml.dll").is_file());
        assert!(dest.join("cudart64_12.dll").is_file(), "flattened beside the binary");
        assert!(!dest.join("nested").exists(), "the staging tree is gone");
        assert!(!dir.path().join("escape.txt").exists(), "the traversal entry was not written");
    }

    #[test]
    fn an_unknown_archive_type_and_an_archive_without_the_binary_are_named_errors() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("engine.rar");
        std::fs::write(&odd, b"x").unwrap();
        let err = unpack_engine(&[odd], &dir.path().join("a")).unwrap_err().to_string();
        assert!(err.contains(".tar.gz"), "{err}");

        let empty = dir.path().join("empty.zip");
        zip::ZipWriter::new(std::fs::File::create(&empty).unwrap()).finish().unwrap();
        let err = unpack_engine(&[empty], &dir.path().join("b")).unwrap_err().to_string();
        assert!(err.contains(engine_file_name()), "{err}");
    }

    #[test]
    fn installed_engines_are_read_from_their_manifests_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        for (tag, at) in [("b1", 10u64), ("b2", 20)] {
            let engine_dir = dir.path().join(format!("{tag}-flavor"));
            std::fs::create_dir_all(&engine_dir).unwrap();
            let binary = engine_dir.join(engine_file_name());
            std::fs::write(&binary, b"x").unwrap();
            let manifest = InstalledEngine {
                path: binary,
                tag: tag.into(),
                flavor: "flavor".into(),
                accelerator: "vulkan".into(),
                version: None,
                installed_at_unix: at,
                devices: None,
            };
            std::fs::write(engine_dir.join(MANIFEST), serde_json::to_string(&manifest).unwrap()).unwrap();
        }
        // A manifest whose binary is gone is not an installed engine.
        let gone = dir.path().join("b3-flavor");
        std::fs::create_dir_all(&gone).unwrap();
        let manifest = InstalledEngine {
            path: gone.join(engine_file_name()),
            tag: "b3".into(),
            flavor: "flavor".into(),
            accelerator: "vulkan".into(),
            version: None,
            installed_at_unix: 30,
            devices: None,
        };
        std::fs::write(gone.join(MANIFEST), serde_json::to_string(&manifest).unwrap()).unwrap();

        let tags: Vec<_> = scan_installed(dir.path()).into_iter().map(|e| e.tag).collect();
        assert_eq!(tags, ["b2", "b1"]);
    }
}
