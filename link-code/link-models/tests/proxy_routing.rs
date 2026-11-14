//! Proves that the process-wide proxy setting really routes outbound requests.
//!
//! The listener plays the role of an HTTP proxy: it terminates the request
//! itself instead of forwarding it, so seeing an absolute-form request line at
//! the proxy port is enough to show that the client did not connect directly.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use link_models::proxy::{self, ProxyProtocol, ProxySettings};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn requests_are_routed_through_the_configured_proxy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let recorded = Arc::clone(&seen);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = vec![0u8; 4096];
        let read = socket.read(&mut buffer).await.unwrap();
        *recorded.lock().unwrap() = Some(String::from_utf8_lossy(&buffer[..read]).to_string());
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            )
            .await
            .unwrap();
    });

    proxy::set(ProxySettings {
        protocol: ProxyProtocol::Http,
        host: "127.0.0.1".to_string(),
        port,
        ..Default::default()
    });

    let client = proxy::apply(reqwest::Client::builder())
        .build()
        .expect("client");
    // Port 9 is not served, so a direct connection cannot succeed.
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client.get("http://127.0.0.1:9/probe").send(),
    )
    .await
    .expect("the request timed out")
    .expect("the request never reached the proxy");

    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");

    server.await.unwrap();
    let request = seen.lock().unwrap().clone().expect("proxy saw a request");
    assert!(
        request.starts_with("GET http://127.0.0.1:9/probe HTTP/1.1"),
        "expected an absolute-form proxy request, got: {request}"
    );

    proxy::set(ProxySettings::default());
}
