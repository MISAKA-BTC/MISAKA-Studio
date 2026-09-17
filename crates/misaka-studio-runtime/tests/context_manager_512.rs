//! **A conversation on a 512-token class, through the Studio's own HTTP API.**
//!
//! Before the context manager, the third question of a conversation reached the lane with the
//! first two turns dropped whole — "use the Q from before" had nothing to refer to. Here a
//! three-turn conversation with a pinned note goes through `/v1/chat/completions` to a stand-in
//! gateway that holds 512 tokens and records what it receives, and the test reads both ends: what
//! the lane was sent, and the `misaka.context` report the chat stream opened with.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures_util::StreamExt;
use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use misaka_studio_runtime::backend::ChatMessage;
use misaka_studio_runtime::context::tokens::TokenCounter;
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

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
                let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"軌跡は円です。\"}}]}\n\n\
                           data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                           data: [DONE]\n\n";
                ([("content-type", "text/event-stream")], sse).into_response()
            }),
        )
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

async fn studio(gateway: &str, tokenizer: Option<std::path::PathBuf>) -> (String, Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("qwen25-1.5b-a16.palwart"), b"PALW\0\0\0\x01").unwrap();
    let mut settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    settings.node.palw_gateway_url = Some(gateway.to_string());
    settings.context.fetch_class_tokenizer = false;
    settings.context.tokenizer_path = tokenizer;
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();
    state.load("qwen25-1.5b-a16", None).await.expect("the gateway 'loads'");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state.clone(), None, Vec::new());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, state, data)
}

const Q1: &str = "原点 O(0, 0) を中心とする半径 1 の円に, 円外の点 P(x0, y0) から 2 本の接線を引く。2 つの接点の中点 Q の座標 (x1, y1) を x0, y0 で表せ。";
const A1: &str = "接点を A, B とする。直線 AB は P の極線 x0 x + y0 y = 1 であり、Q は直線 OP と AB の交点である。OP 上の点を (t x0, t y0) とおいて極線に代入すると t (x0² + y0²) = 1。よって Q = (x0/(x0²+y0²), y0/(x0²+y0²)) となる。";
const Q2: &str = "では OP・OQ = 1 を示してください。";
const A2: &str = "OP = √(x0²+y0²)、OQ = √(x1²+y1²) = 1/√(x0²+y0²) である。したがって OP・OQ = 1 が成り立つ。";
const Q3: &str = "点 P が直線 x+y=2 上を動くとき、さっきの Q の座標を使って Q の軌跡を求めて。";

fn conversation() -> Vec<ChatMessage> {
    vec![
        ChatMessage::new("system", "日本語で答えてください。"),
        ChatMessage::new("user", Q1),
        ChatMessage::new("assistant", A1),
        ChatMessage::new("user", Q2),
        ChatMessage::new("assistant", A2),
        ChatMessage::new("user", Q3),
    ]
}

