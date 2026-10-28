//! **A thinking model's reasoning reaches the client — and never the record.**
//!
//! Qwen3 served by a current `llama-server` sends its thinking as `delta.reasoning_content` and
//! its answer as `delta.content`. The Studio read only the second, so the chat showed nothing for
//! as long as the model thought. Here a stand-in engine streams exactly that shape through the
//! whole path — the engine's SSE, the backend, the runtime, `/v1/chat/completions` — and the test
//! reads both ends: what the client gets, streamed and not, and what the provenance record commits
//! to, which must be the answer alone.
//!
//! The second half drives the mock backend's `/think` switch to show `misaka.thinking: false`
//! arriving at the engine as `disable_thinking`.

use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures_util::StreamExt;
use misaka_studio_core::provenance::commit_output;
use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::AppState;
use std::sync::Arc;

const REASONING: [&str; 2] = ["Okay, the user says hi.", " A short greeting fits."];
const ANSWER: [&str; 2] = ["Hello", "! How can I help?"];
const PREFILL_MS: u64 = 150;

/// An engine answering the way `llama-server --reasoning-format auto` does for a thinking model.
async fn thinking_engine() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route("/health", get(|| async { axum::Json(serde_json::json!({ "n_ctx": 4096, "can_submit": true })) }))
        .route(
            "/v1/chat/completions",
            post(|| async {
                // A prefill's worth of waiting, so the time to the first token is measurable.
                tokio::time::sleep(std::time::Duration::from_millis(PREFILL_MS)).await;
                let mut sse = String::from("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n");
                for r in REASONING {
                    sse.push_str(&format!("data: {}\n\n", serde_json::json!({ "choices": [{ "index": 0, "delta": { "reasoning_content": r } }] })));
                }
                for c in ANSWER {
                    sse.push_str(&format!("data: {}\n\n", serde_json::json!({ "choices": [{ "index": 0, "delta": { "content": c } }] })));
                }
                sse.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":16,\"total_tokens\":25}}\n\n");
                sse.push_str("data: [DONE]\n\n");
                ([("content-type", "text/event-stream")], sse).into_response()
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn serve(state: Arc<AppState>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = misaka_studio_runtime::api::router(state, None, Vec::new());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

/// The `data:` payloads of an SSE body, parsed.
fn chunks(body: &str) -> Vec<serde_json::Value> {
    body.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).filter_map(|p| serde_json::from_str(p).ok()).collect()
}

fn delta_field(chunks: &[serde_json::Value], field: &str) -> String {
    chunks.iter().filter_map(|c| c["choices"][0]["delta"][field].as_str()).collect()
}

async fn sse_body(response: reqwest::Response) -> String {
    assert!(response.status().is_success(), "{}", response.status());
    let mut text = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    text
}

#[tokio::test(flavor = "multi_thread")]
async fn reasoning_streams_as_reasoning_content_and_stays_out_of_the_record() {
    let engine = thinking_engine().await;
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join("qwen25-1.5b-a16.palwart"), b"PALW\0\0\0\x01").unwrap();
    let mut settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
        ..Default::default()
    };
    settings.node.palw_gateway_url = Some(engine);
    settings.context.fetch_class_tokenizer = false;
    settings.provenance.keep_transcripts = true;
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();
    state.load("qwen25-1.5b-a16", None).await.expect("the stand-in engine 'loads'");
    let studio = serve(state.clone()).await;
    let http = reqwest::Client::new();
    let messages = serde_json::json!([{ "role": "user", "content": "hi" }]);

    // Streamed: reasoning chunks first, in OpenAI's chunk envelope, then the answer.
    let body = sse_body(
        http.post(format!("{studio}/v1/chat/completions"))
            .json(&serde_json::json!({ "messages": messages, "stream": true, "max_tokens": 64 }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let parsed = chunks(&body);
    assert_eq!(delta_field(&parsed, "reasoning_content"), REASONING.concat());
    assert_eq!(delta_field(&parsed, "content"), ANSWER.concat());
    let first_reasoning = parsed.iter().position(|c| c["choices"][0]["delta"]["reasoning_content"].is_string()).unwrap();
    let first_content = parsed.iter().position(|c| c["choices"][0]["delta"]["content"].is_string()).unwrap();
    assert!(first_reasoning < first_content, "the thinking arrives before the answer");
    let reasoning_chunk = &parsed[first_reasoning];
    assert_eq!(reasoning_chunk["object"], "chat.completion.chunk");
    assert!(reasoning_chunk["choices"][0]["finish_reason"].is_null());
    assert!(
        reasoning_chunk["choices"][0]["delta"].get("content").is_none(),
        "a reasoning chunk carries no content: {reasoning_chunk}"
    );
    assert!(body.trim_end().ends_with("data: [DONE]"));

    // Not streamed: the reasoning beside the answer, on the message.
    let whole: serde_json::Value = http
        .post(format!("{studio}/v1/chat/completions"))
        .json(&serde_json::json!({ "messages": messages, "stream": false, "max_tokens": 64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let message = &whole["choices"][0]["message"];
    assert_eq!(message["content"], ANSWER.concat());
    assert_eq!(message["reasoning_content"], REASONING.concat());

    // The records — written as each stream ends — commit to the answer and nothing else.
    let records = state.records.read().await.clone();
    let mut recorded = Vec::new();
    for _ in 0..100 {
        recorded = records.list(10).await;
        if recorded.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(recorded.len(), 2, "one record per completion");
    for stored in &recorded {
        assert_eq!(stored.record.output_commitment, commit_output(ANSWER.concat().as_bytes()), "the commitment is the answer's");
        assert_eq!(stored.completion.as_deref(), Some(ANSWER.concat().as_str()), "and so is the transcript");
        // The first token is the first reasoning token, timed from the request — not from after
        // the runtime had already taken the first event to check it for a refusal (which made
        // every record say 0 ms).
        let ttft = stored.record.time_to_first_token_ms.expect("a first token was timed");
        assert!(ttft >= PREFILL_MS - 10, "the wait before the first token is in it: {ttft} ms");
        assert!(stored.record.duration_ms >= ttft);
    }
}

/// A GGUF with a header and nothing else — enough for the store to list it and the mock to load it.
fn tiny_gguf(path: &std::path::Path) {
    let mut out = Vec::new();
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&1u64.to_le_bytes());
    let key = "general.architecture";
    out.extend_from_slice(&(key.len() as u64).to_le_bytes());
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&5u64.to_le_bytes());
    out.extend_from_slice(b"qwen3");
    std::fs::write(path, out).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn thinking_false_reaches_the_engine() {
    let data = tempfile::tempdir().unwrap();
    let models = data.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    tiny_gguf(&models.join("tiny-qwen3.gguf"));
    let settings = Settings {
        models_dir: models,
        backend: BackendSettings { kind: BackendKind::Mock, ..Default::default() },
        ..Default::default()
    };
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    state.store.refresh().await.unwrap();
    state.load("tiny-qwen3", None).await.expect("the mock loads");
    let studio = serve(state).await;
    let http = reqwest::Client::new();

    let ask = |thinking: Option<bool>| {
        let mut body =
            serde_json::json!({ "messages": [{ "role": "user", "content": "why is the sky blue? /think" }], "stream": true });
        if let Some(thinking) = thinking {
            body["misaka"] = serde_json::json!({ "thinking": thinking });
        }
        let http = http.clone();
        let url = format!("{studio}/v1/chat/completions");
        async move { chunks(&sse_body(http.post(url).json(&body).send().await.unwrap()).await) }
    };

    let default = ask(None).await;
    assert!(!delta_field(&default, "reasoning_content").is_empty(), "the model thinks by default");
    let on = ask(Some(true)).await;
    assert!(!delta_field(&on, "reasoning_content").is_empty());
    let off = ask(Some(false)).await;
    assert!(delta_field(&off, "reasoning_content").is_empty(), "thinking: false turns it off");
    assert_eq!(delta_field(&off, "content"), delta_field(&default, "content"), "and leaves the answer alone");
}
