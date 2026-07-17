//! Tokio I/O adapter for the Cap'n Proto byte-stream transport.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use airlock_common::network_capnp::tcp_sink;
use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::PollSender;

/// Bridges an mpsc channel + RPC sink into `AsyncRead + AsyncWrite`.
pub struct RpcTransport {
    prefix: Bytes,
    rx: mpsc::Receiver<Bytes>,
    tx: PollSender<WriterCommand>,
    writer: Option<tokio::task::JoinHandle<io::Result<()>>>,
    writer_state: WriterState,
    pending: Bytes,
}

enum WriterCommand {
    Write(Bytes),
    Flush(oneshot::Sender<()>),
}

enum WriterState {
    Open,
    Flushing(oneshot::Receiver<()>),
    ShuttingDown,
    Finished(Result<(), WriterFailure>),
}

#[derive(Clone)]
struct WriterFailure {
    kind: io::ErrorKind,
    message: String,
}

impl WriterFailure {
    fn from_error(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    fn to_error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
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
        let (tx, write_rx) = mpsc::channel(1);
        let writer = tokio::task::spawn_local(run_writer(write_rx, client_sink));

        Self {
            prefix: prefix.into(),
            rx,
            tx: PollSender::new(tx),
            writer: Some(writer),
            writer_state: WriterState::Open,
            pending: Bytes::new(),
        }
    }

    fn drain(src: &mut Bytes, buf: &mut ReadBuf<'_>) {
        let n = src.len().min(buf.remaining());
        buf.put_slice(&src[..n]);
        src.advance(n);
    }

    fn poll_pending_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let state = std::mem::replace(&mut self.writer_state, WriterState::Open);
        let WriterState::Flushing(mut flushed) = state else {
            self.writer_state = state;
            return Poll::Ready(Ok(()));
        };

        match Pin::new(&mut flushed).poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "RPC writer stopped before flushing",
            ))),
            Poll::Pending => {
                self.writer_state = WriterState::Flushing(flushed);
                Poll::Pending
            }
        }
    }

    fn poll_writer(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let WriterState::Finished(result) = &self.writer_state {
            return Poll::Ready(match result {
                Ok(()) => Ok(()),
                Err(error) => Err(error.to_error()),
            });
        }

        let Some(writer) = self.writer.as_mut() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "RPC writer task is missing",
            )));
        };
        let result = match Pin::new(writer).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(result)) => result.map_err(|error| WriterFailure::from_error(&error)),
            Poll::Ready(Err(error)) => Err(WriterFailure {
                kind: io::ErrorKind::BrokenPipe,
                message: format!("RPC writer task failed: {error}"),
            }),
        };

        self.writer = None;
        self.writer_state = WriterState::Finished(result.clone());
        Poll::Ready(match result {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_error()),
        })
    }
}

async fn run_writer(
    mut commands: mpsc::Receiver<WriterCommand>,
    client_sink: tcp_sink::Client,
) -> io::Result<()> {
    let send_result = loop {
        match commands.recv().await {
            Some(WriterCommand::Write(data)) => {
                let mut req = client_sink.send_request();
                req.get().set_data(&data);
                if let Err(error) = req.send().await {
                    break Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        format!("RPC sink write failed: {error}"),
                    ));
                }
            }
            Some(WriterCommand::Flush(flushed)) => {
                let _ = flushed.send(());
            }
            None => break Ok(()),
        }
    };

    // Drop queued commands immediately after a write failure so pending flush
    // operations observe the broken writer while the close request runs.
    drop(commands);
    let close_result = client_sink
        .close_request()
        .send()
        .promise
        .await
        .map(|_| ())
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("RPC sink close failed: {error}"),
            )
        });

    send_result.and(close_result)
}

impl AsyncRead for RpcTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        if matches!(self.writer_state, WriterState::Flushing(_)) {
            // A flush future may have been dropped after queuing its barrier.
            // Finish that barrier before accepting a later write so it cannot
            // be mistaken for confirmation of the later write.
            match self.poll_pending_flush(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        }
        if !matches!(self.writer_state, WriterState::Open) {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }

