# WebSocket upgrade proxy

## Problem

Codex started inside an Airlock sandbox printed this warning on every launch:

```text
Falling back from WebSockets to HTTPS transport. stream disconnected before
completion: WebSocket protocol error: No "Connection: upgrade" header
```

The TUN, smoltcp stack, vsock RPC, and byte sinks could already carry arbitrary
TCP data. The failure was in the host-side HTTP interception layer. Airlock
terminated HTTP on both sides with Hyper but drove both connections without
upgrade support. A real upstream `101 Switching Protocols` therefore reached
the guest as `Connection: close`, and there was no relay for subsequent
WebSocket frames.

## Upgrade lifecycle

The HTTP/1.1 client connection now runs with Hyper upgrades enabled, as does
the auto-detecting guest-facing server. The service removes and retains the
guest's upgrade handle before the request enters Lua middleware. Middleware
can still inspect the handshake or inject headers such as Codex's host-side
API key. Only a genuine upstream `101` with valid upgrade headers produces the
second handle; middleware cannot fabricate an upgrade from an ordinary HTTP
response.

The existing relay monitors the upstream Hyper driver so an idle upstream
close can gracefully shut down the guest connection. An upgrade makes that
driver complete too, which initially caused a race: Airlock began graceful
shutdown before the request service recorded the `101`, rewriting the response
to `Connection: close`. A per-connection state machine distinguishes an
upgrade request being evaluated from an idle close. The close path waits for
the service to settle as accepted or rejected before deciding whether to shut
down HTTP or enter the raw tunnel.

Once both Hyper drivers yield their upgraded streams, Airlock relays bytes in
both directions until either peer closes. Hyper's upgraded streams retain
their internal read buffers, so bytes coalesced with the `101` are not lost.
Each raw write is flushed because the TLS MITM streams otherwise retain small
interactive frames even though the HTTP response itself has arrived.

## Send-compatible RPC transport

Hyper stores upgraded I/O behind a `Send` trait object, while Cap'n Proto
clients are intentionally local to the CLI's `LocalSet`. `RpcTransport` now
keeps the non-`Send` capability in a local writer task. The transport itself
contains only Tokio channels, so it is safe for Hyper to own. A bounded channel
preserves backpressure; the writer awaits each streaming RPC call in order,
drains queued data, and sends one close after the producer shuts down.

This transport change also covers raw TCP, TLS, Unix socket, and reverse
forwarding paths, so it has explicit ordering, close, and compile-time `Send`
tests.

## Coverage

The regression suite exercises:

- HTTP and HTTPS MITM WebSocket upgrades.
- Lua header injection on the upgrade request.
- Upgrade headers in both directions.
- Bytes coalesced with the request and `101` response.
- Bidirectional post-upgrade traffic, including small TLS frames.
- Guest-first and upstream-first closure with correct disconnect timing.
- Rejection of middleware-fabricated `101` responses.

HTTP/2 extended CONNECT from RFC 8441 remains out of scope; normal HTTP/2
traffic is unchanged.
