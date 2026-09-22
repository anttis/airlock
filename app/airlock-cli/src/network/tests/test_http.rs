use airlock_common::network_capnp::network_proxy;
use axum::Router;
use axum::extract::Path;
use axum::routing::{get, post};
use tokio::io::AsyncWriteExt;

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

// ── HTTP/1.1 Upgrade (WebSocket, CONNECT) ───────────────

async fn upgrade_stream(proxy: &network_proxy::Client) -> (RpcStream, u16) {
    let addr = serve_upgrade_echo().await;
    let conn = TestConnection::connect(proxy, "127.0.0.1", addr.port())
        .await
        .unwrap();
    (conn.into_stream(), addr.port())
}

#[test]
fn websocket_upgrade_relays_raw_bytes() {
    run_plain(|proxy| async move {
        let (mut stream, port) = upgrade_stream(&proxy).await;
        assert_raw_relay(
            &mut stream,
            &websocket_handshake(port, true),
            "HTTP/1.1 101",
        )
        .await;
    });
}

#[test]
fn websocket_upgrade_through_middleware() {
    with_noop_middleware(|proxy| async move {
        let (mut stream, port) = upgrade_stream(&proxy).await;
        assert_raw_relay(
            &mut stream,
            &websocket_handshake(port, true),
            "HTTP/1.1 101",
        )
        .await;
    });
}

#[test]
fn connect_tunnel_relays_raw_bytes() {
    run_plain(|proxy| async move {
        let (mut stream, port) = upgrade_stream(&proxy).await;
        let request =
            format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
        assert_raw_relay(&mut stream, &request, "HTTP/1.1 200").await;
    });
}

#[test]
fn connect_answered_204_is_not_a_switch() {
    // hyper's client keeps a CONNECT connection alive on 204 (no switch),
    // and the upstream here never closes it. Treating 204 as a switch
    // would leave the relay waiting on that idle connection forever.
    run_plain(|proxy| async move {
        let (mut stream, port) = upgrade_stream(&proxy).await;
        let request = format!(
            "CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Reply: 204\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let resp = read_until_eof(&mut stream).await;
        assert!(
            resp.starts_with("HTTP/1.1 204"),
            "expected 204, got: {resp}"
        );
        assert!(
            resp.to_lowercase().contains("connection: close"),
            "expected Connection: close, got: {resp}"
        );
    });
}

#[test]
fn websocket_upgrade_rejected_by_upstream_closes_guest_connection() {
    run_plain(|proxy| async move {
        let (mut stream, port) = upgrade_stream(&proxy).await;
        stream
            .write_all(websocket_handshake(port, false).as_bytes())
            .await
            .unwrap();
        // The upstream connection is spent after an upgrade attempt, so the
        // reply must carry `Connection: close` and the stream must end.
        let resp = read_until_eof(&mut stream).await;
        assert!(
            resp.starts_with("HTTP/1.1 400"),
            "expected 400, got: {resp}"
        );
        assert!(
            resp.to_lowercase().contains("connection: close"),
            "expected Connection: close, got: {resp}"
        );
        assert!(resp.ends_with("not-upgrade"), "body: {resp}");
    });
}

#[test]
fn websocket_upgrade_forged_by_middleware_is_refused() {
    // The upstream says 400; a script rewrites it to 101. There is no
    // upstream stream to relay, so the guest must get a 502 and a close,
    // not a 101 followed by silence.
    run_network(
        vec!["*".into()],
        vec![("forge 101", "res.status = 101")],
        |proxy| async move {
            let (mut stream, port) = upgrade_stream(&proxy).await;
            stream
                .write_all(websocket_handshake(port, false).as_bytes())
                .await
                .unwrap();
            let resp = read_until_eof(&mut stream).await;
            assert!(
                resp.starts_with("HTTP/1.1 502"),
                "expected 502, got: {resp}"
            );
            assert!(
                resp.to_lowercase().contains("connection: close"),
                "expected Connection: close, got: {resp}"
            );
        },
    );
}
