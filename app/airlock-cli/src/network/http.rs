//! HTTP request interception via hyper.
//!
//! When the first bytes from the container look like HTTP, we hand off
//! to hyper's auto-detecting HTTP server (h1/h2) and h1/h2 client.
//! For each request, Lua scripts run and the (possibly modified) request
//! is forwarded via hyper client. Bodies are streamed, not buffered.

pub mod body;
mod executor;
pub mod middleware;
mod senders;

use std::cell::RefCell;
use std::rc::Rc;

use http_body_util::{Either, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONNECTION, UPGRADE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Version};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tracing::{debug, trace};

use crate::network::http::executor::LocalExecutor;
use crate::network::http::senders::{H1Sender, H2Sender, RequestSender};
use crate::network::target::ResolvedTarget;
use crate::network::{DenyReporter, io};

const MAX_DETECT_SIZE: usize = 4096;

/// Peek at the first bytes to detect HTTP.
///
/// Reads up to 4KB or until the first `\r\n`, then checks if the line
/// matches `METHOD path HTTP/x.y\r\n`. Returns `Ok(buf)` if HTTP,
/// `Err(buf)` if not.
pub async fn detect(reader: &mut (impl AsyncRead + Unpin)) -> Result<Bytes, Bytes> {
    let mut buf = bytes::BytesMut::zeroed(MAX_DETECT_SIZE);
    let mut len = 0;
    loop {
        let n = match reader.read(&mut buf[len..]).await {
            Ok(0) | Err(_) => {
                trace!("stream closed before HTTP detection ({len} bytes)");
                buf.truncate(len);
                return Err(buf.freeze());
            }
            Ok(n) => n,
        };
        len += n;

        if let Some(pos) = buf[..len].windows(2).position(|w| w == b"\r\n") {
            buf.truncate(len);
            return if is_http_request_line(&buf[..pos]) {
                debug!("detected HTTP request line");
                Ok(buf.freeze())
            } else {
                trace!(
                    "first line is not HTTP: {:?}",
                    String::from_utf8_lossy(&buf[..pos.min(80)])
                );
                Err(buf.freeze())
            };
        }

        if len >= MAX_DETECT_SIZE {
            trace!("no linebreak in first {MAX_DETECT_SIZE}B, not HTTP");
            buf.truncate(len);
            return Err(buf.freeze());
        }
    }
}

type ResponseBody = Either<Incoming, Full<Bytes>>;

struct PendingUpgrade {
    container: hyper::upgrade::OnUpgrade,
    server: hyper::upgrade::OnUpgrade,
}

enum UpgradeState {
    Idle,
    Requested,
    Accepted(PendingUpgrade),
    Rejected,
}

