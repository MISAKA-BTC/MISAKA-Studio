//! **The guard in front of `/api/v1`, through the assembled server.**
//!
//! The unit tests in `src/api/guard.rs` cover the decision; this covers what only the router can
//! show: that every `/api/v1` route is behind it (a GET that reads, a PUT that rewrites the
//! settings file), that `/v1` is not, that an origin listed in `server.cors_origins` is let into the
//! Studio API and still not into the workspace API — and that a `"*"` in that list, which used to
//! panic the CORS layer at startup, is the deliberate way back to "any origin".

use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use serde_json::{Value, json};
use std::sync::Arc;

struct Studio {
    url: String,
    state: Arc<AppState>,
    http: reqwest::Client,
    _data: tempfile::TempDir,
}

async fn studio(cors_origins: &[&str]) -> Studio {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let mut settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Mock, ..Default::default() },
        ..Default::default()
    };
    settings.server.cors_origins = cors_origins.iter().map(|s| s.to_string()).collect();
    let origins = settings.server.cors_origins.clone();
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state.clone(), None, origins);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Studio { url, state, http: reqwest::Client::new(), _data: data }
}

impl Studio {
    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut request = self.http.get(format!("{}{path}", self.url));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: &Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut request = self.http.request(method, format!("{}{path}", self.url)).json(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

const EVIL: (&str, &str) = ("Origin", "https://evil.example");

#[tokio::test(flavor = "multi_thread")]
async fn the_studio_api_answers_its_own_window_and_refuses_foreign_pages() {
    let studio = studio(&[]).await;

    // The window's own callers: same-origin (no Origin), the Vite dev server, the Tauri window.
    for headers in [vec![], vec![("Origin", "http://localhost:5174")], vec![("Origin", "tauri://localhost")]] {
        let (status, body) = studio.get("/api/v1/settings", &headers).await;
        assert_eq!(status, 200, "{headers:?}: {body}");
    }

    // A foreign page reads nothing…
    let (status, body) = studio.get("/api/v1/settings", &[EVIL]).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["type"], "permission_error");
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(body["error"]["message"].as_str().unwrap().contains("server.cors_origins"), "the refusal says how to allow it: {body}");
    let (status, _) = studio.get("/api/v1/records", &[EVIL]).await;
    assert_eq!(status, 403, "the record log holds prompts");

    // …and changes nothing: the settings file is not rewritten to a page's choosing.
    let (_, before) = studio.get("/api/v1/settings", &[]).await;
    let mut changed = before.clone();
    changed["huggingface"]["endpoint"] = json!("https://evil.example");
    let (status, body) = studio.send(reqwest::Method::PUT, "/api/v1/settings", &changed, &[EVIL]).await;
    assert_eq!(status, 403, "{body}");
    let (_, after) = studio.get("/api/v1/settings", &[]).await;
    assert_eq!(before, after, "a refused PUT leaves the settings as they were");

    // DNS rebinding: the page's own hostname, pointed at 127.0.0.1.
    let (status, body) =
        studio.get("/api/v1/settings", &[("Host", "evil.example:1338"), ("Origin", "http://evil.example:1338")]).await;
    assert_eq!(status, 403, "{body}");
    // A no-cors request from another site carries no Origin, but says where it came from.
    let (status, _) = studio.get("/api/v1/system", &[("Sec-Fetch-Site", "cross-site")]).await;
    assert_eq!(status, 403);

    // Health is behind it too, and the desktop shell's own probe — Host without a port — gets in.
    let (status, _) = studio.get("/api/v1/health", &[("Host", "127.0.0.1")]).await;
    assert_eq!(status, 200);
    let (status, _) = studio.get("/api/v1/health", &[EVIL]).await;
    assert_eq!(status, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_openai_surface_stays_open_to_any_origin() {
    let studio = studio(&[]).await;
    let (status, body) = studio.get("/v1/models", &[EVIL]).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["object"], "list");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listed_origin_reaches_the_studio_api_but_not_the_workspace() {
    let studio = studio(&["https://app.example"]).await;
    let app = ("Origin", "https://app.example");
    let (status, body) = studio.get("/api/v1/settings", &[app]).await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = studio.get("/api/v1/settings", &[EVIL]).await;
    assert_eq!(status, 403, "listing one origin lets in that one");

    let dir = tempfile::tempdir().unwrap();
    let open = json!({ "root": dir.path().to_str().unwrap() });
    let (status, body) = studio.send(reqwest::Method::POST, "/api/v1/workspace/open", &open, &[app]).await;
    assert_eq!(status, 403, "a listed origin never reaches files and commands: {body}");

    // The list is read per request, so a change to the settings applies without a restart.
    studio.state.settings.write().await.server.cors_origins = vec!["https://evil.example".into()];
    let (status, _) = studio.get("/api/v1/settings", &[EVIL]).await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wildcard_listing_restores_any_origin_for_the_studio_api_only() {
    // Building the router with "*" in the list used to panic inside tower-http's CORS layer.
    let studio = studio(&["*"]).await;
    let (status, body) = studio.get("/api/v1/settings", &[EVIL]).await;
    assert_eq!(status, 200, "{body}");

    let dir = tempfile::tempdir().unwrap();
    let open = json!({ "root": dir.path().to_str().unwrap() });
    let (status, _) = studio.send(reqwest::Method::POST, "/api/v1/workspace/open", &open, &[EVIL]).await;
    assert_eq!(status, 403, "the wildcard does not reach the workspace API");
    // Nor past the Host check: rebinding is not an origin.
    let (status, _) = studio.get("/api/v1/settings", &[("Host", "evil.example:1338"), EVIL]).await;
    assert_eq!(status, 403);
}
