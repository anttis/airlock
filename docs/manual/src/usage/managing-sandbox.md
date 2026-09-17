# Managing sandbox data

## Viewing sandbox status

The `airlock show` command displays the current sandbox configuration and
status for the project:

```bash
airlock show
```

The output includes the image name, CPU and memory allocation, disk usage,
configured mounts, network rules, and whether the sandbox is currently
running. This is a quick way to verify your configuration without opening
the TOML file.

Example output:

```
Path:     /Users/me/my-project
Status:   running
Image:    ubuntu:24.04
CPUs:     4
Memory:   2.0 GB
Last run: 2 minutes ago

Sandbox:  /Users/me/my-project/.airlock/sandbox
Disk:     1.2 GB / 10.0 GB

Mounts:
  ssh-config: ~/.ssh/config → ~/.ssh/config

Network rules (default: deny):
  my-api: allow 2, 1 middleware
```

## Removing sandbox state

The `airlock remove` command removes the sandbox state for the current
project. This removes the `.airlock/sandbox/` directory, which includes the
disk image, the CA certificate, and other runtime state:

```bash
airlock remove
```

airlock asks you to confirm before it removes anything. To skip the
confirmation prompt (useful in scripts), pass `--force`:

```bash
airlock remove --force
```

The short alias `airlock rm` also works.

After removal, running `airlock start` again creates a fresh sandbox from
scratch — new disk, new CA certificate, fresh image pull if needed. Removal does
not affect the project configuration files (`airlock.toml`,
`airlock.local.toml`).

## The `.airlock/` directory

Each project that uses airlock has a `.airlock/` directory at its root.
Sandbox state lives inside the project (rather than in a global location
like `~/.airlock/`) so that each checkout gets its own isolated sandbox.
Work on two branches in parallel, clone the same repo twice, or
`airlock rm` a feature branch's state — none of these touches anything
else. The directory contains a `.gitignore` with `*`, which excludes it
from version control automatically. Inside it, the `sandbox/`
subdirectory holds all runtime state:

| File / Directory | Purpose                                                         |
|------------------|-----------------------------------------------------------------|
| `lock`           | PID lock file preventing concurrent sandbox instances           |
| `ca.json`        | Per-project CA certificate and private key for TLS interception |
| `disk.img`       | Sparse ext4 disk image for persistent VM storage                |
| `image`          | Link to the cached OCI image                                    |
| `cli.sock`       | Unix socket `airlock exec` connects to                          |
| `run.json`       | Metadata from the last run (timestamp, working directory)       |
| `overlay/`       | Internal staging directory for file mounts                      |

The `tracing` log lives one level up, at `.airlock/airlock.log`.

You should never need to touch these files directly. If something goes wrong,
`airlock rm` and a fresh `airlock start` is the cleanest recovery path.