/// Run hyper HTTP proxy with middleware interception.
///
/// When `target.allowed` is false, `server` is a [`io::Transport::null`]
/// black hole. We still run a hyper server against the container so the
/// request headers are parsed and surfaced in the Requests sub-tab, but we
/// short-circuit with a 403 before touching the server transport.
pub async fn relay(
    container: io::Transport,
    server: io::Transport,
    target: ResolvedTarget,
    events: tokio::sync::broadcast::Sender<airlock_monitor::NetworkEvent>,
    deny_reporter: Rc<DenyReporter>,
) -> anyhow::Result<()> {
    let client_io = hyper_util::rt::TokioIo::new(tokio::io::join(container.read, container.write));

    if !target.allowed {
        let target_host = target.host.clone();
        let target_port = target.port;
        let deny_reporter = deny_reporter.clone();
        let service = service_fn(move |req: Request<Incoming>| {
            let events = events.clone();
            let target_host = target_host.clone();
            let deny_reporter = deny_reporter.clone();
            async move {
                emit_request_event(&events, &req, &target_host, target_port, false);
                deny_reporter.report();
                let body: ResponseBody =
                    Either::Right(Full::new(Bytes::from("denied by network policy\n")));
                Ok::<_, hyper::Error>(Response::builder().status(403).body(body).unwrap())
            }
        });
        return hyper_util::server::conn::auto::Builder::new(LocalExecutor)
            .serve_connection(client_io, service)
            .await
            .map_err(|e| anyhow::anyhow!("http deny: {e}"));
    }

    let server_io = hyper_util::rt::TokioIo::new(tokio::io::join(server.read, server.write));
    debug!("http proxy: server h2 = {}", server.h2);

    let (sender, upstream_conn): (Rc<dyn RequestSender>, _) = if server.h2 {
        let (sender, conn): (hyper::client::conn::http2::SendRequest<ResponseBody>, _) =
            hyper::client::conn::http2::handshake(LocalExecutor, server_io).await?;
        let handle = tokio::task::spawn_local(conn);
        debug!("h2 client handshake complete");
        (Rc::new(H2Sender(sender)), handle)
    } else {
        let (sender, conn): (hyper::client::conn::http1::SendRequest<ResponseBody>, _) =
            hyper::client::conn::http1::handshake(server_io).await?;
        let handle = tokio::task::spawn_local(conn.with_upgrades());
        debug!("h1 client handshake complete");
        (Rc::new(H1Sender(RefCell::new(sender))), handle)
    };

    let upgrade_state = Rc::new(RefCell::new(UpgradeState::Idle));
    let upgrade_notify = Rc::new(tokio::sync::Notify::new());
    let middleware = target.middleware;
    let target_host = target.host.clone();
    let target_port = target.port;
    let allowed = target.allowed;
    let service_upgrade_state = upgrade_state.clone();
    let service_upgrade_notify = upgrade_notify.clone();
    let service = service_fn(move |mut req: Request<Incoming>| {
        let sender = sender.clone();
        let middleware = middleware.clone();
        let events = events.clone();
        let target_host = target_host.clone();
        let deny_reporter = deny_reporter.clone();
        let upgrade_state = service_upgrade_state.clone();
        let upgrade_notify = service_upgrade_notify.clone();
        async move {
            emit_request_event(&events, &req, &target_host, target_port, allowed);
            let container_upgrade = is_upgrade_message(&req).then(|| {
                *upgrade_state.borrow_mut() = UpgradeState::Requested;
                hyper::upgrade::on(&mut req)
            });
            let upstream_accepted = Rc::new(std::cell::Cell::new(false));
            let send_accepted = upstream_accepted.clone();
            let result = middleware::run(req, &middleware, deny_reporter, move |req| {
                let sender = sender.clone();
                async move {
                    let resp = sender.send(req).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                    send_accepted.set(
                        resp.status() == StatusCode::SWITCHING_PROTOCOLS
                            && is_upgrade_message(&resp),
                    );
                    Ok(resp)
                }
            })
            .await;

            match result {
                Ok(mut resp) if resp.status() == StatusCode::SWITCHING_PROTOCOLS => {
                    let valid = container_upgrade.is_some()
                        && upstream_accepted.get()
                        && is_upgrade_message(&resp)
                        && matches!(resp.body(), Either::Left(_));
                    if valid {
                        let server_upgrade = hyper::upgrade::on(&mut resp);
                        *upgrade_state.borrow_mut() = UpgradeState::Accepted(PendingUpgrade {
                            container: container_upgrade.unwrap(),
                            server: server_upgrade,
                        });
                        upgrade_notify.notify_one();
                        Ok::<_, hyper::Error>(resp)
                    } else {
                        if container_upgrade.is_some() {
                            *upgrade_state.borrow_mut() = UpgradeState::Rejected;
                            upgrade_notify.notify_one();
                        }
                        debug!("rejected malformed or synthetic HTTP upgrade response");
                        Ok(bad_gateway("invalid upstream HTTP upgrade\n"))
                    }
                }
                Ok(resp) => {
                    if container_upgrade.is_some() {
                        *upgrade_state.borrow_mut() = UpgradeState::Rejected;
                        upgrade_notify.notify_one();
                    }
                    Ok::<_, hyper::Error>(resp)
                }
                Err(e) => {
                    if container_upgrade.is_some() {
                        *upgrade_state.borrow_mut() = UpgradeState::Rejected;
                        upgrade_notify.notify_one();
                    }
                    debug!("middleware error: {e}");
                    Ok(bad_gateway(&format!("{e}\n")))
                }
            }
        }
    });

    // Mirror upstream connection state: when the upstream server closes the
    // connection, gracefully shut down the guest-side server so the guest
    // sees a clean close and naturally reconnects. Without this, a stale
    // sender produces "operation was canceled" 502s for every subsequent
    // request on the same guest connection.
    let builder = hyper_util::server::conn::auto::Builder::new(LocalExecutor);
    let connection = builder.serve_connection_with_upgrades(client_io, service);
    let mut connection = std::pin::pin!(connection);
    let mut upstream_conn = upstream_conn;

    tokio::select! {
        result = &mut connection => {
            let result = result.map_err(|e| anyhow::anyhow!("http proxy: {e}"));
            let upgrade = take_upgrade(&upgrade_state);
            if let Some(upgrade) = upgrade {
                result?;
                upstream_conn.await
                    .map_err(|e| anyhow::anyhow!("upstream HTTP task: {e}"))??;
                relay_upgrade(upgrade).await
            } else {
                result
            }
        }
        upstream_result = &mut upstream_conn => {
            // An h1 connection future also completes when its protocol is
            // upgraded. If the request service is still deciding whether a
            // 101 is valid, keep driving it until the state is settled before
            // applying ordinary upstream-close behaviour.
            let result = loop {
                let notified = upgrade_notify.notified();
                let phase = match &*upgrade_state.borrow() {
                    UpgradeState::Idle => 0,
                    UpgradeState::Requested => 1,
                    UpgradeState::Accepted(_) => 2,
                    UpgradeState::Rejected => 3,
                };
                match phase {
                    1 => {
                        tokio::select! {
                            result = &mut connection => {
                                break result.map_err(|e| anyhow::anyhow!("http proxy: {e}"));
                            }
                            () = notified => {}
                        }
                    }
                    2 => {
                        break connection
                            .await
                            .map_err(|e| anyhow::anyhow!("http proxy upgrade: {e}"));
                    }
                    _ => {
                        debug!("upstream connection closed, shutting down guest connection");
                        connection.as_mut().graceful_shutdown();
                        break connection
                            .await
                            .map_err(|e| anyhow::anyhow!("http proxy shutdown: {e}"));
                    }
                }
            };
            let upgrade = take_upgrade(&upgrade_state);
            if let Some(upgrade) = upgrade {
                upstream_result
                    .map_err(|e| anyhow::anyhow!("upstream HTTP task: {e}"))??;
                result?;
                relay_upgrade(upgrade).await
            } else {
                result
            }
        }
    }
}

