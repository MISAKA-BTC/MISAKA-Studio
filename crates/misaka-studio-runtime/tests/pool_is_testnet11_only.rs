//! **A testnet-11 pool slot cannot mine on testnet-12 — refused once, not retried into "gave up".**
//!
//! Field report (2026-09-27): a Studio migrated to testnet-12 kept a testnet-11 pool slot
//! (`misakascan.com/pool`, `contrib/minerpool/pool.py`, hard-coded `--netsuffix=11`) from before the
//! migration — the settings migration only kept the network on testnet-11 for a producer bond or an
//! attached node, not for a joined pool slot. Every chat message enqueued a job, the queue retried
//! it against a gateway that was never going to answer for this chain, and every one of them ended
//! "not mined — gave up" after the retry backoff ran out. This is the fix, from the API a real
//! Studio in that exact state would hit: the mining-queue view reports the slot unavailable by
//! network, before a single attempt, and a direct enqueue is refused the same way.

use misaka_studio_core::settings::{MiningMode, NodeNetwork, Settings};
use misaka_studio_runtime::AppState;
use std::sync::Arc;

async fn studio(network: NodeNetwork, pool_slot_id: Option<&str>) -> (String, Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let mut settings = Settings { models_dir: data.path().join("models"), ..Default::default() };
    settings.node.network = network;
    settings.node.pool_slot_id = pool_slot_id.map(str::to_string);
    settings.node.palw_gateway_url = Some("http://127.0.0.1:8790".to_string());
    settings.node.mining_mode = MiningMode::Background;
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = misaka_studio_runtime::api::router(state.clone(), None, Vec::new());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_testnet11_slot_is_reported_unavailable_once_the_studio_is_on_testnet12() {
    let (base, _state, _data) = studio(NodeNetwork::Testnet12, Some("slot-06")).await;
    let view: serde_json::Value = reqwest::get(format!("{base}/api/v1/network/mining-queue")).await.unwrap().json().await.unwrap();
    assert_eq!(view["background_available"], false);
    let blocker = view["background_blocker"].as_str().expect("a reason");
    assert!(blocker.contains("testnet-11"), "{blocker}");
    assert!(blocker.contains("testnet-12"), "{blocker}");
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_enqueue_is_refused_the_same_way_not_left_to_retry_and_fail() {
    let (base, _state, _data) = studio(NodeNetwork::Testnet12, Some("slot-06")).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/api/v1/network/mining-queue"))
        .json(&serde_json::json!({ "prompt": "1+1=" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let text = response.text().await.unwrap();
    assert!(text.contains("testnet-11"), "{text}");
}

/// The same slot is perfectly usable on the network it was actually joined for.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_slot_is_available_on_testnet11_itself() {
    let (base, _state, _data) = studio(NodeNetwork::Testnet11, Some("slot-06")).await;
    let view: serde_json::Value = reqwest::get(format!("{base}/api/v1/network/mining-queue")).await.unwrap().json().await.unwrap();
    // No local engine is loaded in this test, so it is still unavailable — but for THAT reason,
    // not the network one.
    let blocker = view["background_blocker"].as_str().unwrap_or("");
    assert!(!blocker.contains("testnet-11 only"), "{blocker}");
}
