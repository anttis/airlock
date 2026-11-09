# OpenAI Codex

The `codex` pack installs the
[OpenAI Codex CLI](https://github.com/openai/codex) into the sandbox.

```toml
[packs]
codex = { version = "3" }
```

```bash
airlock start --monitor -- codex --yolo
```

| Arg   | Default | Description                                        |
|-------|---------|----------------------------------------------------|
| `acp` | `false` | Install the ACP adapter as the `codex-acp` command. See [editor integration](../../tips/editor-acp.md). |

## Authentication

### Login in the application

Run `codex login` in the sandbox. The sign-in page opens in the browser of
the host. `codex login --device-auth` also works.

airlock keeps the real tokens on the host, encrypted with a key from the
[secret vault](../../secrets.md). Codex gets only surrogate tokens.

The sign-in is shared:

- A sign-in in one sandbox applies to all sandboxes with the `codex`
  pack.
- A logout in one sandbox signs out all sandboxes.

### API key

Set `OPENAI_API_KEY` on the host, or store it in the
[secret vault](../../secrets.md):

```bash
airlock secrets add OPENAI_API_KEY
```

Codex then uses it automatically. The real value stays on the host. Codex
gets only a surrogate.

## Airlock tools

Since version 2, the pack configures Codex to use the `airlock` MCP
server for advanced interactive sandbox control. See
[Temporary network grants](../../technical/networking.md#temporary-network-grants).

| Version | MCP connection | `execute` commands run as |
|---------|----------------|---------------------------|
| 3       | Stdio adapter `/run/airlock/bin/airlock-mcp` | The user of Codex |
| 2       | HTTP endpoint `http://admin.airlock/mcp` (deprecated for `execute`) | The sandbox user |

Version 3 is the same as version 2, except for the MCP entry. The
adapter lets airlock get the user of Codex from the kernel. Over HTTP,
airlock cannot get this user. See [MCP](../../technical/mcp.md#admin-mcp-server).

The setup wizard offers version 3. A sandbox with version 2 keeps its
install. To use the adapter, set `version = "3"`. A change of the
version is a [changed pack](../../technical/packs.md#install-state).

The pack writes the MCP entry to `/etc/codex/config.toml`.

### Without the pack

For Codex without the pack, add the server to `~/.codex/config.toml`:

```toml
[mcp_servers.airlock]
command = "/run/airlock/bin/airlock-mcp"
tool_timeout_sec = 86400
```

Codex cannot turn off the tool timeout. An `execute` call waits for your
approval, so the pack sets a timeout of one day.