fn take_upgrade(state: &RefCell<UpgradeState>) -> Option<PendingUpgrade> {
    match std::mem::replace(&mut *state.borrow_mut(), UpgradeState::Idle) {
        UpgradeState::Accepted(upgrade) => Some(upgrade),
        _ => None,
    }
}

/// Relay the raw protocol after both HTTP/1.1 sides accept an upgrade.
async fn relay_upgrade(upgrade: PendingUpgrade) -> anyhow::Result<()> {
    let (container, server) = tokio::try_join!(upgrade.container, upgrade.server)?;
    debug!("HTTP upgrade complete, starting raw relay");

    let (cr, cw) = tokio::io::split(hyper_util::rt::TokioIo::new(container));
    let container = io::Transport {
        read: Box::new(cr),
        write: Box::new(cw),
        h2: false,
    };
    let server = upgraded_server_transport(server);
    relay_upgraded_bytes(container, server).await;
    Ok(())
}

/// Relay an upgraded protocol until either peer closes. Flush after every
/// chunk because TLS streams may otherwise retain small interactive frames.
async fn relay_upgraded_bytes(mut container: io::Transport, mut server: io::Transport) {
    let c2s = async {
        let mut buf = vec![0u8; airlock_common::RELAY_CHUNK_SIZE];
        loop {
            match container.read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if server.write.write_all(&buf[..n]).await.is_err()
                        || server.write.flush().await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    };

    let s2c = async {
        let mut buf = vec![0u8; airlock_common::RELAY_CHUNK_SIZE];
        loop {
            match server.read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if container.write.write_all(&buf[..n]).await.is_err()
                        || container.write.flush().await.is_err()
                    {
                        break;
                    }
                }
            }
        }
    };

    tokio::select! {
        () = c2s => {}
        () = s2c => {}
    }
    let _ = server.write.shutdown().await;
    let _ = container.write.shutdown().await;
}

