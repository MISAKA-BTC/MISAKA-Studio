//! **The gateway engine answers at the address the settings name — not the one they used to.**
//!
//! `GatewayBackend::new` copies the gateway's address once, at construction, and `apply_settings` used to rebuild the engine
//! only when the KIND changed. A moved gateway (another port, another listen address for the node-free stack) with the kind
//! still `gateway` then kept posting every chat — and mining it — to the old address. No engine is needed to show this; the
//! test watches only whether the engine instance is replaced.

use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::{AppState, backend::gateway};
use std::sync::Arc;

async fn studio(url: &str) -> (Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().expect("tempdir");
    let mut settings = Settings {
        models_dir: data.path().join("models"),
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    std::fs::create_dir_all(&settings.models_dir).expect("models dir");
    settings.node.palw_gateway_url = Some(url.to_string());
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    (state, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_gateway_replaces_the_gateway_engine() {
    let (state, _data) = studio("http://127.0.0.1:8790").await;
    let before = state.backend().await;
    assert_eq!(before.name(), gateway::NAME, "the configured engine is the gateway");

    let mut moved = state.settings.read().await.clone();
    moved.node.palw_gateway_url = Some("http://127.0.0.1:8791".into());
    state.apply_settings(moved).await.expect("settings apply");

    let after = state.backend().await;
    assert_eq!(after.name(), gateway::NAME, "still the gateway engine");
    assert!(!Arc::ptr_eq(&before, &after), "the engine must be rebuilt when the address changes: the old instance holds the old one");
}

#[tokio::test(flavor = "multi_thread")]
async fn unrelated_settings_leave_the_engine_alone() {
    // The control: rebuilding on every save would unload the model on every theme change. Only the
    // fields the engine actually copied may trigger it.
    let (state, _data) = studio("http://127.0.0.1:8790").await;
    let before = state.backend().await;

    let mut cosmetic = state.settings.read().await.clone();
    cosmetic.generation.temperature = 0.2;
    cosmetic.ui.show_performance = false;
    state.apply_settings(cosmetic).await.expect("settings apply");

    assert!(Arc::ptr_eq(&before, &state.backend().await), "no engine field changed, so the instance stays");
}
