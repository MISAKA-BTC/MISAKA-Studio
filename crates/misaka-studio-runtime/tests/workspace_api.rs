//! **The coding agent's workspace API, through the Studio's own HTTP router.**
//!
//! The unit tests in `src/workspace.rs` cover each operation's rules; this covers what only the
//! assembled server can show: the routes are where the UI expects them, the bodies have the shapes
//! it is written against, errors arrive in the API's `{error:{message,type}}` envelope — and the
//! guard turns away a foreign page and a rebound Host while the Studio's own callers get through.

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

async fn studio() -> Studio {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Mock, ..Default::default() },
        ..Default::default()
    };
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state.clone(), None, Vec::new());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Studio { url, state, http: reqwest::Client::new(), _data: data }
}

impl Studio {
    async fn post(&self, route: &str, body: Value) -> (u16, Value) {
        self.post_with(route, body, &[]).await
    }

    async fn post_with(&self, route: &str, body: Value, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut request = self.http.post(format!("{}/api/v1/workspace/{route}", self.url)).json(&body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let put = |rel: &str, content: &str| {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    put(".git/HEAD", "ref: refs/heads/main\n");
    put(".gitignore", "target/\n");
    put("src/main.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    put("src/lib.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n");
    put("target/debug/junk", "x");
    put("README.md", "# demo\n");
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guard_lets_the_studio_in_and_keeps_foreign_pages_out() {
    let studio = studio().await;
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let open = json!({ "root": root });

    // Allowed: no Origin (curl, the desktop shell), the Vite dev server, the Tauri window.
    for headers in [vec![], vec![("Origin", "http://localhost:5174")], vec![("Origin", "tauri://localhost")]] {
        let (status, body) = studio.post_with("open", open.clone(), &headers).await;
        assert_eq!(status, 200, "{headers:?}: {body}");
    }

    // Refused: a foreign page, and a DNS-rebinding Host.
    let (status, body) = studio.post_with("open", open.clone(), &[("Origin", "https://evil.example")]).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["type"], "permission_error");
    assert_eq!(body["error"]["code"], "forbidden");
    assert!(body["error"]["message"].as_str().unwrap().contains("evil.example"), "{body}");
    let (status, body) =
        studio.post_with("exec", json!({ "root": root, "command": "touch pwned" }), &[("Host", "evil.example:1338")]).await;
    assert_eq!(status, 403, "{body}");
    assert!(!dir.path().join("pwned").exists(), "a refused exec runs nothing");

    // The GET route is guarded too.
    let refused = studio
        .http
        .get(format!("{}/api/v1/workspace/browse", studio.url))
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 403);

    // Bound to the network — even with a key, and even presenting it — the API is not served.
    {
        let mut settings = studio.state.settings.write().await;
        settings.server.host = "0.0.0.0".into();
        settings.server.api_key = Some("k".into());
    }
    let response =
        studio.http.post(format!("{}/api/v1/workspace/open", studio.url)).bearer_auth("k").json(&open).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 403);
    // The rest of the API is unaffected by the guard.
    let health = studio.http.get(format!("{}/api/v1/health", studio.url)).send().await.unwrap();
    assert_eq!(health.status().as_u16(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn every_route_answers_in_the_shape_the_ui_expects() {
    let studio = studio().await;
    let dir = project();
    let root = dir.path().to_str().unwrap().to_string();

    let (status, opened) = studio.post("open", json!({ "root": root })).await;
    assert_eq!(status, 200, "{opened}");
    assert_eq!(opened["is_git"], true);
    assert_eq!(opened["branch"], "main");
    let canonical = opened["root"].as_str().unwrap().to_string();

    let browse: Value = studio
        .http
        .get(format!("{}/api/v1/workspace/browse", studio.url))
        .query(&[("path", canonical.as_str()), ("hidden", "true")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = browse["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec![".git", "src", "target"], "{browse}");
    assert!(browse["home"].is_string() && browse["parent"].is_string());

    let (status, tree) = studio.post("tree", json!({ "root": root, "depth": 2 })).await;
    assert_eq!(status, 200, "{tree}");
    let paths: Vec<&str> = tree["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(paths, vec!["src", "src/lib.rs", "src/main.rs", ".gitignore", "README.md"]);
    assert_eq!(tree["path"], ".");
    assert_eq!(tree["truncated"], false);
    assert!(tree["entries"][0]["size"].is_null() && tree["entries"][1]["size"].is_u64());

    let (_, found) = studio.post("files", json!({ "root": root, "query": "main" })).await;
    assert_eq!(found["paths"][0], "src/main.rs");

    let (_, read) = studio.post("read", json!({ "root": root, "path": "src/main.rs", "start_line": 2, "end_line": 2 })).await;
    assert_eq!(read["content"], "    println!(\"hello\");\n");
    assert_eq!((read["start_line"].as_u64(), read["end_line"].as_u64(), read["total_lines"].as_u64()), (Some(2), Some(2), Some(3)));
    assert_eq!(read["binary"], false);

    let (_, hits) = studio.post("search", json!({ "root": root, "pattern": "fn \\w+", "glob": "*.rs" })).await;
    assert_eq!(hits["matches"].as_array().unwrap().len(), 2, "{hits}");
    assert_eq!(hits["matches"][0], json!({ "path": "src/lib.rs", "line": 1, "text": "pub fn add(a: i32, b: i32) -> i32 {" }));
    let (status, bad_regex) = studio.post("search", json!({ "root": root, "pattern": "(" })).await;
    assert_eq!(status, 400);
    assert!(bad_regex["error"]["message"].as_str().unwrap().contains("invalid regex"));

    let (_, written) = studio.post("write", json!({ "root": root, "path": "notes/todo.md", "content": "- ship\n" })).await;
    assert_eq!(written, json!({ "path": "notes/todo.md", "created": true, "previous": null, "content": "- ship\n", "bytes": 7 }));

    let (_, edited) = studio
        .post("edit", json!({ "root": root, "path": "src/lib.rs", "old_string": "a + b", "new_string": "a.wrapping_add(b)" }))
        .await;
    assert_eq!(edited["replacements"], 1);
    assert!(edited["previous"].as_str().unwrap().contains("a + b"));
    assert!(edited["content"].as_str().unwrap().contains("a.wrapping_add(b)"));
    let (status, missing) =
        studio.post("edit", json!({ "root": root, "path": "src/lib.rs", "old_string": "a - b", "new_string": "x" })).await;
    assert_eq!(status, 400);
    assert_eq!(
        missing["error"]["message"],
        "old_string was not found in src/lib.rs. Read the file again and copy the exact text, including whitespace."
    );
    let (status, malformed) = studio.post("edit", json!({ "root": root, "path": "src/lib.rs", "new_string": "x" })).await;
    assert_eq!(status, 400);
    assert!(malformed["error"]["message"].as_str().unwrap().contains("old_string"), "{malformed}");

    let (_, removed) = studio.post("remove", json!({ "root": root, "path": "notes/todo.md" })).await;
    assert_eq!(removed, json!({ "removed": "notes/todo.md" }));

    let (status, escaped) = studio.post("read", json!({ "root": root, "path": "../../etc/passwd" })).await;
    assert_eq!(status, 400);
    assert!(escaped["error"]["message"].as_str().unwrap().starts_with("path escapes the workspace"));
    let (status, _) = studio.post("write", json!({ "root": root, "path": ".git/config", "content": "x" })).await;
    assert_eq!(status, 400);

    #[cfg(unix)]
    {
        let (_, ran) = studio.post("exec", json!({ "root": root, "command": "ls src && exit 4", "timeout_secs": 30 })).await;
        assert_eq!(ran["exit_code"], 4, "{ran}");
        assert_eq!(ran["stdout"], "lib.rs\nmain.rs\n");
        assert_eq!(ran["timed_out"], false);
        assert_eq!(ran["truncated"], false);
        assert!(ran["duration_ms"].is_u64() && ran["stderr"].is_string() && ran["command"] == "ls src && exit 4");
    }
}
