# RPC transport lifecycle correctness

## Goal

Make `RpcTransport`'s `AsyncWrite` implementation explicit and truthful:
`flush` must wait for preceding RPC writes, and `shutdown` must not complete
until queued writes have drained and the remote sink has received one close
attempt. Strengthen the WebSocket closure regression so it distinguishes EOF
from a timeout.

## High-level plan

1. Isolate the RPC transport lifecycle in `network/io/rpc.rs`.
2. Keep the non-`Send` Cap'n Proto sink in its existing local writer task.
3. Send explicit write and flush-barrier commands through the bounded channel.
4. Store the writer task's `JoinHandle` as the single, direct completion signal
   for shutdown.
5. Model the writer lifecycle explicitly and preserve cancellation safety.
6. Test flush, shutdown, error, ordering, and EOF behavior deterministically.

## Detailed plan

### 1. Ordered writer protocol

In `app/airlock-cli/src/network/io/rpc.rs`, re-exported by `network/io.rs`:

- Replace the writer channel's raw `Bytes` item with a private command enum:
  - `Write(Bytes)` sends one streaming `TcpSink.send` request.
  - `Flush(oneshot::Sender<()>)` acknowledges only after every preceding write
    command has completed successfully.
- Have the local writer task return `std::io::Result<()>`.
- On normal channel closure, drain commands in FIFO order, await exactly one
  `TcpSink.close` request, and then finish.
- On a send failure, stop accepting commands, discard queued commands so flush
  acknowledgements are cancelled, still attempt one close, and return the send
  error in preference to a close error.

### 2. Explicit `AsyncWrite` lifecycle

- Store `PollSender<WriterCommand>`, an optional pending flush acknowledgement,
  and the writer `JoinHandle` in `RpcTransport`.
- Use an explicit lifecycle state for open, flushing, shutting down, and
  terminal behavior.
- Keep `poll_reserve` and `send_item` in the same poll call so `PollSender`
  never retains a ready permit across shutdown.
- `poll_write` must finish any retained flush barrier before accepting another
  write. This makes a cancelled flush future safe: a later write cannot pass
  the stale barrier, and a subsequent flush must create a new barrier.
- `poll_flush` must enqueue at most one barrier and return `Pending` until the
  writer acknowledges it. A cancelled acknowledgement reports a broken pipe.
- `poll_shutdown` must close the command sender once and poll the writer task.
  It returns only after queued writes and the close RPC complete, preserving
  and replaying the terminal result without polling a completed task again.
- Writes after shutdown begins must fail clearly.
- Preserve the existing `Send` guarantees for `RpcTransport` and `Transport`.

### 3. Relay shutdown ordering

In `app/airlock-cli/src/network/tcp.rs`, start the server and container writer
shutdowns together. Once RPC shutdown genuinely waits for remote completion,
serializing these independent operations could unnecessarily delay the other
peer's closure. Add a regression with two gated writers that proves both
shutdowns start before either is released.

### 4. Deterministic lifecycle tests

Replace or split the existing RPC transport lifecycle test using a gated fake
`TcpSink`. Manually poll pinned operations to prove:

- flush remains pending while an earlier send RPC is blocked;
- cancelling that flush, completing the write, adding another write, and
  flushing again waits for the later write;
- shutdown remains pending while queued writes or the close RPC are blocked;
- writes reach the sink in order and close is attempted exactly once;
- send failures are visible to flush/shutdown;
- close failures are visible to shutdown.

### 5. Observable WebSocket closure

In `app/airlock-cli/src/network/tests/helpers.rs`, add
`TestConnection::wait_closed(timeout_ms)`. It succeeds only when
`container_rx.recv()` returns `None`, and fails on timeout or unexpected bytes.

Use this helper in `websocket_upstream_close_ends_tunnel` instead of treating
an empty `recv_bytes` result as proof of EOF.

### 6. Verification

Run:

- `cargo +nightly fmt --all`
- focused networking tests
- `cargo test --workspace`
- `cargo clippy --workspace --tests -- -D warnings`
- `mdbook build docs/manual`
- `git diff --check`
