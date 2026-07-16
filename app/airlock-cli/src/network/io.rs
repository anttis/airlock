//! I/O primitives for bridging RPC byte streams with tokio async I/O.
//!
//! The network proxy needs to treat both real TCP sockets and Cap'n Proto
//! RPC channels as `AsyncRead + AsyncWrite`. This module provides the
//! adapters that make that possible.

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use airlock_common::network_capnp::tcp_sink;
use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;

/// Boxed read half for type-erased async streams.
pub type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
/// Boxed write half for type-erased async streams.
pub type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

/// A connection endpoint with boxed read/write streams and h2 flag.
pub struct Transport {
    pub read: BoxRead,
    pub write: BoxWrite,
    pub h2: bool,
}

impl Transport {
    /// A black-hole transport used as the "server" side when policy denies
    /// the connection: reads return EOF, writes are discarded. Paired with
    /// [`super::tcp::relay`] it causes the relay to tear the connection
    /// down immediately; paired with [`super::http::relay`] it short-
    /// circuits at the `!target.allowed` branch before the sender is used.
    pub fn null() -> Self {
        Self {
            read: Box::new(tokio::io::empty()),
            write: Box::new(tokio::io::sink()),
            h2: false,
        }
    }
}

/// Prepend buffered bytes to an `AsyncRead` stream.
pub struct PrefixedRead {
    prefix: Bytes,
    inner: BoxRead,
}

impl PrefixedRead {
    pub fn new(prefix: Bytes, inner: BoxRead) -> Self {
        Self { prefix, inner }
    }
}

impl AsyncRead for PrefixedRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prefix.is_empty() {
            let n = self.prefix.len().min(buf.remaining());
            buf.put_slice(&self.prefix[..n]);
            self.prefix.advance(n);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

/// Bridges an mpsc channel + RPC sink into `AsyncRead + AsyncWrite`.
pub struct RpcTransport {
    prefix: Bytes,
    rx: mpsc::Receiver<Bytes>,
    tx: PollSender<Bytes>,
    pending: Bytes,
}

impl RpcTransport {
    /// Create a transport with an optional prefix (pre-read bytes), an mpsc
    /// receiver for incoming data, and an RPC sink for outgoing data.
    pub fn new(
        prefix: impl Into<Bytes>,
        rx: mpsc::Receiver<Bytes>,
        client_sink: tcp_sink::Client,
    ) -> Self {
        // Cap'n Proto clients are !Send because their RPC system lives on the
        // current LocalSet. Hyper upgrades, however, require their underlying
        // I/O object to be Send. Keep the capability in this local writer task
        // and expose only a Send channel through the AsyncWrite implementation.
        let (tx, mut write_rx) = mpsc::channel::<Bytes>(1);
        tokio::task::spawn_local(async move {
            while let Some(data) = write_rx.recv().await {
                let mut req = client_sink.send_request();
                req.get().set_data(&data);
                if req.send().await.is_err() {
                    break;
                }
            }

            let req = client_sink.close_request();
            let _ = req.send().promise.await;
        });

        Self {
            prefix: prefix.into(),
            rx,
            tx: PollSender::new(tx),
            pending: Bytes::new(),
        }
    }

    fn drain(src: &mut Bytes, buf: &mut ReadBuf<'_>) {
        let n = src.len().min(buf.remaining());
        buf.put_slice(&src[..n]);
        src.advance(n);
    }
}

impl AsyncRead for RpcTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prefix.is_empty() {
            Self::drain(&mut self.prefix, buf);
            return Poll::Ready(Ok(()));
        }
        if !self.pending.is_empty() {
            Self::drain(&mut self.pending, buf);
            return Poll::Ready(Ok(()));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(mut data)) => {
                Self::drain(&mut data, buf);
                if !data.is_empty() {
                    self.pending = data;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Ok(())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for RpcTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        match self.tx.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                self.tx
                    .send_item(Bytes::copy_from_slice(buf))
                    .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
                Poll::Ready(Ok(buf.len()))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into())),
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.tx.close();
        Poll::Ready(Ok(()))
    }
}

