# WebSocket proxy structural refactor

## Goal

Simplify the HTTP upgrade implementation introduced in `fe8ae89` while
preserving WebSocket behavior through plain and TLS-intercepted connections.
The refactor should leave one canonical byte relay, isolate upgrade lifecycle
coordination, and remove implementation-specific Hyper transport handling.

## High-level plan

1. Make `tcp::relay` the sole byte-relay implementation, including the flush
   semantics required by TLS streams.
2. Introduce a focused HTTP upgrade coordinator that owns lifecycle state,
   validation, wakeups, and accepted upgrade handles.
3. Remove the Hyper transport downcast, duplicate relay, numeric states,
   provenance `Cell`, and thin message trait.
4. Consolidate only genuinely duplicated test helpers and add lifecycle and
   flush regressions.
5. Update technical logs while leaving user-facing documentation unchanged.

A general lifecycle event bus and configurable WebSocket peer framework are
intentionally out of scope: they would add more abstraction than this path
needs.

## Detailed implementation

### 1. Canonical relay

In `app/airlock-cli/src/network/tcp.rs`:

- Extract the repeated one-direction copy loop into a private helper.
- Flush after each successful write.
- Preserve the current contract: either direction closing shuts down both
  sides.
- Add a focused test using a writer that exposes data only after `flush()`.

In the HTTP upgrade path:

- Await both Hyper `OnUpgrade` handles.
- Wrap each `Upgraded` directly in `TokioIo`, split it into `io::Transport`,
  and call `tcp::relay`.
- Delete `relay_upgraded_bytes` and `upgraded_server_transport`.
- Remove the concrete Hyper I/O downcast. Hyper's `Upgraded` already preserves
  its buffered `Rewind` bytes.

### 2. Isolate the upgrade lifecycle

Add `app/airlock-cli/src/network/http/upgrade.rs` containing:

- `UpgradeCoordinator`.
- `UpgradeState::{Idle, Pending(OnUpgrade), Accepted(PendingUpgrade)}`.
- `PendingUpgrade`.
- A typed response decision such as `Forward` or `InvalidUpgrade`.
- Direct `has_upgrade_headers(version, headers)` validation.

The coordinator will provide operations for beginning a request, completing a
response, cancelling an attempt, waiting until it settles, and taking an
accepted upgrade. `wait_until_settled` will encapsulate the missed-wakeup-safe
`Notify` ordering, so `http.rs` does not manipulate notification futures or
inspect lifecycle phases.

Rejected attempts return to `Idle`, allowing another upgrade attempt on the
same keepalive connection.

### 3. Simplify HTTP orchestration

In `app/airlock-cli/src/network/http.rs`:

- Replace the shared state, separate `Notify`, repeated transitions, and
  numeric phases with coordinator calls.
- Validate upstream provenance through the presence of Hyper's `OnUpgrade`
  response extension. This removes the provenance `Cell` and body-origin check
  while still rejecting middleware-fabricated `101` responses.
- Delete `UpgradeMessage` and its request/response implementations.
- Keep the guest/upstream connection race local; extracting it would require
  awkward generic future APIs.
- Ensure middleware errors and malformed responses always settle a pending
  attempt.

### 4. Refactor test infrastructure

In `app/airlock-cli/src/network/tests/helpers.rs`:

- Implement `run_with_config` as a wrapper around
  `run_with_config_and_events`, discarding the event receiver.
- Add small stream-generic helpers for reading and splitting an HTTP head from
  coalesced tunnel bytes, writing an upgrade response plus initial payload,
  and echoing upgraded bytes.

Use these helpers from the plain and TLS WebSocket tests without creating a
generalized peer framework.

Add or preserve regressions for:

- Canonical relay flushing.
- Plain and TLS upgraded traffic.
- Bytes coalesced with the request and `101` response.
- Middleware header injection.
- Synthetic `101` rejection.
- A rejected upgrade followed by a valid attempt on one keepalive connection.
- Guest and upstream closure behavior.
- Existing keepalive and HTTP/2 behavior.

### 5. Documentation and verification

- Add a refactor work log.
- Correct the earlier log's wording about explicitly recovering Hyper's read
  buffer.
- Leave user-facing documentation unchanged because behavior is unchanged.
- Run `mise format`, focused networking tests, `mise run test`, `mise lint`,
  and `mise run docs:build`.

## Acceptance criteria

- One production byte-relay implementation.
- No concrete Hyper I/O downcast.
- No numeric upgrade phases, provenance `Cell`, or `UpgradeMessage` trait.
- Wakeup ordering is fully encapsulated.
- Visible lifecycle branching in `http.rs` is substantially reduced.
- Net production networking code is reduced.
- All existing and new regression tests pass.
