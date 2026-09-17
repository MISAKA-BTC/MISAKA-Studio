//! **One load at a time, and a load of what is already loaded is that load.**
//!
//! Measured (2026-09-17): the Studio opened, began loading its startup model — the 512-token class
//! — and a chat sent before that finished asked for the first model in the list instead. Two
//! engines started side by side; the chat was answered by a 32K GGUF with fifteen copies of the
//! conversation behind it. The window now asks for the startup model, and the runtime serializes
//! loads so a second request for the same model waits for the first and returns it.

use axum::Json;
use axum::extract::State;
use axum::routing::get;
use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test(flavor = "multi_thread")]
async fn two_loads_of_one_model_start_it_once() {
    let hits = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route(
            "/health",
            get(|State(hits): State<Arc<AtomicUsize>>| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                // Slow enough that the second load arrives while the first is still in flight.
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                Json(serde_json::json!({ "n_ctx": 512, "can_submit": true }))
            }),
        )
        .with_state(hits.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("qwen25-1.5b-a16.palwart"), b"PALW\0\0\0\x01").unwrap();
    let mut settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    settings.node.palw_gateway_url = Some(url);
    settings.context.fetch_class_tokenizer = false;
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();

    let (a, b) = tokio::join!(state.load("qwen25-1.5b-a16", None), state.load("qwen25-1.5b-a16", None));
    let (a, b) = (a.expect("first load"), b.expect("second load"));
    assert_eq!(a.model_id, b.model_id);
    assert_eq!(b.context_size, Some(512));
    assert_eq!(b.gpu_layers, None, "an engine that does not offload is not reported as holding layers on a GPU");
    let one_load = hits.load(Ordering::SeqCst);

    // A third, after both: still nothing new.
    state.load("qwen25-1.5b-a16", None).await.expect("third load");
    assert_eq!(hits.load(Ordering::SeqCst), one_load, "a load of the loaded model starts nothing");
    assert!(one_load <= 2, "one load's worth of health checks (availability, then load), not two loads': {one_load}");
}
