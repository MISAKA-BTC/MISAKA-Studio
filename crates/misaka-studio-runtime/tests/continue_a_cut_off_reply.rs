//! **"Continue" reaches a 512-token class as the question and where the answer stopped.**
//!
//! The field report (2026-09-17): a geometry question answered through the mining lane was cut off
//! at "これらを x_1 と $", and pressing 続きを生成 appended "もちろんです、続きを生成します。ただし、
//! 具体的な内容を提供していただけますか？" to it. The button added a user turn saying "please continue";
//! the window trim kept that turn and dropped the question and the partial answer, so the model
//! was asked to continue nothing.
//!
//! This runs the Studio against a stand-in gateway that records what it is sent, with the class's
//! real window (512), and checks the request the lane actually receives.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures_util::StreamExt;
use misaka_studio_core::provenance::SamplingCommitment;
use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use misaka_studio_runtime::backend::{ChatMessage, StreamEvent, prompt_tokens_upper_bound};
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
                let sse = "data: {\"choices\":[{\"delta\":{\"content\":\" y_1 について解くと\"}}]}\n\n\
                           data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                           data: [DONE]\n\n";
                ([("content-type", "text/event-stream")], sse).into_response()
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
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();
    state.load("qwen25-1.5b-a16", None).await.expect("the gateway 'loads'");
    (state, data)
}

const QUESTION: &str = "原点 O(0, 0) を中心とする半径 1 の円に, 円外の点 P(x0, y0) から 2 本の接線を引く。\n\
(1) 2 つの接点の中点を Q とするとき, 点 Q の座標 (x1, y1) を, 点 P の座標 (x0, y0) を用いて表せ。また, OP･OQ=1 であることを示せ。\n\
(2) 点 P が直線 x+y=2 上を動くとき, 点 Q の軌跡を求めよ。";

/// The mined answer as it was cut off (the real one, from the queue, 256 tokens).
const PARTIAL: &str = "(1) まず、点 P から引いた 2 本の接線をそれぞれ $l_1$ と $l_2$ とします。これらの接線の接点を $A$ と $B$ とします。\n\n\
点 $A$ と $B$ はそれぞれ $l_1$ と $l_2$ と接するので、それぞれの接線の方程式は以下のように書けます：\n\n\
$l_1: y - y_0 = \\frac{y_0 - 0}{x_0 - 1}(x - 1)$\n$l_2: y - y_0 = \\frac{y_0 - 0}{x_0 + 1}(x - 1)$\n\n\
これらの接線の方程式をそれぞれ $x = x_1$ と $y = y_1$ に変形すると、\n\n\
$l_1: x_1 = \\frac{x_0 - 1}{y_0 - 0}(y - 1)$\n$l_2: x_1 = \\frac{x_0 + 1}{y_0 - 0}(y - 1)$\n\nこれらを x_1 と";

#[tokio::test(flavor = "multi_thread")]
async fn the_lane_is_sent_the_question_and_the_end_of_the_answer_within_its_window() {
    let (url, seen) = gateway().await;
    let (state, _data) = studio(&url).await;

    // What the chat sends on 続きを生成: the conversation as it stands, ending in the cut-off reply.
    let messages = vec![
        ChatMessage::new("system", "日本語で答えてください。"),
        ChatMessage::new("user", QUESTION),
        ChatMessage::new("assistant", PARTIAL),
    ];
    let params = SamplingCommitment { max_tokens: 2048, ..Default::default() };
    let mut stream = state.generate(messages, None, params, Vec::new()).await.expect("the continuation is sent");
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        if let StreamEvent::Delta(d) = event.expect("no stream error") {
            text.push_str(&d);
        }
    }
    assert_eq!(text, " y_1 について解くと");

    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1, "one request, no refusal retry");
    let sent: Vec<ChatMessage> = serde_json::from_value(bodies[0]["messages"].clone()).unwrap();

    // The lane's template closes every turn, so the request must not end with the partial answer.
    assert_eq!(sent.last().map(|m| m.role.as_str()), Some("user"), "{sent:?}");
    let turn = &sent.last().unwrap().content;
    assert!(turn.starts_with(QUESTION.trim()), "the question is there, whole: {turn}");
    assert!(turn.contains("繰り返さず"), "and what to do with it: {turn}");
    assert!(turn.ends_with("これらを x_1 と"), "and where the answer stopped: {turn}");
    assert_eq!(sent.first().map(|m| m.content.as_str()), Some("日本語で答えてください。"), "the system prompt is kept");

    // It fits: the prompt estimate plus the decode ceiling the lane was asked for stay in 512.
    let ceiling = bodies[0]["max_tokens"].as_u64().unwrap();
    let prompt = prompt_tokens_upper_bound(&sent);
    assert!(prompt + ceiling <= 512, "prompt {prompt} + ceiling {ceiling} over the class's window");
    assert!(ceiling >= 96, "and leaves a real answer's worth of room: {ceiling}");
}

/// A question so long that nothing of the answer fits beside it is told so — it is not sent as a
/// continuation of nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_with_no_room_is_refused_with_a_sentence() {
    let (url, seen) = gateway().await;
    let (state, _data) = studio(&url).await;
    let messages = vec![ChatMessage::new("user", QUESTION.repeat(3)), ChatMessage::new("assistant", PARTIAL)];
    let refused = match state.generate(messages, None, SamplingCommitment { max_tokens: 2048, ..Default::default() }, Vec::new()).await
    {
        Ok(_) => panic!("a continuation with no room must be refused"),
        Err(e) => e.to_string(),
    };
    assert!(refused.contains("新しいチャット"), "{refused}");
    assert!(seen.lock().unwrap().is_empty(), "nothing reached the lane");
}
