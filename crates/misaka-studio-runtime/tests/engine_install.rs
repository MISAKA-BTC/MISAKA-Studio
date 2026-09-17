//! Installing an engine end to end, against a release server this test runs itself.
//!
//! The real thing is GitHub's release API and a 30 MB tarball; this is the same shape — the
//! `latest` release with its `nightly-tag.txt` pointer, the `b<build>` release with its assets and
//! their `sha256:` digests, the download URLs — served from a port on loopback, with a tarball built
//! here around a `llama-server` that is a shell script answering `--version` and `--list-devices`
//! the way the binary does. What is exercised is everything the Studio does between "Install" and
//! a setting that points at a binary it has verified can run: resolution, download through the
//! model pipeline with the digest checked, unpacking in upstream's layout, the probe, the manifest,
//! and the settings write.
//!
//! Unix only: the stand-in engine is a shell script.
#![cfg(unix)]

use axum::Router;
use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::get;
use misaka_studio_core::settings::{BackendKind, BackendSettings, GpuLayers, Settings};
use misaka_studio_runtime::AppState;
use misaka_studio_runtime::backend::devices::probe_program;
use misaka_studio_runtime::engines::{InstallState, RELEASES_API_ENV, flavors_for};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const TAG: &str = "b90001";

/// Both tests point the installer at their own server through one process-wide variable, so they
/// take turns.
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock().await
}

/// A tarball in upstream's layout: `build/bin/llama-server` (a script), a library beside it.
fn tarball() -> Vec<u8> {
    let script = "#!/bin/sh\n\
        case \"$1\" in\n\
          --version) echo 'version: 90001 (feedface)' >&2; echo 'built with test for this machine' >&2 ;;\n\
          --list-devices) echo 'Available devices:'; echo '  Vulkan0: Stand-in GPU (8192 MiB, 8000 MiB free)' ;;\n\
          *) echo 'error: invalid argument' >&2; exit 1 ;;\n\
        esac\n";
    let mut out = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "build/bin/llama-server", script.as_bytes()).unwrap();
        let mut lib = tar::Header::new_gnu();
        lib.set_size(4);
        lib.set_mode(0o644);
        lib.set_cksum();
        tar.append_data(&mut lib, "build/bin/libllama.so", &b"stub"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }
    out
}

