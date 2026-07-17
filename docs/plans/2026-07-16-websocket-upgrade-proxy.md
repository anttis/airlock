# WebSocket upgrade proxy

## Intent

Support standard HTTP/1.1 WebSocket upgrades through Airlock's intercepted
network path. The HTTP handshake must still pass through network policy and
Lua middleware, including the OpenAI Codex API-key injection, before the proxy
switches to a bidirectional byte tunnel.

The fault is in the host-side Hyper relay, not smoltcp or vsock. Airlock
currently forwards the WebSocket handshake as HTTP but does not enable
Hyper's upgrade path or join the two streams after `101 Switching Protocols`.

HTTP/2 extended CONNECT (RFC 8441) is out of scope. Existing HTTP/2 request
handling must remain unchanged.

## High-level plan

- Preserve policy checks and middleware processing for the WebSocket
  handshake.
- Enable Hyper's HTTP/1.1 upgrade support on both proxy hops.
- After a valid `101`, switch to bidirectional byte relaying until either side
  closes.
- Retain the existing upstream-close behavior for ordinary HTTP connections.
- Keep passthrough rules unnecessary for WebSockets, since passthrough would
  also bypass HTTP middleware.

## Detailed plan

1. Add regression tests for plain HTTP and HTTPS MITM WebSocket upgrades. The
   tests cover middleware header injection, request and response upgrade
   headers, bytes coalesced with both sides of the handshake, bidirectional
   tunneled bytes, and connection closure.
2. Make the HTTP transport compatible with Hyper's `Send` requirement for
   upgraded connections. Move the non-`Send` Cap'n Proto sink into a local
   writer task fed by a bounded channel, preserve write ordering and
   backpressure, close the remote sink exactly once, and surface channel
   failure as a broken pipe.
3. Enable upgrades on the upstream HTTP/1.1 connection and the guest-facing
   auto-detecting server. Capture the guest upgrade before middleware, capture
   the upstream upgrade only after a valid `101`, and reject malformed or
   synthetic upgrade responses.
4. Coordinate the guest and upstream HTTP drivers with the upgrade lifecycle.
   Continue driving both until Hyper yields both upgraded streams, then relay
   those streams bidirectionally. Do not return from the connection handler or
   emit a disconnect until the upgraded tunnel finishes.
5. Document automatic HTTP/1.1 WebSocket handling in the networking manual and
   add a technical work log describing the design.
6. Run `mise format`, focused networking tests, `mise run test`, `mise lint`,
   and `mise run docs:build`.

