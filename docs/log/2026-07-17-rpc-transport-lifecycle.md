# RPC transport lifecycle correctness

The Send-compatible RPC transport introduced for HTTP upgrades moved the
non-Send Cap'n Proto sink into a local writer task. The adapter returned from
`poll_write` after placing bytes on a bounded channel, which was valid
buffering, but its `poll_flush` and `poll_shutdown` implementations returned
success immediately. They did not wait for the writer task to deliver buffered
bytes or close the remote sink.

That mismatch mattered during connection teardown. The byte relay could finish
and emit its disconnect event while the RPC writer still held the final payload
or had not delivered the close notification. The WebSocket upstream-close test
also treated an empty timeout result as proof of EOF, so it could not detect a
tunnel that stayed open.

## Ordered writer lifecycle

The RPC writer channel now carries explicit write and flush commands. A flush
command contains a one-shot acknowledgement and is processed in FIFO order, so
the acknowledgement is sent only after all earlier streaming send calls have
completed. The transport retains an outstanding acknowledgement if a flush
future is cancelled and finishes that barrier before accepting another write;
this prevents a stale acknowledgement from satisfying a later flush.

Shutdown closes the command sender and polls the local writer task's join
handle. Channel closure makes the task drain queued commands, attempt one RPC
close, and then finish. Send errors take precedence over a subsequent close
error, while repeated shutdown polls receive a cached terminal result instead
of polling a completed task again.

The writer lifecycle is isolated in `network/io/rpc.rs`. The parent `io` module
remains a compact home for shared transport types, prefixed reads, and the
opposite-direction RPC channel sink.

## Relay teardown

The two writer shutdowns are independent. They now start together so a slow or
blocked shutdown on one peer cannot prevent the other peer from beginning its
own close sequence.

## Regression coverage

The RPC adapter tests use a gated sink to verify that:

- flush waits for preceding sends;
- a cancelled flush cannot be reused for a later write;
- shutdown waits for queued sends and the close RPC;
- send and close failures are reported;
- writes remain ordered and close is attempted once.

A separate relay test gates both writers and requires both shutdowns to start
before either is released. The WebSocket lifecycle test now waits for the RPC
receive channel to return EOF explicitly rather than accepting a timeout as
closure.

Verification completed with the full Rust test suite, Clippy, formatting,
manual documentation build, and whitespace checks.
