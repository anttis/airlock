# Add `req:setBasicAuth` to Lua middleware

## Problem

Middleware scripts that need HTTP Basic authentication had to build the
`Authorization` header by hand. The Lua sandbox has no base64 function,
so the only options were to precompute the encoded credential outside
airlock and paste it into the config, or to write a base64 encoder in
Lua inside the script. Both are error-prone, and the first defeats the
point of the `env` table, which is to keep the raw secret out of the
config file.

## Change

The request object gains `req:setBasicAuth(user, password)`. It is
implemented in Rust next to `setHeader` in
`app/airlock-cli/src/network/http/middleware.rs`. The method joins the
two arguments as `user:password`, encodes the result with the standard
padded base64 alphabet from the `base64` crate already used by the
vault, and inserts `Authorization: Basic <encoded>` into the request
headers, replacing any existing value.

## Design notes

- **A dedicated method rather than a generic `base64` helper.** The
  concrete need was Basic auth. A general encoding helper would also
  have to decide on alphabet and padding for every caller. `setBasicAuth`
  fixes both to what RFC 7617 requires. A generic helper can still be
  added later if another use case shows up.
- **Reject a colon in the user name.** RFC 7617 splits the decoded
  credential at the first colon, so a colon in the user name would make
  the server read a different user and password than the script
  intended. The method raises a Lua runtime error instead of silently
  sending broken credentials. The password may contain colons. The test
  covers that case.
- **Request only.** Responses never carry `Authorization`, so the
  response object does not get the method.

## Tests

`set_basic_auth_header` in `test_middleware.rs` runs a real proxy
round-trip and asserts the upstream server sees the exact expected
encoded value.
