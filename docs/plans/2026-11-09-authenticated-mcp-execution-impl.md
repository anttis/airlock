# Authenticated MCP execution through a Unix socket

## Context

`docs/plans/2026-11-08-authenticated-mcp-execution.md` (draft, untracked in the
main checkout) describes a privilege gap. The admin MCP `execute` tool at
`http://admin.airlock/mcp` runs each approved command with the VM boot
credentials (`SpawnCreds`, `app/airlockd/src/admin/mcp/bridge.rs:36,269`). A
non-root agent in a VM that boots as root therefore gets root commands when it
asks only for network access. Goal: run each approved command as the
kernel-verified Unix user of the MCP client. Keep host approval and the grant
network lifecycle, and support concurrent sessions of different users.

User decisions (2026-11-08/09):
- Supplementary groups: **primary group only** (peer gid, `setgroups(0)` as today).
- HTTP `execute`: **keep for one release**, as the boot user, with a deprecation
  note in the result.
- Adapter path: **`/run/airlock/bin/airlock-mcp`** (per-boot root-owned tmpfs).
- Packs: **new `claude@3`, `codex@3`, `copilot@3`** use the adapter. `@2` stays
  on HTTP. This follows the rule at `app/airlock-cli/src/packs/mod.rs:641-646`
  (built-in fingerprints leave out scripts, so breaking changes need a new version).

Decisions from code facts:
- Socket framing: **MCP stdio framing**. rmcp 3.8's `server` feature already
  compiles `transport-async-rw`. airlockd runs `rmcp::serve_server(tools,
  UnixStream)` per connection. Each request runs in its own task with a child
  cancellation token, and `notifications/cancelled` works (`rmcp service.rs:1571-1639`).
- Adapter: **mode of the airlockd binary**, chosen by argv[0] basename
  `airlock-mcp`. Shell shims cannot open Unix sockets (`app/airlockd/src/bridge.rs:28`).
  airlockd is static musl and runs on any image.
- Guest processes share airlockd's user and PID namespaces (spawn unshares only
  NEWNS/IPC/UTS, `process.rs:642-644`), so `SO_PEERCRED` maps 1:1.

Out of scope (as in the draft): isolation between processes of one user, guest
root impersonating others, authorization of the rule tools, hooks, removal of
HTTP execute (next release).

## High-level design

```
agent ──stdio──> /run/airlock/bin/airlock-mcp   (caller's uid, no privileges)
                    │ byte relay
                    v
               /run/airlock/mcp.sock             (0666; dir is airlockd's 0755 tmpfs)
                    │ accept → peer_cred() → Caller::Verified{uid,gid,pid}
                    v
               rmcp session: AdminTools{caller}
                    │ execute → ExecRequest{caller}
                    v
               GrantBridge::execute → requestGrant(…, user) → prompt "User" line
                    │ approved
                    v
               spawn_collect(caller uid/gid, groups cleared, HOME of caller)
```

- **Caller** is an immutable value made once per connection and copied into each
  `ExecRequest`. No shared "active user". HTTP uses `Caller::BootUnverified`,
  which keeps today's boot-user behavior and is marked "not verified" in the prompt.
- **Account data** (name, home) comes from the guest `/etc/passwd`. It is only
  display text and `HOME`, never identity. It is read off the PID 1 thread with
  `O_NOFOLLOW|O_NONBLOCK`, regular file only, max 1 MiB.
- **Privilege order** in the child: netns, mount ns / chroot, harden, then
  `setgroups(0)`, `setgid`, `setuid`, and only then `chdir(cwd)` (fallback `/`).
  Moving `chdir` after `setuid` stops a caller from passing a `cwd` below a
  directory it cannot enter.
- **Teardown** stays: process-group SIGKILL plus deleting the grant veth. A closed
  connection ends the rmcp session. rmcp drains for up to 5 s and then cancels
  handlers, which drops the bridge reply and kills the command.
- **Fail closed**: when the `/run/airlock` tmpfs mount was skipped (symlink case,
  `container.rs:146`), neither the adapter bind nor the socket exists. The adapter
  never falls back to HTTP.
- **Resource limits**: max 64 open socket connections, and 30 s for the first
  message (initialize). Line length is already capped at 16 MiB by rmcp.

## Detailed plan

Setup: `EnterWorktree`. First copy this plan to
`docs/plans/2026-11-09-authenticated-mcp-execution-impl.md`. Also copy the
untracked draft `docs/plans/2026-11-08-authenticated-mcp-execution.md` into the
worktree, so the impl plan's reference is committed with it.

### Phase 1: caller identity and execution (airlockd)

1. `admin/caller.rs` (new):
   - `pub(crate) enum Caller { Verified(PeerId), BootUnverified }`,
     `PeerId { uid, gid, pid }`.
   - `Account { name: Option<String>, home: Option<String> }`.
   - `async fn lookup_account(root: &str, uid) -> Account` reads in
     `spawn_blocking` and parses `name:x:uid:gid:gecos:home:shell`, ignoring bad
     lines. Inline unit tests for the parser and for the limits (FIFO/symlink refused).
