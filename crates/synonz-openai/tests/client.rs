//! Client-level tests: SSE transform wiring over a local mock server, plus
//! an opt-in smoke test against the real API.
//!
//! Smoke test gating: `SYNONZ_OPENAI_API_KEY` (and optionally
//! `SYNONZ_OPENAI_BASE_URL` / `SYNONZ_OPENAI_MODEL`). When the key is
//! absent the test is skipped — `cargo test` must stay green without
//! credentials.

use futures::StreamExt;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use synonz::message::Message;
use synonz::model::ModelRequest;
use synonz::{Model, ModelError, ModelStreamItem};
use synonz_openai::Client;

const SSE_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"beijing \"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"is sunny\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],",
    "\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}\n\n",
    "data: [DONE]\n\n",
);

fn smoke_request() -> ModelRequest {
    ModelRequest::new(vec![Message::user("Reply with exactly: pong")], vec![])
}

#[tokio::test]
async fn client_streams_deltas_and_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(&server)
        .await;

    let client = Client::new(server.uri(), "test-key", "gpt-4o-mini");
    let mut stream = client.stream(smoke_request()).await.unwrap();

    let first = stream.next().await;
    assert!(matches!(
        first,
        Some(ModelStreamItem::Delta(synonz::ModelDelta::Text { ref text })) if text == "beijing "
    ));
    let second = stream.next().await;
    assert!(matches!(
        second,
        Some(ModelStreamItem::Delta(synonz::ModelDelta::Text { ref text })) if text == "is sunny"
    ));

    let finish = stream.next().await;
    match finish {
        Some(ModelStreamItem::Finish { message, usage }) => {
            assert_eq!(
                message.blocks[0],
                synonz::ContentBlock::Text {
                    text: "beijing is sunny".into(),
                }
            );
            assert_eq!(usage, synonz::TokenUsage::new(7, 3));
        }
        other => panic!("expected finish item, got {other:?}"),
    }
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn error_status_maps_to_model_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&server)
        .await;

    let client = Client::new(server.uri(), "test-key", "gpt-4o-mini");
    let error = match client.stream(smoke_request()).await {
        Err(error) => error,
        Ok(_) => panic!("429 must surface as an error"),
    };
    assert!(matches!(error, ModelError::RateLimited { .. }));
}

#[tokio::test]
async fn runtime_options_and_model_apply_to_the_next_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(serde_json::json!({
            "model": "gpt-5",
            "reasoning_effort": "high",
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(&server)
        .await;

    let client = Client::new(server.uri(), "test-key", "gpt-4o-mini");
    // Clones share the live state: adjust through the clone, request
    // through the original — the next request must carry the changes.
    let clone = client.clone();
    clone.set_model("gpt-5");
    let mut options = clone.options();
    options.reasoning_effort = Some(synonz_openai::ReasoningEffort::High);
    clone.set_options(options);

    // The request is sent eagerly; the mock asserts the body.
    let _stream = client.stream(smoke_request()).await.unwrap();
}

/// Opt-in smoke test against the real API. Skipped (not failed) when
/// `SYNONZ_OPENAI_API_KEY` is not set.
#[tokio::test]
async fn real_api_smoke() {
    let Ok(api_key) = std::env::var("SYNONZ_OPENAI_API_KEY") else {
        eprintln!("skipped: SYNONZ_OPENAI_API_KEY not set");
        return;
    };
    let base_url = std::env::var("SYNONZ_OPENAI_BASE_URL")
        .unwrap_or_else(|_| synonz_openai::DEFAULT_BASE_URL.into());
    let model_name = std::env::var("SYNONZ_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let client = Client::new(base_url, api_key, model_name);
    let mut stream = client.stream(smoke_request()).await.unwrap();

    let mut text = String::new();
    while let Some(item) = stream.next().await {
        match item {
            ModelStreamItem::Delta(synonz::ModelDelta::Text { text: fragment }) => {
                text.push_str(&fragment);
            }
            ModelStreamItem::Finish { .. } => break,
            ModelStreamItem::Failed(error) => panic!("smoke call failed: {error}"),
            _ => {}
        }
    }
    assert!(!text.is_empty(), "expected non-empty response text");
}
