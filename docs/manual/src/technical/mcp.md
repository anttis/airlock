# MCP

The supervisor in the VM serves all MCP endpoints of the sandbox. One
local HTTP server on the guest loopback (port 80) serves the HTTP
endpoints. The virtual DNS resolves all `*.airlock` names to this
address (see [Networking](./networking.md#virtual-dns)). The traffic
never goes through the TCP proxy and never leaves the VM. The server
picks the endpoints by the host name of the request:

| Host name         | Endpoints                                        |
|-------------------|--------------------------------------------------|
| `admin.airlock`   | Admin service, with the `airlock` MCP server     |
| `mcp.airlock`     | MCP servers of the config, `/<name>`             |
| `acp.mcp.airlock` | MCP servers of editor sessions, `/<key>/<name>`  |

All HTTP endpoints use the MCP Streamable HTTP transport. They refuse
requests from web pages of other origins. This is a guard against DNS
rebinding.

The supervisor also serves the `airlock` MCP server on a local Unix
socket. See [Local socket](#local-socket).

## Admin MCP server

The `airlock` MCP server gives the agent tools for the network policy.
It has two endpoints:

| Endpoint                   | Transport                         | Commands of `execute` run as   |
|----------------------------|-----------------------------------|--------------------------------|
| `/run/airlock/mcp.sock`    | Unix socket, one message per line | The user of the client         |
| `http://admin.airlock/mcp` | Streamable HTTP                   | The sandbox user (deprecated)  |

Agents connect to the socket through the
[stdio adapter](#stdio-adapter). Both endpoints offer the same tools.
The server offers only the tools of the features that the host enables
at boot:

| Tool                  | Offered when                               |
|-----------------------|--------------------------------------------|
| `execute`             | `network.grants.enabled = true`            |
| `show_rules`          | `network.rule_editing` is not `"none"`     |
| `upsert_rule`         | `network.rule_editing` is not `"none"`     |
| `list_manual_entries` | Always                                     |
| `get_manual_entry`    | Always                                     |

The manual tools give the Markdown pages of this manual. The build puts
the pages into the supervisor. The server also gives the agent a text
with instructions. The text describes only the enabled tools. For the
meaning of grants, on-demand rules and rule changes, see
[Temporary network grants](./networking.md#temporary-network-grants).

The [agent packs](../packs/agents.md) `claude@3`, `codex@3` and
`copilot@3` connect the agent to the server through the stdio adapter.
The `@2` versions of these packs use the HTTP endpoint. All these packs
also give the agent the tool instructions at the start of each session.
Hooks on the HTTP endpoint tell the agent when the network policy
blocked a connection during a tool call. Codex runs shell commands in
its own sandbox. That sandbox has no network unless you start Codex with
`--yolo`. Without `--yolo`, airlock sees no denied connections.

### Local socket

The socket `/run/airlock/mcp.sock` is in `/run/airlock`. This directory
is a tmpfs that the supervisor makes at each boot. It belongs to root
and has mode 0755. Thus guest users cannot replace the files in it. The
socket has mode 0666, so all guest users can connect.

- Each connection is one MCP session. The messages are JSON-RPC, one
  message on each line. This is the same framing as the MCP stdio
  transport.
- At connect time, the kernel gives the user ID, the group ID and the
  process ID of the client. The `execute` commands of the session run as
  this user. No tool argument can change the user. See
  [Command user](#command-user).
- A client must complete the MCP initialization in 30 seconds. If it
  does not, the supervisor closes the connection.
- At most 64 connections can be open at the same time. The supervisor
  closes a new connection above this limit.
- When the client disconnects, the session ends and the supervisor stops
  the running command. This can take up to 5 seconds, because the MCP
  library first waits for the pending replies.

### Stdio adapter

Agents start a local MCP server as a program with stdin and stdout. The
adapter `/run/airlock/bin/airlock-mcp` is such a program. It is the
supervisor binary in a read-only bind. Its program name selects the
adapter mode.

- The agent starts the adapter as a stdio MCP server. The adapter runs
  as the user of the agent and has no other rights. Thus the server gets
  the user of the agent from the kernel.
- The adapter sends the bytes of its stdin to the socket, and the bytes
  of the socket to its stdout. It does not change the messages.
- At the end of stdin, the adapter still sends the pending replies to
  stdout. It stops when the server closes the connection.
- If the socket is not available, the adapter writes an error to stderr
  and exits with code 1. It never uses the HTTP endpoint.

The guest controls the image. A symlink at `/run` or `/run/airlock` in
the image could send the tmpfs mount to a different path. Thus the
supervisor does not mount the tmpfs if one of them is a symlink. Then
the socket and the adapter do not exist, and the agent cannot start the
adapter (command not found). The clipboard and browser bridges still
work in this case.

### HTTP endpoint

The endpoint `http://admin.airlock/mcp` is stateless and sends JSON
responses. It has no MCP sessions. It accepts only the `Host` names
`admin.airlock`, `localhost` and `127.0.0.1`.

The HTTP endpoint cannot get the user of the client from the kernel.
Thus its `execute` tool is deprecated:

- It still runs the commands as the sandbox user.
- Its tool description starts with a `DEPRECATED` note that names the
  stdio adapter.
- Each result has a `warning` field with the deprecation text.
- A later release removes `execute` from this endpoint.

The manual tools, the rule tools and the hooks of the agent packs stay
on the HTTP endpoint with no change.

### Request flow

The tools do not decide. The host CLI decides. The supervisor sends each
request to the host over the network channel (see [RPC](./rpc.md)):

1. The tool checks its arguments. For `execute`, it refuses environment
   keys that change program start, code loading, certificates or
   proxies, for example `PATH` and `LD_*`.
2. The supervisor sends the request to the host. For `execute`, the
   request also has the user of the command. The host checks the request
   against the config and refuses a request that the config does not
   permit. It does not ask the user then.
3. The host asks the user (see [Approvals](./approvals.md)). The answer
   can take minutes. If the MCP client disconnects, the supervisor drops
   the request, and the host removes the approval request.
4. `show_rules` and `upsert_rule` return the result of the host.
   `execute` continues as below.

An approved `execute` gets a grant proxy from the host. The grant proxy
allows the approved hosts or the rule, in addition to the normal policy.
The supervisor then:

1. Makes a network namespace for the grant. A virtual cable connects it
   to the main network of the VM, with a source address that is unique
   for the grant. The TCP proxy sends the connections from this address
   through the grant proxy. The main network refuses local packets that
   use a grant address as source. Thus other processes cannot use the
   grant without root rights.
2. Runs the command with `sh -c` in the namespace, as the
   [command user](#command-user). The command gets the sandbox
   environment and the CA variables.
3. Stops the command when the time limit of the grant expires, when the
   MCP client disconnects, or when the user stops the grant on the host.
4. Reads the grant log: the hosts that the command connected to, and if
   the network blocked them.
5. Removes the namespace and releases the grant. The host then closes
   the open connections of the grant.

The result has the exit code, the last 64 KiB of stdout and of stderr,
the hosts, and a flag that tells if the user stopped the command. On the
HTTP endpoint, it also has the `warning` field.

### Command user

The endpoint of the client selects the user of the command:

| Endpoint     | User and group                                          |
|--------------|---------------------------------------------------------|
| Local socket | User ID and primary group ID of the client, from the kernel |
| HTTP         | The sandbox user, which the sandbox boots with          |

The command has no supplementary groups. If the user is not the sandbox
user, the supervisor changes some values of the sandbox environment:

- `HOME` is the home directory of the user in `/etc/passwd` of the
  container. If the file has no home for the user, `HOME` is `/`.
- `USER` and `LOGNAME` get the user name from `/etc/passwd`, but only if
  the sandbox environment has these keys.

The supervisor does not follow a symbolic link at `/etc` or at
`/etc/passwd` of the container. If one of them is a link, the supervisor
uses no account data: `HOME` is `/`, `USER` and `LOGNAME` do not change,
and the approval prompt shows only the user ID.

A `HOME` in the `env` argument of `execute` overrides the value above.

The supervisor enters the working directory after it changes the user.
If the user cannot enter the directory, the command starts in `/`. This
applies to all processes in the sandbox.

### Security notes for the admin server

- The socket identifies users, not processes. All processes of one user
  have the same access.
- A root process in the sandbox can act as any user.
- The HTTP endpoint has no per-process access control. Any process in
  the sandbox can call it.

## Host MCP servers

The servers of `[mcp.<name>]` (see [MCP servers](../configuration/mcp.md))
run on the host, with the rights of the user. The guest relays the
JSON-RPC messages without change. The host sends them to the stdin of
the server program and reads the replies from its stdout, one message on
each line.

The boot config gives the guest the names of the enabled servers and one
host capability that starts them. The guest cannot start other programs.
Commands, arguments and environment values stay on the host. The host
resolves the `${VAR}` values and never sends them to the guest.

### Sessions

1. An `initialize` request without a session ID opens a session. The
   guest makes a random session ID and asks the host to start a new
   process of the server. The working directory is the project
   directory.
2. Each later request gets its reply as an event stream. The stream also
   carries the server messages that arrive while the request is open.
3. A `GET` opens one more stream for server messages that arrive while no
   request is open. The guest keeps up to 256 of these messages. Then it
   drops the oldest.
4. The session ends when the client sends `DELETE`, when the server
   process stops, or when the host stops the session.

Each session has its own process. Thus the sessions stay apart, and
the request IDs do not change. The server program writes its stderr to
the log. When a session ends, the host stops the process and all
processes that it started.

The host stops a session after the idle `timeout`, and the session with
the oldest activity when a new session exceeds `max_sessions`.

### Editor sessions

With `--proxy-mcp-servers`, [`airlock acp`](../tips/editor-acp.md#editor-mcp-servers)
adds the MCP servers of the editor at run time. The editor can give
local (stdio) servers and remote servers (HTTP and SSE). The host runs
the local servers and relays the remote servers. URLs and headers also
stay on the host.

The servers of one editor session get a random key of 16 hex characters.
The route is `http://acp.mcp.airlock/<key>/<name>`. Thus the servers of
different editor sessions stay apart. The host adds each route with
`addMcpServer` and keeps the grant that the call returns. The route ends
when the host releases or drops the grant. This also ends the open
sessions of the route. The host releases the grants when the editor
closes or loads the session again, and when the `airlock acp` process
stops. The guest accepts at most 256 run-time routes.

### Security notes

- Any process in the sandbox can call a host server. The sandbox has no
  per-process access control for MCP.
- A host server acts outside the sandbox. The network policy does not
  apply to it.
- The config-change prompt protects the `[mcp.*]` entries. A project
  config or a pack cannot add a server without your approval.
