# Show middleware denials as Denied in the monitor

## Problem

The monitor's Requests list showed `Result: Allowed` for a request that
a middleware script blocked with `req:deny()`. The guest got the 403,
and the monitor saw it only as a 403 reply, not as a denial.

The proxy emits the `Request` event as soon as hyper parses the request,
before any secret is unmasked and before middleware runs. Its `allowed`
flag is the connection-level policy verdict. `middleware::run` turned a
denial into a plain 403 response, and nothing told the monitor that the
verdict had changed.

## Change

- `middleware::run` tags the 403 it sends for a `req:deny()` with a
  `Denied` response extension (`app/airlock-cli/src/network/http/middleware.rs`).
- `ResponseInfo` gains `denied: bool`. `emit_response_event` sets it from
  that extension (`app/airlock-cli/src/network/http.rs`).
- In the monitor, a denied response flips the row and any open details
  snapshot to `Denied`. It also moves one count from the allowed counter
  to the denied counter
  (`app/airlock-monitor/src/tabs/monitor/network/{mod,requests}.rs`).

## Design notes

- **Overturn the verdict on the response, not delay the request event.**
  We considered holding back the `Request` event until the middleware
  request side had finished, so the monitor would get a final verdict
  and need no response-side update. A script can call `req:deny()`
  *after* `req:send()` (e.g. to block on what the upstream returned).
  By then the delayed event has already gone out as allowed, so the
  response side would still need the flag. Delaying would also hide a
  request until a script that calls `req:body()` has buffered the whole
  upload. The response-side flag covers a deny at any point and keeps
  rows showing up on arrival.
- **Only middleware denials set the flag.** A policy-denied request
  already says `allowed = false` on its `Request` event, so its 403
  keeps `denied = false`. That makes a set flag always mean
  "counted as allowed, now denied". The monitor can therefore move the
  count without looking up the row, and the running totals stay exact
  even after the list cap has evicted that row.
- **Response extension rather than a new return type.** The 403 already
  flows through secret masking and the upgrade handling before it
  reaches `emit_response_event`. An extension rides along with it
  unchanged, and neither scripts nor upstream replies can set it, so an
  upstream 403 never reads as a denial.

## Tests

- `deny_is_reported_on_response_event` (`test_middleware.rs`) drives a
  real proxy and reads the monitor event stream. It covers a deny
  before `req:send()`, a deny after it, an upstream 403 and a policy
  deny. The new `run_with_events` harness subscribes before the proxy
  starts, and the test channel grew from 1 to 64 slots so the events
  are still there to read afterwards.
- Monitor unit tests cover the row flip, an upstream 403 staying
  allowed, the counter move for an evicted row, and the open details
  snapshot.