/// The release API, three routes deep, and the download host, on one loopback port.
async fn release_server(asset_name: String, bytes: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let size = bytes.len() as u64;

    let latest = {
        let base = base.clone();
        move || async move {
            axum::Json(serde_json::json!({
                "tag_name": "v9.9.9",
                "assets": [{ "name": "nightly-tag.txt", "size": 7, "browser_download_url": format!("{base}/dl/nightly-tag.txt"), "digest": null }]
            }))
        }
    };
    let tagged = {
        let base = base.clone();
        let asset_name = asset_name.clone();
        move |Path(tag): Path<String>| async move {
            if tag != TAG {
                return (axum::http::StatusCode::NOT_FOUND, "no such release").into_response();
            }
            axum::Json(serde_json::json!({
                "tag_name": TAG,
                "assets": [
                    { "name": asset_name, "size": size, "browser_download_url": format!("{base}/dl/{asset_name}"), "digest": digest },
                    { "name": "llama-b90001-ui.tar.gz", "size": 1, "browser_download_url": format!("{base}/dl/ui"), "digest": null }
                ]
            }))
            .into_response()
        }
    };
    let download = move |Path(name): Path<String>| {
        let bytes = bytes.clone();
        let asset_name = asset_name.clone();
        async move {
            if name == "nightly-tag.txt" {
                return format!("{TAG}\n").into_response();
            }
            if name == asset_name {
                return bytes.into_response();
            }
            (axum::http::StatusCode::NOT_FOUND, "no such asset").into_response()
        }
    };
    let app = Router::new()
        .route("/releases/latest", get(latest))
        .route("/releases/tags/{tag}", get(tagged))
        .route("/dl/{name}", get(download));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_is_resolved_downloaded_verified_unpacked_probed_and_wired_in() {
    let _turn = one_at_a_time().await;
    let flavor = flavors_for(std::env::consts::OS, std::env::consts::ARCH).into_iter().next().expect("a build for this platform");
    assert!(flavor.companions.is_empty(), "the stand-in serves one archive; a flavour with companions needs more routes");
    let asset_name = flavor.asset.replace("{tag}", TAG);
    let bytes = tarball();
    let base = release_server(asset_name.clone(), bytes).await;

    // The installer reads its API base once, at construction — before the state is built.
    unsafe { std::env::set_var(RELEASES_API_ENV, &base) };
    let data = tempfile::tempdir().unwrap();
    let models = tempfile::tempdir().unwrap();
    let settings = Settings {
        models_dir: models.path().to_path_buf(),
        backend: BackendSettings { kind: BackendKind::LlamaCpp, gpu_layers: GpuLayers::Auto, ..Default::default() },
        ..Default::default()
    };
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;

    let started = state.engines.install(state.clone(), flavor.id, None).await.expect("the install starts");
    assert!(started.state.is_active());

    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        let status = state.engines.status().await;
        if !status.state.is_active() {
            break status;
        }
        assert!(Instant::now() < deadline, "the install did not finish: {status:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.state, InstallState::Done, "{status:?}");
    let installed = status.installed.expect("an installed engine");

    // 1. The pointer was followed to the b-tag, and the binary lives under a directory named
    //    for the tag and the flavour.
    assert_eq!(installed.tag, TAG);
    assert_eq!(installed.flavor, flavor.id);
    assert!(
        installed.path.starts_with(data.path().join("engines").join("llama.cpp").join(format!("{TAG}-{}", flavor.id))),
        "{}",
        installed.path.display()
    );
    assert!(installed.path.is_file());
    assert!(installed.path.parent().unwrap().join("libllama.so").is_file(), "the library came with it");

    // 2. The binary was asked: its banner and its devices are in the manifest.
    assert_eq!(installed.version.as_deref(), Some("version: 90001 (feedface)"));
    let devices = installed.devices.expect("the device list");
    assert_eq!(devices[0].id, "Vulkan0");
    let manifest = installed.path.parent().unwrap().parent().unwrap().parent().unwrap().join("misaka-engine.json");
    assert!(manifest.is_file(), "manifest at {}", manifest.display());
    assert_eq!(state.engines.installed().await.len(), 1);

    // 3. The setting now names it, on disk as well as in memory, and the backend that was
    //    rebuilt from it resolves to the installed binary.
    let settings = state.settings.read().await.clone();
    assert_eq!(settings.backend.llama_server_path.as_deref(), Some(installed.path.as_path()));
    let on_disk = Settings::load(data.path().join("settings.json")).unwrap();
    assert_eq!(on_disk.backend.llama_server_path, settings.backend.llama_server_path);
    let backend = state.backend().await;
    let engine_devices = backend.devices().await.expect("the rebuilt backend can list devices");
    assert_eq!(engine_devices[0].id, "Vulkan0");

    // 4. The archive did its job and is gone; the download is no longer listed.
    assert!(!data.path().join("engines/llama.cpp/downloads").join(&asset_name).exists());
    assert!(state.downloads.list().await.is_empty(), "{:?}", state.downloads.list().await);

    // 5. The probe is what the engines page will show: a GPU verdict from the engine's own list.
    let probe = probe_program(&installed.path).await;
    assert!(probe.has_gpu());

    // 6. A second install while none is running is allowed; one during another is refused.
    let again = state.engines.install(state.clone(), flavor.id, Some(TAG.to_string())).await.expect("starts again");
    assert!(again.state.is_active());
    let refused = state.engines.install(state.clone(), flavor.id, None).await;
    assert!(refused.is_err(), "two installs at once");
    while state.engines.status().await.state.is_active() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(state.engines.status().await.state, InstallState::Done);
}

/// A flavour upstream does not publish for this platform, and a release without the asset, are
/// both refusals with the name in them — not a download of nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_flavour_or_asset_is_a_named_refusal() {
    let _turn = one_at_a_time().await;
    let flavor = flavors_for(std::env::consts::OS, std::env::consts::ARCH).into_iter().next().unwrap();
    let base = release_server("llama-b90001-bin-somewhere-else.tar.gz".into(), b"nope".to_vec()).await;
    unsafe { std::env::set_var(RELEASES_API_ENV, &base) };
    let data = tempfile::tempdir().unwrap();
    let state = AppState::new(
        Settings { models_dir: data.path().to_path_buf(), ..Default::default() },
        data.path().join("s.json"),
        data.path().to_path_buf(),
    )
    .await;

    let err = state.engines.install(state.clone(), "amiga-warp-x64", None).await.unwrap_err().to_string();
    assert!(err.contains("amiga-warp-x64"), "{err}");

    state.engines.install(state.clone(), flavor.id, None).await.expect("starts");
    while state.engines.status().await.state.is_active() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = state.engines.status().await;
    assert_eq!(status.state, InstallState::Failed);
    let error = status.error.unwrap();
    assert!(error.contains(&flavor.asset.replace("{tag}", TAG)), "{error}");
    assert!(state.settings.read().await.backend.llama_server_path.is_none(), "a failed install moves no setting");
}
