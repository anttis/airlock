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
    let _ = tokio::join!(server.write.shutdown(), container.write.shutdown());
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
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use tokio::io::AsyncWrite;
    use tokio::sync::oneshot;

    use super::*;

    struct FlushGatedWriter {
        pending: Vec<u8>,
        visible: Arc<Mutex<Vec<u8>>>,
    }

    struct ShutdownGatedWriter {
        started: Option<oneshot::Sender<()>>,
        release: oneshot::Receiver<()>,
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

    impl AsyncWrite for ShutdownGatedWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            match Pin::new(&mut self.release).poll(cx) {
                Poll::Ready(_) => Poll::Ready(Ok(())),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    fn shutdown_gated_writer() -> (
        ShutdownGatedWriter,
        oneshot::Receiver<()>,
        oneshot::Sender<()>,
    ) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        (
            ShutdownGatedWriter {
                started: Some(started_tx),
                release: release_rx,
            },
            started_rx,
            release_tx,
        )
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

    #[tokio::test]
    async fn relay_starts_both_shutdowns_together() {
        let (container_writer, container_started, release_container) = shutdown_gated_writer();
        let (server_writer, server_started, release_server) = shutdown_gated_writer();
        let container = io::Transport {
            read: Box::new(tokio::io::empty()),
            write: Box::new(container_writer),
            h2: false,
        };
        let server = io::Transport {
            read: Box::new(tokio::io::empty()),
            write: Box::new(server_writer),
            h2: false,
        };

        let mut relay = Box::pin(relay(container, server));
        let both_started = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            container_started.await.unwrap();
            server_started.await.unwrap();
        });
        tokio::select! {
            result = both_started => result.expect("relay serialized writer shutdowns"),
            () = &mut relay => panic!("relay finished before writer shutdowns were released"),
        }

        let _ = release_container.send(());
        let _ = release_server.send(());
        relay.await;
    }
}