/// Recover the upstream Tokio transport and explicitly retain Hyper's read
/// buffer. This matters when a TLS server puts its first WebSocket frame in
/// the same decrypted read as the `101` response.
fn upgraded_server_transport(upgraded: hyper::upgrade::Upgraded) -> io::Transport {
    type HyperIo = hyper_util::rt::TokioIo<tokio::io::Join<io::BoxRead, io::BoxWrite>>;

    match upgraded.downcast::<HyperIo>() {
        Ok(parts) => {
            let (sr, sw) = tokio::io::split(parts.io.into_inner());
            io::Transport {
                read: Box::new(io::PrefixedRead::new(parts.read_buf, Box::new(sr))),
                write: Box::new(sw),
                h2: false,
            }
        }
        Err(upgraded) => {
            let (sr, sw) = tokio::io::split(hyper_util::rt::TokioIo::new(upgraded));
            io::Transport {
                read: Box::new(sr),
                write: Box::new(sw),
                h2: false,
            }
        }
    }
}

fn bad_gateway(message: &str) -> Response<ResponseBody> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Either::Right(Full::new(Bytes::copy_from_slice(
            message.as_bytes(),
        ))))
        .unwrap()
}

/// HTTP/1.1 upgrade messages need both hop-by-hop headers. Hyper leaves
/// validating the application protocol named by `Upgrade` to the endpoints.
fn is_upgrade_message<B>(message: &impl UpgradeMessage<B>) -> bool {
    message.version() == Version::HTTP_11
        && message.headers().contains_key(UPGRADE)
        && message
            .headers()
            .get_all(CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
}

trait UpgradeMessage<B> {
    fn version(&self) -> Version;
    fn headers(&self) -> &hyper::HeaderMap;
}

impl<B> UpgradeMessage<B> for Request<B> {
    fn version(&self) -> Version {
        self.version()
    }

    fn headers(&self) -> &hyper::HeaderMap {
        self.headers()
    }
}

impl<B> UpgradeMessage<B> for Response<B> {
    fn version(&self) -> Version {
        self.version()
    }

    fn headers(&self) -> &hyper::HeaderMap {
        self.headers()
    }
}

/// Broadcast a `NetworkEvent::Request` describing this HTTP request. Silently
/// drops the event when there are no subscribers — and short-circuits *before*
/// cloning any request fields in that common case (non-monitor runs).
fn emit_request_event(
    events: &tokio::sync::broadcast::Sender<airlock_monitor::NetworkEvent>,
    req: &Request<Incoming>,
    target_host: &str,
    target_port: u16,
    allowed: bool,
) {
    if events.receiver_count() == 0 {
        return;
    }
    let method = req.method().to_string();
    let path = req
        .uri()
        .path_and_query()
        .map_or_else(|| "/".to_string(), ToString::to_string);
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect();
    let info = airlock_monitor::RequestInfo {
        timestamp: std::time::SystemTime::now(),
        method,
        path,
        host: target_host.to_string(),
        port: target_port,
        allowed,
        headers,
    };
    let _ = events.send(airlock_monitor::NetworkEvent::Request(std::sync::Arc::new(
        info,
    )));
}

/// Check if a line matches an HTTP request line or h2 connection preface.
fn is_http_request_line(line: &[u8]) -> bool {
    use std::sync::LazyLock;

    use regex::bytes::Regex;

    static H2_PREFACE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^PRI \* HTTP/2\.0$").unwrap());
    static H1_REQUEST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[A-Z]+ \S+ HTTP/\S+$").unwrap());

    H2_PREFACE.is_match(line) || H1_REQUEST.is_match(line)
}
