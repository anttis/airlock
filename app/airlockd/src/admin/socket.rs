//! Local socket of the `airlock` MCP server.
//!
//! Serves the `airlock` MCP server on a Unix socket in the container. Each
//! connection is one MCP session with newline-delimited JSON-RPC messages,
//! the same framing as the MCP stdio transport. Agents connect through the
//! stdio adapter.
//!
//! The kernel gives the user of each client at connect time. That user is
//! the identity of the connection, and its commands run as that user. No
//! value from the client can change it. Also limits the number of open
//! connections and the time to the first message.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::caller::PeerId;
use super::capabilities::Capabilities;
use super::mcp::{AdminTools, GrantBridge};

/// Maximum number of open connections. Any guest user can connect, so this
/// limits the file descriptors that clients can hold in PID 1.
const MAX_CONNECTIONS: usize = 64;

/// Time for a client to complete the MCP initialization.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Serve the MCP socket in the container in a background task. Does
/// nothing if `/run/airlock` is not the tmpfs of airlockd, because guest
/// users could then replace the socket.
/// Args:
///  - `grants`: Handle to the grant task, or `None` if the host network
///    service is not available
///  - `caps`: Agent features that the host enabled
pub fn start(grants: Option<GrantBridge>, caps: Capabilities) {
    if !crate::bridge::bridge_tmpfs_mounted() {
        warn!("MCP socket not served: /run/airlock is not the tmpfs of airlockd");
        return;
    }
    let path = format!(
        "{}{}",
        crate::sandbox_ns::ROOTFS,
        airlock_common::MCP_SOCKET
    );
    match bind(Path::new(&path)) {
        Ok(listener) => {
            info!("MCP socket at {}", airlock_common::MCP_SOCKET);
            tokio::task::spawn_local(serve(listener, grants, caps));
        }
        Err(e) => warn!("MCP socket not served: {e:#}"),
    }
}

/// Bind the socket with mode 0666, so that all guest users can connect.
/// The peer credentials authenticate the clients, not the file mode.
/// Removes an old file at the path first.
/// Args:
///  - `path`: Socket path as this process sees it
///
/// Returns:
///   The listener, or an error if the bind fails.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).map_err(|e| anyhow::anyhow!("bind {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Accept clients and serve one MCP session for each. Runs until the
/// listener fails. Call inside a `LocalSet`.
/// Args:
///  - `listener`: Bound socket
///  - `grants`, `caps`: Same as in [`start`]
pub async fn serve(listener: UnixListener, grants: Option<GrantBridge>, caps: Capabilities) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                warn!("MCP socket: accept failed: {e}");
                return;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            warn!("MCP socket: connection refused, {MAX_CONNECTIONS} connections are open");
            continue;
        };
        let grants = grants.clone();
        tokio::task::spawn_local(async move {
            serve_client(stream, grants, caps).await;
            drop(slot);
        });
    }
}

/// Serve one MCP session. The kernel IDs of the client are the identity
/// of the session.
async fn serve_client(stream: UnixStream, grants: Option<GrantBridge>, caps: Capabilities) {
    let peer = match stream.peer_cred() {
        Ok(cred) => PeerId {
            uid: cred.uid(),
            gid: cred.gid(),
            pid: cred.pid(),
        },
        Err(e) => {
            warn!("MCP socket: no peer credentials: {e}");
            return;
        }
    };
    let id = format!("uid {} pid {:?}", peer.uid, peer.pid);
    let tools = AdminTools::for_caller(grants, caps, peer);
    let running =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, rmcp::serve_server(tools, stream)).await {
            Ok(Ok(running)) => running,
            Ok(Err(e)) => {
                debug!("MCP socket: {id}: initialization failed: {e}");
                return;
            }
            Err(_) => {
                debug!("MCP socket: {id}: initialization timed out");
                return;
            }
        };
    info!("MCP socket: {id}: session open");
    // The session ends when the client closes the connection. Then the
    // service cancels the open requests, and their commands stop.
    let reason = running.waiting().await;
    info!("MCP socket: {id}: session closed ({reason:?})");
}