2. `GrantContext` (`admin/mcp/bridge.rs:41`) gets `rootfs: String` (prod
   `ROOTFS`, tests a temp dir) for the account lookup, and `#[cfg(test)]
   real_creds: bool` (default false), next to `skip_netns`.
3. `ExecRequest` gets `caller: Caller`. In `execute` (`bridge.rs:264`):
   - Boot check is unchanged (`SpawnCreds` `None` → same error). `harden` still
     comes from `SpawnCreds`.
   - `(uid, gid)` = the peer ids for `Verified`, the boot ids for `BootUnverified`.
   - Account lookup for that uid. Send `user` in `requestGrant`.
   - Env layers become: CA defaults, sandbox env, **caller layer**, agent env (an
     explicit agent `HOME` still wins as today). Caller layer when uid != boot uid:
     `HOME` = passwd home or `/`, and `USER`/`LOGNAME` = name only if the sandbox
     env has them. When uid == boot uid: no caller layer.
   - `BootUnverified`: new `ExecOutput.warning: Option<String>`
     (`skip_serializing_if`) with the deprecation text, plus a `warn!` log.
4. `process.rs`:
   - In `build_pre_exec` (`:567-669`), move `chdir` after `setuid`, keeping the
     fallback to `/`. This applies to all spawns (`spawn_user` too). The effect is
     only that a cwd the user cannot enter falls back to `/`.
   - `spawn_collect` (`:304`): split `container` into `enter_rootfs` (stays
     `!cfg!(test)`) and `switch_creds` (prod true, tests take
     `CollectSpec.real_creds` from `GrantContext.real_creds`). Root-only tests set
     it. Existing tests keep running as the test user, also under root.
   - Update the doc comments at `process.rs:271-272` and `tool_execute.rs:105`.
5. Schema `airlock-common/schema/network.capnp` `GrantRequest`: add
   `user @5 :ExecUser`, with `struct ExecUser { uid @0 :UInt32; name @1 :Text;
   verified @2 :Bool; }`. Codegen runs from `airlock-common/build.rs`.

### Phase 2: Unix socket server (airlockd)

6. `init/linux/container.rs`: record in a static whether the `/run/airlock` tmpfs
   is mounted, and add `pub fn bridge_tmpfs_mounted() -> bool` (non-Linux stub
   false), like `sandbox_ns::fd()`.
7. `admin/socket.rs` (new) `pub async fn serve(path, grants, caps)`:
   - Return early with a `warn!` if `!bridge_tmpfs_mounted()`.
   - Remove a stale file, `UnixListener::bind`, `chmod 0666`.
   - Accept loop with a 64-connection semaphore. `peer_cred()` fails → drop.
     `spawn_local`: `timeout(30s, serve_server(AdminTools::for_caller(..), stream))`
     for the handshake, then keep the `RunningService` and await `waiting()`. Log
     open and close with uid and pid.
8. `admin/mcp/tools/mod.rs`: `AdminTools` gets `caller: Caller`. `new()` (HTTP) →
   `BootUnverified`, `for_caller()` → `Verified`. Same tool set and capability
   gating. Per-transport `execute` description through the public
   `ToolRouter.map` (`rmcp router/tool.rs:327`): the HTTP text says it is
   deprecated and names the adapter. `tool_execute.rs` passes `self.caller` into
   `ExecRequest`. Forged argument fields cannot reach identity (`ExecuteArgs`
   ignores unknown fields).
9. `admin/server.rs::start`: spawn the socket server next to the HTTP server.
10. `airlock-common/src/lib.rs`: `MCP_SOCKET = "/run/airlock/mcp.sock"` and
    `MCP_ADAPTER = "/run/airlock/bin/airlock-mcp"`, next to `BRIDGE_DIR`.

### Phase 3: adapter and delivery

11. `main.rs`: replace `#[tokio::main]` with a plain `fn main()`. If the argv[0]
    basename is `airlock-mcp`, run `adapter::run()` and exit. Else build the
    current_thread runtime by hand and run the existing body unchanged.
12. `adapter.rs` (new, std only): connect to `MCP_SOCKET`. On error print a clear
    message to stderr and exit 1. Thread A copies stdin→socket and on EOF calls
    `shutdown(Write)` only. Thread B copies socket→stdout with a flush per read.
    Exit on socket EOF or a stdout write error. Stdout carries only socket bytes.
13. Delivery in `container::setup`, after the tmpfs mount and only when it
    succeeded: create `run/airlock/bin` (0755), touch `airlock-mcp`, then
    `mount::bind(current_exe, target, true)` (`init/linux/mount.rs:223`, does the
    read-only remount). This runs before `sandbox_ns::create`, and recursive
    binds keep the read-only flag (also in `harden` copies). The browser shim's
    `create_dir_all` + write of `xdg-open` does not conflict.

### Phase 4: host prompt, packs, docs

