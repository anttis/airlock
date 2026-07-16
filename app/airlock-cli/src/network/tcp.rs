use airlock_common::network_capnp::tcp_sink;
use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::debug;

use super::io;

/// Wrap the RPC channel to the guest in a `Transport` without touching the
/// real server. Used on both the allow path (paired with `connect_server`)
/// and the deny path (where we hand the transport to a 403-serving hyper
/// instance instead).
pub fn container_transport(
    first: Bytes,
    rx: mpsc::Receiver<Bytes>,
    client_sink: tcp_sink::Client,
) -> io::Transport {
    let rpc_io = io::RpcTransport::new(first, rx, client_sink);
    let (cr, cw) = tokio::io::split(rpc_io);
    io::Transport {
        read: Box::new(cr),
        write: Box::new(cw),
        h2: false,
    }
}

/// Open a plain TCP socket to the real server.
pub async fn connect_server(addr: &str) -> anyhow::Result<io::Transport> {
    debug!("plain tcp: {addr}");
    let server = tokio::time::timeout(
        crate::constants::TCP_CONNECT_TIMEOUT,
        TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("connection timed out: {addr}"))??;
    let (sr, sw) = server.into_split();
    Ok(io::Transport {
        read: Box::new(sr),
        write: Box::new(sw),
        h2: false,
    })
}

/// Bidirectional relay between two transports.
/// When either direction closes, both sides are fully shut down.
pub async fn relay(mut container: io::Transport, mut server: io::Transport) {
    // When either direction finishes, shut down everything
    tokio::select! {
        () = relay_direction(&mut container.read, &mut server.write) => {}
        () = relay_direction(&mut server.read, &mut container.write) => {}
    }
    let _ = server.write.shutdown().await;
    let _ = container.write.shutdown().await;
}

async fn relay_direction(reader: &mut io::BoxRead, writer: &mut io::BoxWrite) {
    let mut buf = vec![0u8; airlock_common::RELAY_CHUNK_SIZE];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer.write_all(&buf[..n]).await.is_err() || writer.flush().await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use tokio::io::AsyncWrite;

    use super::*;

    struct FlushGatedWriter {
        pending: Vec<u8>,
        visible: Arc<Mutex<Vec<u8>>>,
    }

    impl AsyncWrite for FlushGatedWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.pending.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            let pending = std::mem::take(&mut self.pending);
            self.visible.lock().unwrap().extend_from_slice(&pending);
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            self.poll_flush(cx)
        }
    }

    #[tokio::test]
    async fn relay_flushes_each_chunk() {
        let visible = Arc::new(Mutex::new(Vec::new()));
        let writer = FlushGatedWriter {
            pending: Vec::new(),
            visible: visible.clone(),
        };
        let (pending_stream, _peer) = tokio::io::duplex(64);
        let (pending_read, _) = tokio::io::split(pending_stream);

        let container = io::Transport {
            read: Box::new(std::io::Cursor::new(b"interactive frame".to_vec())),
            write: Box::new(tokio::io::sink()),
            h2: false,
        };
        let server = io::Transport {
            read: Box::new(pending_read),
            write: Box::new(writer),
            h2: false,
        };

        relay(container, server).await;

        assert_eq!(&*visible.lock().unwrap(), b"interactive frame");
    }
}
