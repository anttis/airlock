# Authenticated MCP execution

## Goal and user-visible behavior

The `execute` tool of the admin MCP server at `http://admin.airlock/mcp`
ran each command as the sandbox (boot) user, for any client. A non-root
agent in a sandbox that boots as root thus got root commands when it
asked only for network access. Now the command runs as the user of the
agent:

* airlockd also serves the `airlock` MCP server on the Unix socket
  `/run/airlock/mcp.sock` (mode 0666) in the per-boot, root-owned (0755)
  tmpfs `/run/airlock`. The kernel gives the uid, gid and pid of the
  client at connect time. The command runs as that uid and primary gid,
  with no supplementary groups. No tool argument can change the user.
* Agents connect through the stdio adapter `/run/airlock/bin/airlock-mcp`.
  The packs `claude@3`, `codex@3` and `copilot@3` use it. They are the
  same as `@2` except the MCP entry. The `@2` packs do not change.
* The host approval prompt has a new `User` line, for example
  `agent (uid 1000)`. uid 0 always shows as `root`.
* `execute` on the HTTP endpoint is deprecated for one release. It still
  runs commands as the sandbox user. Its description starts with a
  `DEPRECATED` note, and its results have a `warning` field. The manual
  tools, the rule tools and the hooks stay on HTTP.

## Implementation

* `admin/socket.rs`: listener, one rmcp session per connection with
  newline-delimited JSON-RPC. At most 64 connections, 30 s for the MCP
  initialization. A disconnect ends the session and stops the command.
  This can take up to 5 s, because rmcp first waits for pending replies.
* `admin/caller.rs`: `Caller::Verified(PeerId)` for socket clients and
  `Caller::BootUnverified` for HTTP. Reads name and home from the
  container `/etc/passwd` (size limit, no link, no FIFO). These values are
  only for display and the environment. They never select the user.
* `admin/mcp/bridge.rs`: selects uid and gid from the caller. For another
  user than the sandbox user, it sets `HOME` (or `/`), and `USER` and
  `LOGNAME` only if the sandbox env has them. An agent `HOME` still wins.
* `process.rs`: `chdir` now runs after the privilege drop, for all
  sandbox processes. If the user cannot enter the cwd, it starts in `/`.
* `adapter.rs`: the airlockd binary selects adapter mode by program name.
  It relays stdin and the socket unchanged, delivers pending replies at
  end of stdin and exits 1 with a stderr message if the socket is
  missing. It never falls back to HTTP.
* `init/linux/container.rs`: the adapter is a read-only bind of the
  airlockd binary in the tmpfs. If `/run` or `/run/airlock` is a symlink
  in the image, init skips the tmpfs. Then the socket and the adapter do
  not exist (fail closed). The FIFO bridges still work.
* RPC: `GrantRequest.user @5 :ExecUser` (`uid`, `name`, `verified`). The
  host cleans the name like other sandbox text. A request from an older
  guest shows `unknown`.

Not protected: processes of the same user, and guest root, which can act
as any user. The HTTP endpoint still has no per-process access control.

## Tests

* Unit tests: passwd parser and account read limits, environment layers
  of the caller, adapter relay, approval `User` line.
* `admin/tests/test_admin_mcp_socket.rs`: tool list over the socket and
  the HTTP deprecation, forged user arguments, and a disconnect that
  stops only the command of that client.
* `admin/tests/test_execute_as_caller.rs`: root-only test with an
  unprivileged and a root client on one server. Run with
  `mise run test:root`.
* `tests/vm/admin_mcp.bats`: the adapter for root and for an
  unprivileged user, owners and modes of the socket and the adapter, and
  the deprecated `execute` on HTTP. `packs.bats` installs the `@3` packs
  and checks the adapter entry of Codex.