/// Shared error state between relay task and ChannelSink.
pub type RelayError = Rc<RefCell<Option<String>>>;

/// RPC interface for the supervisor to push container bytes into the channel.
pub struct ChannelSink {
    tx: RefCell<Option<mpsc::Sender<Bytes>>>,
    error: RelayError,
}

impl ChannelSink {
    /// Create a new sink with the given channel and shared error state.
    pub fn new(tx: mpsc::Sender<Bytes>, error: RelayError) -> Self {
        Self {
            tx: RefCell::new(Some(tx)),
            error,
        }
    }
}

impl tcp_sink::Server for ChannelSink {
    async fn send(self: Rc<Self>, params: tcp_sink::SendParams) -> Result<(), capnp::Error> {
        if let Some(err) = self.error.borrow().as_ref() {
            return Err(capnp::Error::failed(err.clone()));
        }
        let data = params.get()?.get_data()?;
        let tx = self.tx.borrow().clone();
        match tx.as_ref() {
            Some(tx) => {
                tx.send(Bytes::copy_from_slice(data)).await.map_err(|_| {
                    let err = self.error.borrow();
                    let msg = err.as_deref().unwrap_or("relay closed");
                    capnp::Error::failed(msg.to_string())
                })?;
            }
            None => {
                return Err(capnp::Error::failed("channel closed".to_string()));
            }
        }
        Ok(())
    }

    async fn close(
        self: Rc<Self>,
        _params: tcp_sink::CloseParams,
        _results: tcp_sink::CloseResults,
    ) -> Result<(), capnp::Error> {
        self.tx.borrow_mut().take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn http_transport_types_are_send() {
        assert_send::<RpcTransport>();
        assert_send::<PrefixedRead>();
        assert_send::<Transport>();
    }

    #[test]
    fn rpc_transport_orders_writes_before_close() {
        struct RecordingSink {
            writes: Rc<RefCell<Vec<Bytes>>>,
            closes: Rc<RefCell<usize>>,
            closed: Rc<tokio::sync::Notify>,
        }

        impl tcp_sink::Server for RecordingSink {
            async fn send(
                self: Rc<Self>,
                params: tcp_sink::SendParams,
            ) -> Result<(), capnp::Error> {
                let data = params.get()?.get_data()?;
                self.writes.borrow_mut().push(Bytes::copy_from_slice(data));
                Ok(())
            }

            async fn close(
                self: Rc<Self>,
                _params: tcp_sink::CloseParams,
                _results: tcp_sink::CloseResults,
            ) -> Result<(), capnp::Error> {
                *self.closes.borrow_mut() += 1;
                self.closed.notify_one();
                Ok(())
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let writes = Rc::new(RefCell::new(Vec::new()));
            let closes = Rc::new(RefCell::new(0));
            let closed = Rc::new(tokio::sync::Notify::new());
            let sink: tcp_sink::Client = capnp_rpc::new_client(RecordingSink {
                writes: writes.clone(),
                closes: closes.clone(),
                closed: closed.clone(),
            });
            let (guest_tx, guest_rx) = mpsc::channel(1);
            let mut transport = RpcTransport::new(Bytes::new(), guest_rx, sink);

            transport.write_all(b"first").await.unwrap();
            transport.write_all(b"second").await.unwrap();
            transport.shutdown().await.unwrap();
            drop(guest_tx);

            tokio::time::timeout(std::time::Duration::from_secs(1), closed.notified())
                .await
                .expect("RPC sink was not closed");
            assert_eq!(
                &*writes.borrow(),
                &[Bytes::from_static(b"first"), Bytes::from_static(b"second")]
            );
            assert_eq!(*closes.borrow(), 1);
        }));
    }
}
