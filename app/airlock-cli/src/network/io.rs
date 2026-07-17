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

mod rpc;
pub use rpc::RpcTransport;

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
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn http_transport_types_are_send() {
        assert_send::<RpcTransport>();
        assert_send::<PrefixedRead>();
        assert_send::<Transport>();
    }
}
