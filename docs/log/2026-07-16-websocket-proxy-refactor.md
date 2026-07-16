# WebSocket proxy structural refactor

The initial HTTP upgrade implementation fixed WebSocket transport, but its
connection lifecycle and raw relay logic were larger than necessary. This
follow-up keeps the behavior while making the upgrade path use the same
primitives as the rest of the network proxy.

## One byte relay

`tcp::relay` is now the only production byte pump. It flushes after each
successful write, which is effectively free for TCP and the RPC transport but
is required for TLS streams to release small interactive records. A focused
test uses a flush-gated writer to protect that contract.

Hyper's `Upgraded` type already owns any bytes buffered while parsing the
upgrade response. Both upgraded connections can therefore be wrapped directly
as Tokio transports. The HTTP-specific relay copy and concrete Hyper I/O
downcast were removed.

## Focused lifecycle coordinator

Upgrade state now lives in a small coordinator with three states: idle,
pending, and accepted. It owns request capture, response validation, rejected
attempt cleanup, notification ordering, and the paired upgrade handles. The
HTTP connection driver only waits for an attempt to settle and consumes an
accepted tunnel; it no longer maps lifecycle states to numeric values.

Response completion is one atomic transition under a single mutable state
borrow. The transition simultaneously classifies the response, extracts the
upstream upgrade capability, and chooses the next lifecycle state, avoiding a
separate check followed by an assumed-unreachable state match.

A genuine upstream upgrade is identified by Hyper's `OnUpgrade` response
extension in addition to the HTTP/1.1 upgrade headers. Hyper adds that
capability when its client dispatcher recognizes an actual protocol switch,
and middleware preserves it while editing the response. This replaces the
separate provenance flag and continues to reject middleware-fabricated `101`
responses.

Rejected attempts return the coordinator to idle. A regression test exercises
a rejected upgrade followed by a successful upgrade on the same keepalive
connection.

Both outcomes of the guest/upstream connection race now feed one completion
tail. That tail validates the two HTTP drivers once and starts the accepted raw
tunnel, instead of maintaining parallel copies of upgrade finalization.

## Test support

The plain and TLS WebSocket peers now share small stream-generic helpers for
reading HTTP headers, writing an upgrade response, and echoing tunneled bytes.
The synthetic-response and upstream-close cases use the same request and
response fixtures. The test runners also share one runtime setup path.

## Verification

- All 73 networking tests pass, including the new relay and upgrade lifecycle
  regressions.
- The full Rust test suite passes: 185 unit tests plus doc tests.
- `mise lint` passes.
- `mise run docs:build` passes.
