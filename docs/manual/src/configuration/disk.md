# Disk and cache

airlock creates a sparse ext4 disk image for each project sandbox. This disk
persists writes that happen outside of mounted host directories — things like
installed system packages, global tool caches, or any files the container
process creates on the root filesystem.

The disk is 10 GB by default (sparse, so it only uses actual disk space for
data written). You can change the size:

```toml
[disk]
size = "20 GB"
```

## Resizing

If you increase `disk.size` in the config, airlock grows the disk image on
the next start and expands the ext4 filesystem automatically inside the VM.
Existing data survives — this is a safe operation.

If you decrease `disk.size`, airlock asks for confirmation on the next start,
because shrinking means destroying all data on the disk — installed
packages, named caches, and any other files stored outside mounted host
directories. If you confirm, airlock
erases the disk image and recreates it at the smaller size. If you decline,
airlock keeps the existing larger disk untouched. When it runs without a
terminal (non-interactive), airlock never erases the disk on its own — it
keeps the larger image and prints a warning.

## Named caches

When you change the project's OCI image (e.g. upgrading from `ubuntu:22.04`
to `ubuntu:24.04`), airlock resets the disk contents to match the new image. This
is usually what you want — a clean slate — but some directories are worth
preserving across image changes.

Named caches solve this. Each cache entry lists one or more container paths
to back with persistent storage that survives image changes:

```toml
[disk.cache.cargo]
paths = ["~/.cargo/registry"]

[disk.cache.node-modules]
paths = ["node_modules"]
```

airlock resolves relative paths (like `node_modules`) against the project
directory inside the container. It expands paths starting with `~` to the
container user's home directory.

This is especially useful for package manager caches. Without named caches,
switching the base image would force a full re-download of all dependencies.
With them, `cargo build` or `npm install` continues from where it stopped.

You can disable a cache temporarily without removing it from the config:

```toml
[disk.cache.cargo]
enabled = false
paths = ["~/.cargo/registry"]
```