        match self.tx.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                self.tx
                    .send_item(WriterCommand::Write(Bytes::copy_from_slice(buf)))
                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
                Poll::Ready(Ok(buf.len()))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if matches!(self.writer_state, WriterState::Flushing(_)) {
            return self.poll_pending_flush(cx);
        }
        if !matches!(self.writer_state, WriterState::Open) {
            return self.poll_writer(cx);
        }

        match self.tx.poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                let (flushed, flushed_rx) = oneshot::channel();
                self.tx
                    .send_item(WriterCommand::Flush(flushed))
                    .map_err(|_| io::ErrorKind::BrokenPipe)?;
                self.writer_state = WriterState::Flushing(flushed_rx);
                self.poll_pending_flush(cx)
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !matches!(
            self.writer_state,
            WriterState::ShuttingDown | WriterState::Finished(_)
        ) {
            // Closing the only sender makes the writer drain queued commands,
            // close the RPC sink, and then complete its task.
            self.tx.close();
            self.writer_state = WriterState::ShuttingDown;
        }
        self.poll_writer(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::sync::Semaphore;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    struct GatedSink {
        writes: Rc<RefCell<Vec<Bytes>>>,
        closes: Rc<Cell<usize>>,
        send_attempts: Cell<usize>,
        send_started: mpsc::UnboundedSender<()>,
        close_started: mpsc::UnboundedSender<()>,
        send_gate: Arc<Semaphore>,
        close_gate: Arc<Semaphore>,
        fail_send_at: Option<usize>,
        fail_close: bool,
    }

    struct SinkHarness {
        writes: Rc<RefCell<Vec<Bytes>>>,
        closes: Rc<Cell<usize>>,
        send_started: mpsc::UnboundedReceiver<()>,
        close_started: mpsc::UnboundedReceiver<()>,
        send_gate: Arc<Semaphore>,
        close_gate: Arc<Semaphore>,
    }

    impl tcp_sink::Server for GatedSink {
        async fn send(self: Rc<Self>, params: tcp_sink::SendParams) -> Result<(), capnp::Error> {
            let data = Bytes::copy_from_slice(params.get()?.get_data()?);
            let attempt = self.send_attempts.get() + 1;
            self.send_attempts.set(attempt);
            let _ = self.send_started.send(());
            self.send_gate.acquire().await.unwrap().forget();

            if self.fail_send_at == Some(attempt) {
                return Err(capnp::Error::failed("injected send failure".into()));
            }
            self.writes.borrow_mut().push(data);
            Ok(())
        }

        async fn close(
            self: Rc<Self>,
            _params: tcp_sink::CloseParams,
            _results: tcp_sink::CloseResults,
        ) -> Result<(), capnp::Error> {
            let _ = self.close_started.send(());
            self.close_gate.acquire().await.unwrap().forget();
            self.closes.set(self.closes.get() + 1);
            if self.fail_close {
                Err(capnp::Error::failed("injected close failure".into()))
            } else {
                Ok(())
            }
        }
    }

    fn gated_sink(
        fail_send_at: Option<usize>,
        fail_close: bool,
    ) -> (tcp_sink::Client, SinkHarness) {
        let writes = Rc::new(RefCell::new(Vec::new()));
        let closes = Rc::new(Cell::new(0));
        let (send_started_tx, send_started) = mpsc::unbounded_channel();
        let (close_started_tx, close_started) = mpsc::unbounded_channel();
        let send_gate = Arc::new(Semaphore::new(0));
        let close_gate = Arc::new(Semaphore::new(0));
        let sink = capnp_rpc::new_client(GatedSink {
            writes: writes.clone(),
            closes: closes.clone(),
            send_attempts: Cell::new(0),
            send_started: send_started_tx,
            close_started: close_started_tx,
            send_gate: send_gate.clone(),
            close_gate: close_gate.clone(),
            fail_send_at,
            fail_close,
        });
        let harness = SinkHarness {
            writes,
            closes,
            send_started,
            close_started,
            send_gate,
            close_gate,
        };
        (sink, harness)
    }

    fn run_local(future: impl Future<Output = ()>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(tokio::task::LocalSet::new().run_until(future));
    }

    async fn within<F: Future>(future: F) -> F::Output {
        tokio::time::timeout(TEST_TIMEOUT, future)
            .await
            .expect("test operation timed out")
    }

    async fn next_signal(rx: &mut mpsc::UnboundedReceiver<()>) {
        within(rx.recv())
            .await
            .expect("test sink dropped its signal channel");
    }

    #[test]
    fn rpc_transport_flush_and_shutdown_follow_rpc_completion() {
        run_local(async {
            let (sink, mut harness) = gated_sink(None, false);
            let (_guest_tx, guest_rx) = mpsc::channel(1);
            let mut transport = RpcTransport::new(Bytes::new(), guest_rx, sink);

            transport.write_all(b"first").await.unwrap();
            next_signal(&mut harness.send_started).await;

            let mut cancelled_flush = Box::pin(transport.flush());
            assert!(futures::poll!(&mut cancelled_flush).is_pending());
            drop(cancelled_flush);

            harness.send_gate.add_permits(1);
            within(transport.write_all(b"second")).await.unwrap();
            next_signal(&mut harness.send_started).await;

            let mut flush = Box::pin(transport.flush());
            assert!(futures::poll!(&mut flush).is_pending());
            harness.send_gate.add_permits(1);
            within(flush).await.unwrap();

            transport.write_all(b"third").await.unwrap();
            next_signal(&mut harness.send_started).await;

            let mut shutdown = Box::pin(transport.shutdown());
            assert!(futures::poll!(&mut shutdown).is_pending());
            harness.send_gate.add_permits(1);
            next_signal(&mut harness.close_started).await;
            assert!(futures::poll!(&mut shutdown).is_pending());
            harness.close_gate.add_permits(1);
            within(shutdown).await.unwrap();

            assert_eq!(
                &*harness.writes.borrow(),
                &[
                    Bytes::from_static(b"first"),
                    Bytes::from_static(b"second"),
                    Bytes::from_static(b"third")
                ]
            );
            assert_eq!(harness.closes.get(), 1);
        });
    }

    #[test]
    fn rpc_transport_reports_send_failure_during_shutdown() {
        run_local(async {
            let (sink, mut harness) = gated_sink(Some(1), false);
            let (_guest_tx, guest_rx) = mpsc::channel(1);
            let mut transport = RpcTransport::new(Bytes::new(), guest_rx, sink);

            transport.write_all(b"fails").await.unwrap();
            next_signal(&mut harness.send_started).await;
            let flush = Box::pin(transport.flush());
            harness.send_gate.add_permits(1);
            let flush_error = within(flush).await.unwrap_err();
            assert_eq!(flush_error.kind(), io::ErrorKind::BrokenPipe);

            let shutdown_error = within(transport.shutdown()).await.unwrap_err();
            assert!(shutdown_error.to_string().contains("injected send failure"));
        });
    }

    #[test]
    fn rpc_transport_reports_close_failure() {
        run_local(async {
            let (sink, mut harness) = gated_sink(None, true);
            let (_guest_tx, guest_rx) = mpsc::channel(1);
            let mut transport = RpcTransport::new(Bytes::new(), guest_rx, sink);

            let mut shutdown = Box::pin(transport.shutdown());
            assert!(futures::poll!(&mut shutdown).is_pending());
            next_signal(&mut harness.close_started).await;
            harness.close_gate.add_permits(1);
            let error = within(shutdown).await.unwrap_err();
            assert!(error.to_string().contains("injected close failure"));
            assert_eq!(harness.closes.get(), 1);
        });
    }
}
