//! The Web surface shares the workspace's loopback-only guard; off/mining
//! refusals must happen before a browser is launched or any remote request runs.
use misaka_studio_core::settings::{BackendKind, Settings, WebMode};
use misaka_studio_runtime::state::AppState;
use serde_json::json;

#[tokio::test]
async fn web_is_opt_in_mining_gated_and_never_available_to_foreign_origins() {
    let data = tempfile::tempdir().unwrap();
    let mut settings = Settings { models_dir: data.path().join("models"), ..Default::default() };
    settings.backend.kind = BackendKind::Mock;
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().into()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state.clone(), None, vec![]);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let http = reqwest::Client::new();
    let response = http.post(format!("{base}/api/v1/web/search")).json(&json!({"query":"q", "mining":false})).send().await.unwrap();
    assert_eq!(response.status(), 400);
    assert!(response.text().await.unwrap().contains("Web access is off"));
    state.settings.write().await.web.mode = WebMode::Tor;
    for body in [json!({"query":"q"}), json!({"query":"q", "mining":true})] {
        let response = http.post(format!("{base}/api/v1/web/search")).json(&body).send().await.unwrap();
        assert_eq!(response.status(), 400);
        assert!(response.text().await.unwrap().contains("mining to be off"));
    }
    state.settings.write().await.server.cors_origins.push("https://allowed.example".into());
    for path in ["/browser", "/search", "/fetch", "/open", "/check"] {
        let request = if path == "/browser" {
            http.get(format!("{base}/api/v1/web{path}"))
        } else {
            http.post(format!("{base}/api/v1/web{path}")).json(&json!({"query":"q", "url":"https://example.org", "mining":false}))
        };
        let response = request.header("Origin", "https://allowed.example").send().await.unwrap();
        assert_eq!(response.status(), 403, "{path}: even listed foreign origins cannot launch a browser");
    }
}
