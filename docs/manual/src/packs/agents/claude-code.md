# Claude Code

The `claude` pack installs
[Claude Code](https://code.claude.com/docs/en/overview) into the sandbox.

```toml
[packs]
claude = { version = "3" }
```

```bash
airlock start --monitor -- claude --dangerously-skip-permissions
```

| Arg   | Default | Description                                               |
|-------|---------|-----------------------------------------------------------|
| `acp` | `false` | Install the ACP adapter as the `claude-agent-acp` command. See [editor integration](../../tips/editor-acp.md). |

## Authentication

### Login in the application

Start `claude` in the sandbox and sign in with `/login`. The sign-in page
opens in the browser of the host.

airlock keeps the real tokens on the host, encrypted with a key from the
[secret vault](../../secrets.md). Claude Code gets only surrogate tokens.

The sign-in is shared:

- A sign-in in one sandbox applies to all sandboxes with the `claude`
  pack.
- A logout in one sandbox signs out all sandboxes.

### Token or API key

Set `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`) or
`ANTHROPIC_API_KEY` on the host, or store it in the
[secret vault](../../secrets.md):

```bash
airlock secrets add CLAUDE_CODE_OAUTH_TOKEN
```

Claude Code then uses it automatically. The real value stays on the host.
Claude Code gets only a surrogate.

## Airlock tools

Since version 2, the pack configures Claude Code to use the `airlock` MCP
server for advanced interactive sandbox control. See
[Temporary network grants](../../technical/networking.md#temporary-network-grants).

| Version | MCP connection | `execute` commands run as |
|---------|----------------|---------------------------|
| 3       | Stdio adapter `/run/airlock/bin/airlock-mcp` | The user of Claude Code |
| 2       | HTTP endpoint `http://admin.airlock/mcp` (deprecated for `execute`) | The sandbox user |

Version 3 is the same as version 2, except for the MCP entry. The
adapter lets airlock get the user of Claude Code from the kernel. Over HTTP,
airlock cannot get this user. See [MCP](../../technical/mcp.md#admin-mcp-server).

The setup wizard offers version 3. A sandbox with version 2 keeps its
install. To use the adapter, set `version = "3"`. A change of the
version is a [changed pack](../../technical/packs.md#install-state).

The pack writes the MCP entry to `$HOME/.claude.json` of the user that
runs the pack setup. Other users in the sandbox do not get the entry.

### Without the pack

For another user in the sandbox, or for Claude Code without the pack,
add the server as that user:

```bash
claude mcp add --scope user airlock /run/airlock/bin/airlock-mcp
```

> [!NOTE]
> A user change with `su -` removes the sandbox environment. Then Claude
> Code does not get `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=0` from the pack.
> Without this value, Claude Code stops an `execute` call after 5
> minutes of idle time, also while the call waits for your approval. Set
> the value again in the new shell.

## Clipboard

The `claude` command sets `WAYLAND_DISPLAY` only when the
[clipboard](../../configuration/clipboard.md) `copy` permission is on.
Claude Code needs this variable to copy to your clipboard. You do not
need to set it in `[env]`.

## Updates

The pack turns off the background updates of Claude Code. A new sandbox
gets the newest version. You can also run `claude update`. The `claude`
command always runs the newest installed version.
