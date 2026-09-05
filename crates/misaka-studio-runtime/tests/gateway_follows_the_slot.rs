//! **The gateway engine answers for the slot the settings name — not the one they used to.**
//!
//! `GatewayBackend::new` copies the gateway's address and the slot's token once, at construction.
//! Joining a pool slot for prompt mining (or forgetting one) rewrites exactly those two settings
//! while the engine kind stays `gateway`, and `apply_settings` used to rebuild the engine only
//! when the KIND changed.
//!
//! On 2026-09-05 that left a Studio whose Network tab said `slot-06` — bonded, lane on — while the
//! chat kept posting to `slot-04`'s gateway with `slot-04`'s token. The inference ran on the wrong
//! host's CPU for five minutes and the claim was refused at commit: that slot's bond was full,
//! which is why the person had left it. No engine is needed to show this; the test watches only
//! whether the engine instance is replaced.

use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::{backend::gateway, AppState};
use std::sync::Arc;

async fn studio(url: &str, token: &str) -> (Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().expect("tempdir");
    let mut settings = Settings {
        models_dir: data.path().join("models"),
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    std::fs::create_dir_all(&settings.models_dir).expect("models dir");
    settings.node.palw_gateway_url = Some(url.to_string());
    settings.node.pool_slot_token = Some(token.to_string());
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    (state, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn joining_another_slot_replaces_the_gateway_engine() {
    let (state, _data) = studio("https://pool.example/pool/v1/slots/slot-04/fp", "token-04").await;
    let before = state.backend().await;
    assert_eq!(before.name(), gateway::NAME, "the configured engine is the gateway");

    // The join handler's write: same kind, new address, new token.
    let mut joined = state.settings.read().await.clone();
    joined.node.pool_slot_id = Some("slot-06".into());
    joined.node.pool_slot_token = Some("token-06".into());
    joined.node.palw_gateway_url = Some("https://pool.example/pool/v1/slots/slot-06/fp".into());
    state.apply_settings(joined).await.expect("settings apply");

    let after = state.backend().await;
    assert_eq!(after.name(), gateway::NAME, "still the gateway engine");
    assert!(
        !Arc::ptr_eq(&before, &after),
        "the engine must be rebuilt when the slot changes: the old instance holds slot-04's address and token"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_token_alone_replaces_the_gateway_engine() {
    // The pool can re-issue a slot's token without moving the slot; the header the engine sends
    // must follow, or every chat is refused with 403 while the Network tab reads fine.
    let (state, _data) = studio("https://pool.example/pool/v1/slots/slot-06/fp", "old-token").await;
    let before = state.backend().await;

    let mut rotated = state.settings.read().await.clone();
    rotated.node.pool_slot_token = Some("new-token".into());
    state.apply_settings(rotated).await.expect("settings apply");

    assert!(!Arc::ptr_eq(&before, &state.backend().await), "a token change alone rebuilds the engine");
}

#[tokio::test(flavor = "multi_thread")]
async fn unrelated_settings_leave_the_engine_alone() {
    // The control: rebuilding on every save would unload the model on every theme change. Only the
    // fields the engine actually copied may trigger it.
    let (state, _data) = studio("https://pool.example/pool/v1/slots/slot-06/fp", "token-06").await;
    let before = state.backend().await;

    let mut cosmetic = state.settings.read().await.clone();
    cosmetic.generation.temperature = 0.2;
    cosmetic.ui.show_performance = false;
    state.apply_settings(cosmetic).await.expect("settings apply");

    assert!(Arc::ptr_eq(&before, &state.backend().await), "no engine field changed, so the instance stays");
}
