# Relay HTTP/1.1 upgrades (WebSocket) through the HTTP proxy

## Symptom

Codex's WebSocket transport to `api.openai.com` failed on every attempt
and fell back to plain HTTPS:

```
Stream disconnected before completion: WebSocket protocol error:
No "Connection: upgrade" header
failed to send websocket request: Connection closed normally
```

`airlock-debug.log` showed the proxy logging "upstream connection closed,
shutting down guest connection" at the exact moment of the first error,
on an h1 (no ALPN) connection to `api.openai.com:443`.

## Root cause

The HTTP relay in `network/http.rs` had no notion of `Upgrade`. It served
the guest with hyper's auto builder and forwarded requests through a
plain hyper h1 client connection, never calling `hyper::upgrade::on` on
either side. Two things followed from that once the upstream answered
`101 Switching Protocols`:

- The hyper client connection future resolves as soon as it parses a 101,
  because an upgraded h1 connection can never carry another request. The
  relay's `select!` took that as "upstream closed" and called
  `graceful_shutdown` on the guest connection, which disables keep-alive.
  When hyper then encoded the 101 for the guest, its `enforce_version`
  step did `headers.insert(CONNECTION, "close")`, silently replacing
  `Connection: upgrade`. `Upgrade: websocket` survived, which is exactly
  why tungstenite complained about the Connection header only.
- Even without that rewrite, both hyper connections were simply dropped
  after the 101, so the guest saw the response followed by EOF.

## Fix

hyper's own upgrade support (`with_upgrades`, `hyper::upgrade::on`) needs
`Send` IO, and the guest transport is an `Rc`-backed Cap'n Proto RPC
stream. So the relay reclaims the raw sockets instead: after a 101 both
hyper connections finish *without* shutting their IO down, and
`Connection::into_parts` on each side returns the socket plus whatever
bytes hyper had already read past the handshake (the guest's first frame
sent behind its request, the server's first frame sent behind its 101).
Those become two `Transport`s with a `PrefixedRead` and go through the
existing `tcp::relay`.

Supporting changes:

- The guest side is served with `hyper::server::conn::http1` or `http2`
  directly, chosen from the sniffed prefix (`is_h2_preface`), because the
  hyper-util auto connection has no `into_parts`. `detect_http` now sets
  `Transport::h2` from the preface rather than from ALPN alone.
- The upstream h1 connection task returns the `Connection` object so it
  can be taken apart; h2 returns `None`.
- `http/upgrade.rs` owns the state machine. `Upgrade::wants` mirrors
  hyper's own `Server::parse` test (an `Upgrade` header decides on
  HTTP/1.1 only, else `CONNECT`) and `switches` mirrors its client-side
  `Client::decoder` test (101 to anything, or a 2xx other than 204 to
  `CONNECT`), because hyper ends both connections with
  `Dispatched::Upgrade` for exactly these. So `CONNECT` tunnels the same
  way. Both were wrong in a first version: requiring `Connection:
  upgrade` would have re-broken lenient servers, and counting 204 to
  `CONNECT` as a switch made the relay wait on an upstream connection
  hyper had kept alive. The state is set to `Requested` before the request is
  forwarded, because the upstream task can complete before the service
  even sees the reply, and to `Switched` from the upstream's *raw* reply,
  before middleware. While an upgrade is in flight `drive_guest` skips
  the "mirror upstream close" shutdown, which is what rewrote the header.
- `Upgrade::reply` reconciles the raw and final replies. Unless both
  switch, the guest connection ends with the reply (`Connection: close`):
  the upstream may already be gone, and the mirror shutdown that would
  normally pass that on is suppressed while an upgrade is in flight. A
  switch status forged by a Lua script becomes a 502: there is no
  upstream stream to relay, and a 101 would leave the guest waiting
  forever.
- hyper ends a guest connection that carried an upgrade request with
  `Dispatched::Upgrade` whether or not the switch happened, and never
  shuts the socket down itself. After a failed upgrade the relay takes
  the socket back and shuts it down, so a TLS guest gets a proper
  `close_notify` rather than a bare drop.
- `to_origin_form` leaves `CONNECT` requests in authority form.

Handshake headers still pass through secret unmasking and re-masking, so
an injected API key in the upgrade request is substituted as before. The
Monitor tab sees the 101 like any other response. Bytes after it are
relayed raw in both directions: surrogates inside them go upstream
unmasked, and a real value the upstream sends back is not re-masked.

## Tests

- `websocket_upgrade_relays_raw_bytes` (plain) and
  `websocket_upgrade_through_middleware` — handshake through the proxy
  against a raw-TCP upgrade server, asserting the 101 reaches the guest
  with `Connection: upgrade` intact and without `Connection: close`, that
  bytes sent behind the request and behind the 101 arrive in order, and
  that later writes are echoed both ways. Both failed before the fix.
- `tls_websocket_upgrade_relays_raw_bytes` — the Codex shape: a guest
  offering no ALPN through the MITM to an upstream that advertises h2 and
  http/1.1.
- `connect_tunnel_relays_raw_bytes` — `CONNECT` through the proxy, 2xx
  and then raw bytes both ways.
- `connect_answered_204_is_not_a_switch` — a 204 on a connection the
  upstream keeps open reaches the guest with `Connection: close` and the
  stream ends, instead of the relay waiting forever.
- `websocket_upgrade_rejected_by_upstream_closes_guest_connection` — an
  upstream 400 to a handshake without a key reaches the guest with
  `Connection: close`, and the stream ends.
- `websocket_upgrade_forged_by_middleware_is_refused` — a script that
  rewrites that 400 to 101 gets the guest a 502 and a close.