14. `airlock-cli` (texts in `messages.rs`):
    - `network/grant.rs`: read `user` (`!has_user()` → unknown) into `Request` and
      `PromptCommand` (`approval/mod.rs:94`). Add the user to the `info!` at `:610`.
    - `approval/text.rs::command_items`: `LABEL_USER` before Command. The value
      passes through `clean` + `cut_end` (guest text). uid 0 shows as `root`.
    - `messages::grants`: `LABEL_USER = "User"`, `fn exec_user(name, uid,
      verified) -> String`, e.g. `agent (uid 1000)` or
      `root (uid 0, not verified: old HTTP connection)`.
    - Monitor, Linux dialog and macOS render the generic details list, so no change.
    - Tests: `text.rs:736`, `:782` (index `details[3]` at `:808` shifts), the
      `:816` fixture, `test_temporary_grant.rs:108` builder and `:832`
      `test_command()`. Add an assert that the prompt shows the user.
15. Packs: copy `packs/{claude,codex,copilot}@2` to `@3` and change only the MCP entry:
    - claude@3: `{"type":"stdio","command":"/run/airlock/bin/airlock-mcp","args":[]}`
      in `.mcpServers.airlock` (same jq merge).
    - codex@3: `command = "/run/airlock/bin/airlock-mcp"`, drop `url`, keep the
      timeouts, approval mode and `omit_tools_from`.
    - copilot@3: `{"type":"local","command":"…","args":[],"tools":["*"],"timeout":86400000}`.
      Check the type name against Copilot CLI docs before committing.
    - Update `packs/builtin.rs:271` (newest versions → `@3`) and add `@3` pinned
      fingerprints in `packs/tests/test_configure.rs:195`.
    - Do not change the project's own `airlock.toml` (it uses `@2`). Mention this
      to the user.
16. Manual (STE skill, lint each page):
    - `technical/mcp.md`: socket, adapter, caller identity, the request flow at
      `:79`, HTTP deprecation, the 5 s disconnect delay, and the security note at `:145`.
    - `technical/approvals.md:32-38` (User line).
    - `technical/services.md:32` (`/run/airlock/bin` contents).
    - `technical/rpc.md` (`GrantRequest.user`).
    - `configuration/network.md:152`.
    - `packs/agents/{claude-code,codex,copilot}.md`: version 3. Claude config is
      per setup user. Add the "agent without packs" setup
      (`claude mcp add --scope user airlock -- /run/airlock/bin/airlock-mcp`,
      codex and copilot entries).
    - Dev log `docs/log/2026-11-09-authenticated-mcp-execution.md`.

### Phase 5: tests

17. `admin/tests/test_admin_mcp_socket.rs` (unprivileged, CI). Helpers go in
    `test_cfg/admin.rs`: serve the socket in a temp dir, a line-JSON client, and
    `GrantCall.user` in `FakeGrantHost`.
    - initialize and `tools/list` over the socket; the `execute` description has
      no deprecation text.
    - execute with forged `"uid":0,"user":"root"` args → recorded `user.uid ==
      getuid()`, `verified`, output OK.
    - Two connections with requests pending at once; closing one aborts only its
      command (deadline > 5 s), and the other completes.
    - Over HTTP, execute still works, returns `warning`, and records `verified=false`.
18. Root-only tests (`#[ignore = "needs root: mise run test:root"]`, names
    `root_*`; new mise task `test:root` = `cargo test -p airlockd root_ --
    --ignored`). The client runs on a dedicated `std::thread` that does
    `SYS_setresgid` then `SYS_setresuid` to 65534 before `connect`.
    `real_creds = true`, the test rootfs has a passwd with a home for 65534, and
    the temp dir is chmod 0755.
    - `id -u; id -g; id -G` → `65534` ×3, `HOME` from the passwd.
    - A 0600 root file in a 0755 dir cannot be read.
    - `cwd` below a 0700 root dir falls back to `/`.
    - A concurrent root client gets uid 0.
19. Adapter tests: relay against a fake socket server. Stdin EOF keeps replies,
    socket EOF exits, no extra stdout bytes, and a missing socket gives exit 1
    with a stderr message.
20. bats `tests/vm/admin_mcp.bats` (`alpine:latest`, `adduser -D agent`, run as
    root and with `su agent -s /bin/sh -c …`):
    - `printf initialize+tools/list | /run/airlock/bin/airlock-mcp` gives both
      replies for both users.
    - Socket and adapter owners and modes. `agent` cannot replace the adapter.
    - HTTP tools/list still has `execute`.
    - `packs.bats`: add the `@3` checks (codex `command` entry); `@2` checks unchanged.

### Verification gates

- After each phase: `mise format`, `mise run lint`, `mise run test`.
- Phase 5: `mise run test:root` here (the sandbox runs as root).
- End: `mise run bats` (KVM and Docker both work here), with a Monitor.
- Manual test for the user (host): two sessions, root and non-root, both with
  `@3` packs. Approve a grant for `id -u` in each, and check the prompt "User"
  line and the outputs.
- Commit on the worktree branch with `/git-commit`. Push the branch, but open no PR.
