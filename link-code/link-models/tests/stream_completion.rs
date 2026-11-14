//! Streaming completion tests.
//!
//! A large share of OpenAI-compatible gateways and local servers end a stream
//! with `data: [DONE]` (or by closing the connection) and never send a chunk
//! carrying `finish_reason`. Those replies used to look truncated to the agent
//! loop, which then printed "the previous reply was cut off". These tests pin
//! the adapter to reporting a normal finish in that case.

use futures_util::StreamExt;
use link_models::openai::OpenAiProvider;
use link_models::registry::ModelConfig;
use link_models::types::{ChatRequest, FinishReason, StreamEvent};
use link_models::ModelProvider;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serve one SSE response and return the bound port.
async fn serve_once(body: String) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Drain the request head; the body is small enough to ignore.
        let mut buffer = vec![0u8; 8192];
        let _ = socket.read(&mut buffer).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    port
}

fn provider(port: u16) -> OpenAiProvider {
    let config = ModelConfig {
        id: "mock".to_string(),
        display_name: "Mock".to_string(),
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model_id: "mock".to_string(),
        ..Default::default()
    };
    OpenAiProvider::new(config, None)
}

async fn collect_events(port: u16) -> Vec<StreamEvent> {
    let stream = provider(port)
        .stream_chat(ChatRequest::new("mock"))
        .await
        .expect("the mock server accepted the request");
    futures_util::pin_mut!(stream);
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("the mock stream is valid"));
    }
    events
}

#[tokio::test]
async fn a_done_sentinel_without_finish_reason_is_a_complete_reply() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"All done.\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let events = collect_events(serve_once(body).await).await;

    assert_eq!(events.len(), 2, "delta then done: {events:?}");
    assert!(matches!(
        events.last(),
        Some(StreamEvent::Done {
            finish_reason: FinishReason::Stop
        })
    ));
}

#[tokio::test]
async fn an_explicit_length_finish_still_reports_length() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"fn main() {\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let events = collect_events(serve_once(body).await).await;

    assert!(matches!(
        events.last(),
        Some(StreamEvent::Done {
            finish_reason: FinishReason::Length
        })
    ));
}
