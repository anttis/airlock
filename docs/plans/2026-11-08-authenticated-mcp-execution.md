# Draft: Authenticate MCP execution through a Unix socket

Status: proposal for discussion. This document does not describe an implemented feature.

## Problem

Airlock's admin MCP server accepts `execute` requests over HTTP at
`http://admin.airlock/mcp`. Each request contains a shell command and requested
network access. The host user must approve the request before Airlock runs it.

The execution path uses the VM's boot user and group IDs. It does not identify
the Unix user of the process that made the request. The HTTP endpoint is shared
by processes in the VM.

A VM can boot as root while individual shells switch to an unprivileged user.
An agent in such a shell can request a command that then runs as root after
host approval. The request therefore changes both network access and Unix
privileges, although the workflow presents it as a network grant.

Multiple root and non-root sessions can run concurrently. A single configured
execution user cannot preserve each caller's identity.

## Goal

Run each approved command as the authenticated Unix user of its MCP adapter.
Keep host approval and the existing temporary network grant lifecycle.
Support concurrent agents and sessions without agent-specific authentication.

This proposal preserves the guest user boundary. It does not create isolation
between processes that already run as the same user, or protect against guest
root impersonating another guest user.

## Proposed connection

```text
Claude or another MCP client           any guest user
    |
    | MCP over stdin/stdout
    v
airlock-mcp adapter                   same guest user as its launcher
    |
    | Unix socket connection
    v
airlockd                              privileged VM service
    |
    +-- obtain peer credentials from the kernel
    +-- bind the request to those credentials
    +-- ask the host user for network approval
    +-- create the grant's network namespace
    +-- drop privileges and run the command as the verified user
    +-- remove granted access when execution ends
```

Proposed names are `/usr/local/bin/airlock-mcp` for the adapter and
`/run/airlock/mcp.sock` for the socket. Neither name is an existing interface.

An agent would launch the adapter through its standard MCP stdio support.
For example, a proposed Claude configuration is:

```json
{
  "mcpServers": {
    "airlock": {
      "command": "/usr/local/bin/airlock-mcp",
      "args": []
    }
  }
}
```

## Adapter responsibilities

The adapter runs without elevated privileges. It connects directly to the
Unix socket as the user who launched it.

- Relay MCP requests, responses, notifications, and cancellation.
- Keep diagnostic output on stderr; reserve stdout for MCP messages.
- Close the socket when the client exits or closes its input.
- Report connection failures without falling back to unauthenticated execution.
- Preserve request IDs and concurrent request handling.

Prefer a transparent relay if the socket uses the same message framing as
MCP stdio. If the server instead uses HTTP over the Unix socket, the adapter
must translate transports and preserve MCP lifecycle behavior. Select the
transport after checking the existing `rmcp` integration.

The adapter must not become a privileged broker shared by unrelated users.
Otherwise the kernel would identify the broker instead of the agent's user.

## Authentication and execution

On acceptance of a socket connection, `airlockd` reads Linux `SO_PEERCRED`.
The kernel supplies the connecting process's user ID, group ID, and process ID.
Client-supplied usernames, headers, session IDs, and tool arguments are not
proof of identity.

Attach the verified identity to the connection and each queued request.
Keep it immutable while the request waits for host approval. Do not store an
"active user" shared by all connections.

| Adapter user | Approved command user |
| --- | --- |
| An unprivileged user | The same unprivileged user |
| root | root |

Before command execution, Airlock must establish the intended group policy,
drop unintended supplementary groups and capabilities, and set the verified
user and group IDs. Resolve the home directory from the guest account data.
Do not retain the daemon's home or root privileges for an unprivileged request.

`SO_PEERCRED` does not supply the caller's complete supplementary group list.
Choose an explicit policy: primary group only, or groups resolved from trusted
guest account data. Do not silently inherit the daemon's groups.

Show the execution user in the host approval details. Approval remains necessary
for root callers as well as unprivileged callers.

The socket directory and adapter executable must be protected against
replacement by unprivileged users. Socket permissions should allow intended
guest users to connect. Authentication comes from peer credentials, not from
making the socket secret. Confirm socket visibility and credential mapping
across Airlock's existing mount and user namespaces.

## Network grant lifecycle

Keep the existing network mechanism. After host approval, Airlock creates a
network namespace and virtual link for the command. Its source address selects
the proxy that enforces the approved grant.

On command completion, cancellation, expiry, revocation, or client disconnect,
Airlock must stop the command as required and remove the granted network path.
Remaining descendants must not retain granted network access. Ordinary agent
shells keep their original network policy.

Bind cancellation to the correct connection and request. One adapter's exit
must not cancel another user's command.

## Compatibility and migration

Unauthenticated HTTP must no longer expose command execution through the old
boot-user path. Leaving that path available would bypass the new boundary.
Read-only manual tools and existing hook endpoints can remain separate.
Review rule-editing tools independently; this proposal does not define their
authorization policy.

Update agent packs to install or expose the adapter and configure stdio MCP.
Document migration for users who configure their agents without packs.
Existing HTTP execution clients should receive a clear migration error, rather
than silently selecting a different execution user.

Determine how Airlock supplies the adapter inside arbitrary images. Prefer
reusing existing guest executable installation mechanisms. Decide whether a
small separate binary or a mode of an existing guest binary is simpler.

## Verification

- Connect as two different users concurrently and verify each execution UID.
- Verify that an unprivileged caller cannot read a root-only test file.
- Submit a forged user field and verify that it cannot change execution identity.
- Verify that unauthenticated HTTP cannot execute commands.
- Hold requests pending approval and verify that later connections cannot change their identities.
- Verify home, group, and capability behavior for both users.
- Verify approval, denial, cancellation, expiry, revocation, and disconnection.
- Verify that teardown removes access for surviving child processes.
- Verify that one client's cancellation does not affect another client's grant.
- Run a real-VM test with simultaneous root and unprivileged agent sessions.

## Open decisions

1. Socket framing: MCP stdio framing or Streamable HTTP over a Unix socket?
2. Adapter delivery: separate executable or an existing guest binary mode?
3. Supplementary groups: primary group only or trusted account memberships?
4. Migration: when should unauthenticated HTTP execution stop working?

## Current implementation references

- `app/airlockd/src/admin/server.rs`: shared loopback HTTP listener.
- `app/airlockd/src/admin/mcp/mod.rs`: stateless HTTP MCP service.
- `app/airlockd/src/admin/mcp/tools/tool_execute.rs`: command arguments and environment checks.
- `app/airlockd/src/admin/mcp/bridge.rs`: boot credentials and grant execution.
- `app/airlockd/src/rpc.rs`: assignment of boot credentials.
- `app/airlockd/src/process.rs`: command spawning and privilege handling.
- `app/airlockd/src/net/grant_ns.rs`: grant network namespaces and teardown.

These observations come from local source inspection on 2026-11-08. The
proposed adapter and authentication flow have not been implemented or tested.
