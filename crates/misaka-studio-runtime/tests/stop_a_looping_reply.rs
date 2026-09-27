//! **A backend with no anti-repetition mechanism at all** — `misaka-palw-serve`'s own `/health`
//! reports `"sampler": "greedy-argmax-lowest-id"`, and the free-prompt gateway refuses `stop` by
//! name (ADR-0096 Decision 4: "the seat replays a greedy decode and nothing else") — has no way to
//! escape a loop on its own, and nothing upstream cuts one off: without help it runs to
//! `max_tokens` (2026-09-27 field report: a math question answered by the 8k class repeated a short
//! phrase for the full 38-second, 2048-token budget).
//!
//! This runs the Studio against a stand-in gateway that streams the same short phrase over and
//! over, and checks that `crate::repetition` (checked live in `AppState::generate_managed`) ends
//! the reply itself — the client never sees more than the three repeats it takes to recognise the
//! pattern, however many more the "backend" was prepared to send.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures_util::StreamExt;
use misaka_studio_core::provenance::SamplingCommitment;
use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use misaka_studio_runtime::backend::{ChatMessage, StreamEvent};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// The phrase repeated in the stand-in's stream. 9 characters, no internal whitespace, so three
/// copies back to back are exactly the `period = 9` case `crate::repetition::short_loop_period`
/// looks for — the same shape as its `a_short_period_needs_three_full_repeats_not_two` unit test,
/// run here through a real streamed HTTP response instead of a literal string.
const PHRASE: &str = "テスト用の文章です";

/// Five copies, each its own SSE delta — enough to prove the fourth and fifth are never read.
fn looping_sse_body() -> String {
    let mut body = String::new();
    for _ in 0..5 {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
            serde_json::to_string(PHRASE).unwrap()
        ));
    }
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
    body
}

async fn gateway() -> (String, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/health", get(|| async { Json(serde_json::json!({ "class_id": "4277d84f", "n_ctx": 512, "can_submit": true })) }))
        .route(
            "/v1/chat/completions",
            post(|State(seen): State<Seen>, Json(body): Json<serde_json::Value>| async move {
                seen.lock().unwrap().push(body);
                ([("content-type", "text/event-stream")], looping_sse_body()).into_response()
            }),
        )
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

async fn studio(url: &str) -> (Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("qwen25-1.5b-a16.palwart"), b"PALW\0\0\0\x01").unwrap();
    let mut settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    settings.node.palw_gateway_url = Some(url.to_string());
    settings.context.fetch_class_tokenizer = false; // no network fetch in a test
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();
    state.load("qwen25-1.5b-a16", None).await.expect("the gateway 'loads'");
    (state, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeating_stream_is_cut_after_three_copies_not_run_to_max_tokens() {
    let (url, seen) = gateway().await;
    let (state, _data) = studio(&url).await;

    let messages = vec![ChatMessage::new("user", "同じ答えを繰り返して")];
    // Large on purpose: if this were not caught, the mock backend has 5 copies queued and the
    // ceiling would let every one of them through.
    let params = SamplingCommitment { max_tokens: 2048, ..Default::default() };
    let mut stream = state.generate(messages, None, params, Vec::new()).await.expect("sent");

    let mut text = String::new();
    let mut finish_reason = String::new();
    while let Some(event) = stream.next().await {
        match event.expect("no stream error") {
            StreamEvent::Delta(d) => text.push_str(&d),
            StreamEvent::Done { finish_reason: reason, .. } => finish_reason = reason,
        }
    }

    assert_eq!(finish_reason, "repetition", "stopped for the reason it actually stopped for");
    assert_eq!(text, PHRASE.repeat(3), "exactly the three copies it took to recognise the loop — not four, not five");
    assert_eq!(seen.lock().unwrap().len(), 1, "one request; a loop is not a refusal and must not retry");
}

/// Ordinary, non-repeating text is untouched — three separate sentences never once repeat a run,
/// so nothing here should ever fire on a real answer.
#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_reply_streams_to_the_end() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"まず\"}}]}\n\n\
               data: {\"choices\":[{\"delta\":{\"content\":\"接線の方程式を立てる。\"}}]}\n\n\
               data: {\"choices\":[{\"delta\":{\"content\":\"次に交点を求める。\"}}]}\n\n\
               data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let app = axum::Router::new()
        .route("/health", get(|| async { Json(serde_json::json!({ "class_id": "4277d84f", "n_ctx": 512, "can_submit": true })) }))
        .route(
            "/v1/chat/completions",
            post(move |State(seen): State<Seen>, Json(body): Json<serde_json::Value>| async move {
                seen.lock().unwrap().push(body);
                ([("content-type", "text/event-stream")], sse).into_response()
            }),
        )
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (state, _data) = studio(&base).await;
    let messages = vec![ChatMessage::new("user", "接線を求めて")];
    let params = SamplingCommitment { max_tokens: 2048, ..Default::default() };
    let mut stream = state.generate(messages, None, params, Vec::new()).await.expect("sent");
    let mut text = String::new();
    let mut finish_reason = String::new();
    while let Some(event) = stream.next().await {
        match event.expect("no stream error") {
            StreamEvent::Delta(d) => text.push_str(&d),
            StreamEvent::Done { finish_reason: reason, .. } => finish_reason = reason,
        }
    }
    assert_eq!(text, "まず接線の方程式を立てる。次に交点を求める。");
    assert_eq!(finish_reason, "stop");
}
