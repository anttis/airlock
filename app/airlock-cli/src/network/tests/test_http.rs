use airlock_common::network_capnp::network_proxy;
use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::helpers::*;

fn with_noop_middleware<F, Fut>(f: F)
where
    F: FnOnce(network_proxy::Client) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    run_network(
        vec!["*".into()],
        vec![("noop", "-- triggers HTTP detection")],
        f,
    );
}

#[test]
fn http_detection_with_middleware() {
    with_noop_middleware(|proxy| async move {
        let addr = serve(Router::new().route(
            "/{*path}",
            get(|Path(p): Path<String>| async move { format!("path={p}") }),
        ))
        .await;
        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        let resp = conn.roundtrip(&http_get(addr.port(), "/test-path")).await;
        assert!(resp.contains("200"), "expected 200, got: {resp}");
        assert!(
            resp.contains("path=test-path"),
            "expected path echo, got: {resp}"
        );
    });
}

#[test]
fn http_without_middleware_raw_relay() {
    run_plain(|proxy| async move {
        let addr = serve(Router::new().route("/", get(|| async { "raw-relay" }))).await;
        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        let resp = conn.roundtrip(&http_get(addr.port(), "/")).await;
        assert!(resp.contains("raw-relay"), "expected body, got: {resp}");
    });
}

#[test]
fn http_post_through_middleware() {
    with_noop_middleware(|proxy| async move {
        let addr =
            serve(Router::new().route("/echo", post(|body: String| async move { body }))).await;
        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        let resp = conn
            .roundtrip(&http_post(addr.port(), "/echo", "payload"))
            .await;
        assert!(resp.contains("200"), "expected 200, got: {resp}");
        assert!(
            resp.contains("payload"),
            "expected echoed body, got: {resp}"
        );
    });
}

#[test]
fn http_preserves_status_codes() {
    with_noop_middleware(|proxy| async move {
        let addr = serve(Router::new().route(
            "/not-found",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "nope") }),
        ))
        .await;
        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        let resp = conn.roundtrip(&http_get(addr.port(), "/not-found")).await;
        assert!(resp.contains("404"), "expected 404, got: {resp}");
    });
}

#[test]
fn http_preserves_response_headers() {
    with_noop_middleware(|proxy| async move {
        let addr =
            serve(Router::new().route("/", get(|| async { ([("x-custom", "test-value")], "ok") })))
                .await;
        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        let resp = conn.roundtrip(&http_get(addr.port(), "/")).await;
        assert!(
            resp.contains("x-custom: test-value"),
            "expected custom header, got: {resp}"
        );
    });
}

#[test]
fn http_keepalive_multiple_requests() {
    with_noop_middleware(|proxy| async move {
        let addr = serve(Router::new().route(
            "/{*path}",
            get(|Path(p): Path<String>| async move { format!("path={p}") }),
        ))
        .await;

        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();

        // First request without Connection: close (keep-alive by default in HTTP/1.1)
        conn.send(http_get_keepalive(addr.port(), "/first").as_bytes())
            .await;
        let resp = conn.recv(500).await;
        assert!(resp.contains("200"), "first request: {resp}");
        assert!(resp.contains("path=first"), "first body: {resp}");

        // Second request on the same connection
        conn.send(http_get_keepalive(addr.port(), "/second").as_bytes())
            .await;
        let resp = conn.recv(500).await;
        assert!(resp.contains("200"), "second request: {resp}");
        assert!(resp.contains("path=second"), "second body: {resp}");
    });
}

/// When the upstream server closes the connection, the relay should
/// propagate the close to the guest so it can reconnect naturally
/// instead of getting 502 "operation was canceled" on subsequent requests.
#[test]
fn http_upstream_close_propagates_to_guest() {
    with_noop_middleware(|proxy| async move {
        let (addr, shutdown_tx) =
            serve_with_shutdown(Router::new().route("/", get(|| async { "hello" }))).await;

        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();

        // First request succeeds
        conn.send(http_get_keepalive(addr.port(), "/").as_bytes())
            .await;
        let resp = conn.recv(500).await;
        assert!(
            resp.contains("200"),
            "initial request should succeed: {resp}"
        );

        // Shut down the upstream server
        let _ = shutdown_tx.send(());
        // Give the server time to close
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // The guest connection should close (recv returns empty or connection
        // closed). If the old bug were present, we'd get a 502 instead.
        let resp = conn.recv(1000).await;
        assert!(
            !resp.contains("502"),
            "should not get 502 after upstream close: {resp}"
        );
    });
}

