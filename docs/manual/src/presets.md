# Presets

Presets are built-in configuration bundles that ship with airlock. Without
them, you must list every package registry and cache directory for your tech
stack, and every API endpoint and credential mount for an AI agent. With
them, you pick the relevant bundles and they handle the details.

## Using presets

Add presets to the top-level `presets` array in your config:

```toml
presets = ["debian", "rust", "claude-code"]

[vm]
image = "ubuntu:24.04"
```

airlock applies presets as a base layer. Your own configuration always takes
priority and overrides anything a preset defines. You can combine multiple
presets freely.

## Distribution presets

These open network access to the package repositories for each Linux
distribution so that commands like `apt install` and `apk add` work.

- **`alpine`** — Alpine Linux package mirrors
- **`debian`** — Debian and Ubuntu package repositories (including PPAs and
  security updates)
- **`fedora`** — Fedora, CentOS, and RHEL package mirrors
- **`arch`** — Arch Linux and AUR repositories
- **`suse`** — openSUSE and SUSE update servers

Pick the one that matches your base image. For `ubuntu:24.04`, the
`debian` preset is the right choice.

## Language presets

These open network access to language-specific package registries so your
package manager can fetch dependencies.

- **`rust`** — crates.io and Rust toolchain downloads
- **`python`** — PyPI
- **`nodejs`** — npm and Yarn registries

## AI agent presets

These configure network rules, credential forwarding, and settings mounts
for popular AI coding agents. Each agent has its own chapter with the full
setup — what the preset configures, which secret or environment variable it
expects, and an example `airlock.toml`:

- [Claude Code](./presets/claude-code.md)
- [GitHub Copilot CLI](./presets/copilot-cli.md)
- [OpenAI Codex](./presets/openai-codex.md)

Missing a preset for your favourite agent? PRs welcome — the presets live
as small TOML files under `app/airlock-cli/src/config/presets/`.

## Combining presets with custom rules

A typical project config combines a distribution preset with a language
preset and an agent preset, then adds project-specific rules on top:

```toml
presets = ["debian", "python", "claude-code"]

[vm]
image = "ubuntu:24.04"

[network]
policy = "deny-by-default"

[network.rules.internal-api]
allow = ["api.internal.company.com:443"]

[network.middleware.internal-api-auth]
target = ["api.internal.company.com:443"]
env.TOKEN = "${INTERNAL_API_TOKEN}"
script = '''
req:setHeader("Authorization", "Bearer " .. env.TOKEN)
'''
```

This gives you Debian package repositories, PyPI, Claude API access, and your
internal API — all in a deny-by-default sandbox.

## Overriding preset rules

Since presets are regular configuration applied at a lower priority, you
can override or disable any rule they define. If a preset opens network
access to something you don't need, disable it in your project config or
local overrides:

```toml
# airlock.local.toml
[network.rules.alpine-packages]
enabled = false
```

See the [Configuration](./configuration.md) chapter for more on how the
hierarchical config system and `enabled` flags work together.
