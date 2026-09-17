# Attaching to a running sandbox

Once a sandbox is running (started with `airlock start`), you can open
additional sessions into it using `airlock exec`. This is useful when you
need a second terminal inside the VM — for example, to run tests in one
window while an agent works in another.

```bash
airlock exec bash
```

The shorthand alias `airlock x` does the same thing:

```bash
airlock x bash
```

You can run any command, not just shells:

```bash
airlock exec cat /etc/os-release
airlock exec python3 -m pytest tests/
```

## Working directory

`airlock exec` can be used from any subdirectory under the sandbox
root; `airlock exec` walks up from the current directory to find a
running sandbox VM. The executed command has the same working
directory as in the host machine. To override the directory, use `--cwd`
(or `-w`):

```bash
# /home/example/my-project
#   .airlock
#   airlock.toml
#   src 

cd src
airlock x pwd          # prints: /home/example/my-project/src
airlock x -w /tmp pwd  # prints: /tmp
```

## Environment variables

Pass extra environment variables with `-e` (repeatable):

```bash
airlock exec -e DEBUG=1 -e LOG_LEVEL=trace ./run-tests.sh
```

airlock layers them on top of the sandbox's resolved environment
(image env + `airlock.toml` env). Entries with the same key replace
the base value.

## Login shell

Like `start`, the `--login` flag sources profile scripts before running the
command:

```bash
airlock exec --login bash
```

