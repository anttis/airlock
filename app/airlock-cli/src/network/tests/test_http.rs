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
                let (request, tunneled) = read_http_head(&mut stream).await;
                let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
                assert!(headers.contains("connection: upgrade\r\n"), "{headers}");
                assert!(headers.contains("upgrade: websocket\r\n"), "{headers}");
                assert!(
                    headers.contains("x-airlock-upgrade: injected\r\n"),
                    "{headers}"
                );

                write_websocket_upgrade(&mut stream, b"server-first").await;
                stream.write_all(&tunneled).await.unwrap();
                echo_until_eof(&mut stream).await;
                let _ = closed_tx.send(());
            });

            let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
                .await
                .unwrap();
            let mut handshake = websocket_request(addr.port(), "/responses");
            handshake.extend_from_slice(b"client-first");
            conn.send(&handshake).await;

            let response = conn.recv_bytes(500).await;
            assert_websocket_response(&response);
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
            conn.send(&websocket_request(addr.port(), "/")).await;
            let response = conn.recv(3000).await;
            assert!(response.contains("502 Bad Gateway"), "{response}");
            assert!(
                response.contains("invalid upstream HTTP upgrade"),
                "{response}"
            );
        },
    );
}

#[test]
fn rejected_upgrade_can_be_followed_by_valid_upgrade() {
    run_plain(|proxy| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            let (first_request, trailing) = read_http_head(&mut stream).await;
            assert!(trailing.is_empty());
            assert!(String::from_utf8_lossy(&first_request).starts_with("GET /reject "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let (second_request, trailing) = read_http_head(&mut stream).await;
            assert!(String::from_utf8_lossy(&second_request).starts_with("GET /accept "));
            write_websocket_upgrade(&mut stream, b"accepted").await;
            stream.write_all(&trailing).await.unwrap();
            echo_until_eof(&mut stream).await;
        });

        let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
            .await
            .unwrap();
        conn.send(&websocket_request(addr.port(), "/reject")).await;
        let rejected = conn.recv(500).await;
        assert!(rejected.contains("200 OK"), "{rejected}");

        let mut accepted = websocket_request(addr.port(), "/accept");
        accepted.extend_from_slice(b"coalesced-client-bytes");
        conn.send(&accepted).await;
        let response = conn.recv_bytes(500).await;
        assert_websocket_response(&response);
        assert!(
            response
                .windows(b"accepted".len())
                .any(|w| w == b"accepted"),
            "server bytes coalesced with the second response were lost"
        );
        assert!(
            response
                .windows(b"coalesced-client-bytes".len())
                .any(|w| w == b"coalesced-client-bytes"),
            "client bytes coalesced with the second request were lost"
        );

        conn.close().await;
    });
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
                let _ = read_http_head(&mut stream).await;
                write_websocket_upgrade(&mut stream, &[]).await;

                let mut payload = [0u8; 16];
                stream.read_exact(&mut payload).await.unwrap();
                assert_eq!(&payload, b"close-after-echo");
                stream.write_all(&payload).await.unwrap();
                stream.shutdown().await.unwrap();
            });

            let mut conn = TestConnection::connect(&proxy, "127.0.0.1", addr.port())
                .await
                .unwrap();
            conn.send(&websocket_request(addr.port(), "/")).await;
            let response = conn.recv_bytes(500).await;
            assert_websocket_response(&response);

            conn.send(b"close-after-echo").await;
            assert_eq!(conn.recv_bytes(500).await, b"close-after-echo".as_slice());
            conn.wait_closed(1000).await;

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