#[test]
fn websocket_upgrade_runs_middleware_and_relays_bytes() {
    run_with_config_and_events(
        TestNetworkConfig {
            middleware_scripts: vec![(
                "upgrade header",
                r#"req:setHeader("x-airlock-upgrade", "injected")"#,
            )],
            ..Default::default()
        },
        |proxy, _log, _ca, mut events| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0, "client closed before upgrade request");
                    request.extend_from_slice(&chunk[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }

                let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let tunneled = request.split_off(header_end);
                let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
                assert!(headers.contains("connection: upgrade\r\n"), "{headers}");
                assert!(headers.contains("upgrade: websocket\r\n"), "{headers}");
                assert!(
                    headers.contains("x-airlock-upgrade: injected\r\n"),
                    "{headers}"
                );

                stream
                    .write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\nserver-first",
                    )
                    .await
                    .unwrap();
                stream.write_all(&tunneled).await.unwrap();

                let mut buf = [0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    stream.write_all(&buf[..n]).await.unwrap();
                }
                let _ = closed_tx.send(());
            });

            let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
                .await
                .unwrap();
            let mut handshake = format!(
                "GET /responses HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                addr.port()
            )
            .into_bytes();
            handshake.extend_from_slice(b"client-first");
            conn.send(&handshake).await;

            let response = conn.recv_bytes(500).await;
            let response_text = String::from_utf8_lossy(&response).to_ascii_lowercase();
            assert!(
                response_text.contains("101 switching protocols"),
                "{response_text}"
            );
            assert!(
                response_text.contains("connection: upgrade\r\n"),
                "{response_text}"
            );
            assert!(
                response_text.contains("upgrade: websocket\r\n"),
                "{response_text}"
            );
            assert!(
                response
                    .windows(b"server-first".len())
                    .any(|w| w == b"server-first"),
                "server bytes coalesced with 101 were lost"
            );
            assert!(
                response
                    .windows(b"client-first".len())
                    .any(|w| w == b"client-first"),
                "client bytes coalesced with request were lost"
            );

            conn.send(b"websocket-payload").await;
            assert_eq!(conn.recv_bytes(500).await, b"websocket-payload".as_slice());

            while let Ok(event) = events.try_recv() {
                assert!(
                    !matches!(event, airlock_monitor::NetworkEvent::Disconnect(_)),
                    "connection was marked disconnected while upgrade tunnel was active"
                );
            }

            conn.close().await;
            tokio::time::timeout(std::time::Duration::from_secs(1), closed_rx)
                .await
                .expect("upstream did not see guest close")
                .unwrap();

            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match events.recv().await {
                        Ok(airlock_monitor::NetworkEvent::Disconnect(_)) => break,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            panic!("network event channel closed before disconnect")
                        }
                    }
                }
            })
            .await
            .expect("disconnect event was not emitted after tunnel close");
        },
    );
}

#[test]
fn synthetic_websocket_upgrade_is_rejected() {
    run_network(
        vec!["*".into()],
        vec![(
            "fabricate upgrade",
            r#"
                local res = req:send()
                res.status = 101
                res:setHeader("connection", "Upgrade")
                res:setHeader("upgrade", "websocket")
            "#,
        )],
        |proxy| async move {
            let addr = serve(Router::new().route("/", get(|| async { "not-an-upgrade" }))).await;
            let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
                .await
                .unwrap();
            let request = format!(
                "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                addr.port()
            );
            let response = conn.roundtrip(&request).await;
            assert!(response.contains("502 Bad Gateway"), "{response}");
            assert!(
                response.contains("invalid upstream HTTP upgrade"),
                "{response}"
            );
        },
    );
}

#[test]
fn websocket_upstream_close_ends_tunnel() {
    run_with_config_and_events(
        TestNetworkConfig::default(),
        |proxy, _log, _ca, mut events| async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0);
                    request.extend_from_slice(&chunk[..n]);
                }
                stream
                    .write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                    )
                    .await
                    .unwrap();

                let mut payload = [0u8; 16];
                stream.read_exact(&mut payload).await.unwrap();
                assert_eq!(&payload, b"close-after-echo");
                stream.write_all(&payload).await.unwrap();
                stream.shutdown().await.unwrap();
            });

            let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
                .await
                .unwrap();
            let request = format!(
                "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                addr.port()
            );
            conn.send(request.as_bytes()).await;
            let response = conn.recv(500).await;
            assert!(response.contains("101 Switching Protocols"), "{response}");

            conn.send(b"close-after-echo").await;
            assert_eq!(conn.recv_bytes(500).await, b"close-after-echo".as_slice());
            assert!(
                conn.recv_bytes(1000).await.is_empty(),
                "guest connection stayed open after upstream closed"
            );

            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match events.recv().await {
                        Ok(airlock_monitor::NetworkEvent::Disconnect(_)) => break,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            panic!("network event channel closed before disconnect")
                        }
                    }
                }
            })
            .await
            .expect("disconnect event was not emitted after upstream close");
        },
    );
}
