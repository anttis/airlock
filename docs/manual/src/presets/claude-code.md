# Claude Code

The `claude-code` preset bundles the sandbox setup for running
[Claude Code](https://docs.claude.com/en/docs/claude-code/overview)
inside airlock. It configures the network rules, credential handling,
and settings persistence. You only need to pick an image that ships
the `claude` CLI and add the preset to your config.

## What the preset does

The real OAuth token stays on the host. The VM sees a same-length
random surrogate, and airlock inserts the real token at the host
boundary.

- **Your token stays on the host.** `CLAUDE_CODE_OAUTH_TOKEN` is
  [masked](../configuration/env.md#masking) and the `claude-code`
  rule [injects](../configuration/network.md#injecting-masked-secrets)
  the real value into request headers to the Anthropic hosts.
- **Only Anthropic endpoints are reachable** (`api.anthropic.com`,
  `claude.ai`, `downloads.claude.ai`, `platform.claude.com`).
  Everything else stays blocked by your deny-by-default policy.
- **Claude knows it's sandboxed.** The preset sets `IS_SANDBOX=1` so
  Claude skips host-only behaviour, and points `NODE_EXTRA_CA_CERTS`
  at the airlock CA so the middleware's TLS interception is trusted.
- **Your onboarding survives.** `~/.claude` and `~/.claude.json`
  inside the sandbox are backed by `~/.airlock/claude/settings` and
  `~/.airlock/claude/claude.json` on the host, so login state,
  preferences, and project memory persist between sandbox runs.
  Disable either mount in `airlock.local.toml` if you prefer a
  fresh sandbox each time.

## Example `airlock.toml`

```toml
presets = ["claude-code"]

[network]
policy = "deny-by-default"

[vm]
image = "docker/sandbox-templates:claude-code"
```

The `docker/sandbox-templates:claude-code` image ships with `claude`
already installed. For a real project, you might prefer your own
[project-specific image](../tips/mise.md#building-a-local-image-with-docker).

## Providing the OAuth token

The preset expects `CLAUDE_CODE_OAUTH_TOKEN` on the **host**.
Get one by running `claude setup-token` outside the sandbox.

Store the token in the airlock
[secret vault](../secrets.md) under the name `CLAUDE_CODE_OAUTH_TOKEN`:

```bash
airlock secrets add CLAUDE_CODE_OAUTH_TOKEN
```

## Running it

```bash
airlock start --monitor -- claude --dangerously-skip-permissions
```

## Mounting your host Claude settings

By default, the `claude-code` preset mounts Claude settings from the
`~/.airlock/claude` directory so the sandboxed Claude doesn't touch
your primary host settings. If you'd rather share the host settings
into the VM, point the default mount sources at them:

```toml 
[mounts.claude-settings]
source = "~/.claude"

[mounts.claude-json]
source = "~/.claude.json"
```