/// The request as the Studio's chat sends it, and the stream's opening chunk.
async fn chat(studio: &str, messages: &[ChatMessage], pinned: &[&str]) -> (serde_json::Value, String) {
    let body = serde_json::json!({
        "messages": messages,
        "stream": true,
        "max_tokens": 2048,
        "misaka": { "pinned": pinned },
    });
    let response = reqwest::Client::new().post(format!("{studio}/v1/chat/completions")).json(&body).send().await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let mut text = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    let opener = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .find_map(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .expect("an opening chunk");
    (opener, text)
}

fn check_lane_request(seen: &Seen, counter: &TokenCounter) -> (Vec<ChatMessage>, u64) {
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "one request reached the lane");
    let sent: Vec<ChatMessage> = serde_json::from_value(bodies[0]["messages"].clone()).unwrap();
    let ceiling = bodies[0]["max_tokens"].as_u64().unwrap();
    let prompt = counter.messages(&sent);
    assert!(prompt + ceiling <= 512, "prompt {prompt} + ceiling {ceiling} over the class's 512");
    (sent, ceiling)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_third_question_reaches_the_lane_with_the_first_two_remembered() {
    let (gateway_url, seen) = gateway().await;
    let (studio_url, _state, _data) = studio(&gateway_url, None).await;
    let (opener, stream) = chat(&studio_url, &conversation(), &["答えは x, y の式で書く"]).await;
    assert!(stream.contains("軌跡は円です。"), "the lane's answer streamed back");

    // What the lane received.
    let (sent, _) = check_lane_request(&seen, &TokenCounter::estimate());
    assert_eq!(sent.last().map(|m| m.content.as_str()), Some(Q3), "the question, last and whole");
    let system = &sent[0];
    assert_eq!(system.role, "system");
    assert!(system.content.starts_with("日本語で答えてください。"), "the client's system prompt leads: {}", system.content);
    assert!(system.content.contains("答えは x, y の式で書く"), "the pin: {}", system.content);
    assert!(system.content.contains("これまでの会話の要点"), "a memory: {}", system.content);
    assert!(
        system.content.contains("Q = (x0/(x0²+y0²)") || sent.iter().any(|m| m.content.contains("Q = (x0/(x0²+y0²)")),
        "the result the question refers to reached the lane, verbatim or in the memory: {sent:?}"
    );

    // What the stream said about it, before the first token.
    let context = &opener["misaka"]["context"];
    assert_eq!(context["managed"], true, "{context}");
    assert_eq!(context["window"], 512);
    assert_eq!(context["pinned_included"], 1);
    assert_eq!(context["pinned_omitted"], 0);
    assert_eq!(context["memory"]["source"], "extract");
    assert_eq!(context["counter"]["kind"], "estimate");
    let reported: Vec<ChatMessage> = serde_json::from_value(context["sent"].clone()).unwrap();
    assert_eq!(reported, sent, "the report shows exactly what the lane received");
}

/// The same conversation, counted by Qwen2.5's own tokenizer: nothing is spent on the estimate's
/// margin, so the answer keeps its half and the report says the count is the tokenizer's.
#[tokio::test(flavor = "multi_thread")]
async fn with_the_class_tokenizer_the_plan_is_counted_exactly() {
    let Ok(path) = std::env::var("MISAKA_TEST_QWEN25_TOKENIZER") else {
        eprintln!("skipping: set MISAKA_TEST_QWEN25_TOKENIZER to Qwen2.5's tokenizer.json");
        return;
    };
    let (gateway_url, seen) = gateway().await;
    let (studio_url, _state, _data) = studio(&gateway_url, Some(path.clone().into())).await;
    let (opener, _) = chat(&studio_url, &conversation(), &["答えは x, y の式で書く"]).await;
    let counter = TokenCounter::from_tokenizer_file(std::path::Path::new(&path)).unwrap();
    let (sent, ceiling) = check_lane_request(&seen, &counter);
    let context = &opener["misaka"]["context"];
    assert_eq!(context["counter"]["kind"], "tokenizer");
    let prompt = context["prompt_tokens"].as_u64().unwrap();
    assert_eq!(prompt, counter.messages(&sent), "the report's count is the tokenizer's count of what was sent");
    assert!(prompt <= 256, "inside the prompt's half: {prompt}");
    assert!(ceiling >= 256 - 4, "and the answer gets its half, less the exact margin: {ceiling}");
    eprintln!(
        "exact: prompt {prompt}, ceiling {ceiling}, recent {}, older {}, memory {}",
        context["recent_messages"], context["older_messages"], context["memory"]
    );
}

/// The preview endpoint answers the same plan without generating.
#[tokio::test(flavor = "multi_thread")]
async fn the_plan_can_be_previewed_without_sending_anything() {
    let (gateway_url, seen) = gateway().await;
    let (studio_url, _state, _data) = studio(&gateway_url, None).await;
    let report: serde_json::Value = reqwest::Client::new()
        .post(format!("{studio_url}/api/v1/context/plan"))
        .json(&serde_json::json!({ "messages": conversation(), "pinned": ["答えは x, y の式で書く"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["managed"], true, "{report}");
    assert_eq!(report["window"], 512);
    assert!(seen.lock().unwrap().is_empty(), "nothing reached the lane");
}
