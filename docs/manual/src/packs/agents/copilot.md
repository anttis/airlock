# GitHub Copilot CLI

The `copilot` pack installs the
[GitHub Copilot CLI](https://docs.github.com/en/copilot/concepts/agents/about-copilot-cli)
into the sandbox. The ACP support is built in (`copilot --acp`). See [editor integration](../../tips/editor-acp.md).

```toml
[packs]
copilot = { version = "3" }
```

```bash
airlock start --monitor -- copilot
```

The pack has no args.

## Authentication

### GitHub token

Create a [fine-grained personal access token][new-pat] with the
**Account > Copilot Requests** permission. Store it in the airlock
[secret vault](../../secrets.md):

[new-pat]: https://github.com/settings/personal-access-tokens/new

```bash
airlock secrets add COPILOT_GITHUB_TOKEN
```

airlock reads the token from the host environment first and then from the
vault. The token is optional. If neither has the token, the sandbox starts
without `COPILOT_GITHUB_TOKEN`.

If the token is set, the sandbox sees only a
[masked](../../configuration/env.md#masking) surrogate, so you do not need
`/login`. airlock puts the real token into the requests. For the supported
token types, see the [Copilot CLI authentication docs][copilot-auth].

[copilot-auth]: https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli#authenticating-with-environment-variables

## Airlock tools

Since version 2, the pack configures Copilot to use the `airlock` MCP
server for advanced interactive sandbox control. See
[Temporary network grants](../../technical/networking.md#temporary-network-grants).

| Version | MCP connection | `execute` commands run as |
|---------|----------------|---------------------------|
| 3       | Stdio adapter `/run/airlock/bin/airlock-mcp` | The user of Copilot |
| 2       | HTTP endpoint `http://admin.airlock/mcp` (deprecated for `execute`) | The sandbox user |

Version 3 is the same as version 2, except for the MCP entry. The
adapter lets airlock get the user of Copilot from the kernel. Over HTTP,
airlock cannot get this user. See [MCP](../../technical/mcp.md#admin-mcp-server).

The setup wizard offers version 3. A sandbox with version 2 keeps its
install. To use the adapter, set `version = "3"`. A change of the
version is a [changed pack](../../technical/packs.md#install-state).

The pack writes the MCP entry to
`/usr/local/share/airlock/copilot-mcp.json`. The `copilot` command of
the pack gives this file to Copilot.

### Without the pack

For Copilot without the pack, add a `local` server to
`~/.copilot/mcp-config.json`:

```json
{
  "mcpServers": {
    "airlock": {
      "type": "local",
      "command": "/run/airlock/bin/airlock-mcp",
      "args": [],
      "tools": ["*"],
      "timeout": 86400000
    }
  }
}
```

An `execute` call waits for your approval. Thus the pack sets a timeout
of 24 hours (in milliseconds). Copilot refuses a timeout of 0.
