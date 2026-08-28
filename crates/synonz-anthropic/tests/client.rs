//! Client-level tests: SSE transform wiring over a local mock server, plus
//! an opt-in smoke test against the real API.
//!
//! Smoke test gating: `SYNONZ_ANTHROPIC_API_KEY` (and optionally
//! `SYNONZ_ANTHROPIC_BASE_URL` / `SYNONZ_ANTHROPIC_MODEL`). When the key is
//! absent the test is skipped — `cargo test` must stay green without
//! credentials.

use futures::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use synonz::message::Message;
use synonz::model::ModelRequest;
use synonz::{Model, ModelError, ModelStreamItem};
use synonz_anthropic::{Client, DEFAULT_BASE_URL};

const SSE_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"beijing \"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"is sunny\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

fn smoke_request() -> ModelRequest {
    ModelRequest::new(
        vec![Message::user("Reply with exactly: pong")],
        vec![],
        synonz::ModelParams::default().with_max_tokens(16),
    )
}

#[tokio::test]
async fn client_streams_deltas_and_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(SSE_BODY),
        )
        .mount(&server)
        .await;

    let client = Client::new(server.uri(), "test-key", "claude-sonnet-4-5");
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
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&server)
        .await;

    let client = Client::new(server.uri(), "test-key", "claude-sonnet-4-5");
    let error = match client.stream(smoke_request()).await {
        Err(error) => error,
        Ok(_) => panic!("429 must surface as an error"),
    };
    assert!(matches!(error, ModelError::RateLimited { .. }));
}

/// Opt-in smoke test against the real API. Skipped (not failed) when
/// `SYNONZ_ANTHROPIC_API_KEY` is not set.
#[tokio::test]
async fn real_api_smoke() {
    let Ok(api_key) = std::env::var("SYNONZ_ANTHROPIC_API_KEY") else {
        eprintln!("skipped: SYNONZ_ANTHROPIC_API_KEY not set");
        return;
    };
    let base_url =
        std::env::var("SYNONZ_ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
    let model_name =
        std::env::var("SYNONZ_ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-5".into());

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
