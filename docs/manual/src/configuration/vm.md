# VM options

The `[vm]` section controls the virtual machine image and resource allocation.

## Image

The `image` field specifies which OCI image to use as the container root
filesystem. By default, airlock uses `alpine:latest`:

```toml
[vm]
image = "ubuntu:24.04"
```

For more control — for example when pulling from a private or local
registry — use the object form:

```toml
[vm.image]
name = "registry.company.com/base-image:latest"
resolution = "registry"
```

The `resolution` field controls where airlock looks for the image:

- `auto` (default) — try the local Docker daemon first, fall back to the OCI
  registry. This is convenient if you already have the image locally.
- `docker` — only use the local Docker daemon. Fails if the image isn't found.
- `registry` — always pull from the OCI registry, ignore Docker entirely.
  This is the right choice when Docker isn't installed.

For development registries served over plain HTTP, set `insecure = true`:

```toml
[vm.image]
name = "localhost:5005/dev-image:latest"
resolution = "registry"
insecure = true
```

## Pinning a digest

An image name can pin an exact digest with `@sha256:…`, the same way Docker
accepts it — with or without a tag:

```toml
[vm]
image = "ubuntu:24.04@sha256:8f9e08b6a0b1e0b0f1f2a3c4d5e6f708192a3b4c5d6e7f80912a3b4c5d6e7f80"
```

The digest decides which image you get. The tag alongside it is just a label.
Pin one when the sandbox should keep running the same image even if the tag
later moves to something else.

If a local Docker image carries the same tag but isn't that exact image,
airlock ignores it and pulls the pinned one instead.

## Pull policy

`pull-policy` controls how often airlock checks whether the image has changed:

```toml
[vm.image]
name = "ubuntu:24.04"
pull-policy = "if-changed"
```

- `if-not-present` (default) — once an image is cached under this name, use it
  and skip the network entirely. Fast, but a tag that has moved in the registry
  goes unnoticed until you change the name or clear the cache.
- `if-changed` — check on every start, and keep using the cached image only
  while it's still current. If it has changed you get the usual "Image has
  changed" prompt, so airlock never re-creates anything without asking.

`if-changed` adds a short registry check to every start. If that check fails
— no network, registry down — airlock asks whether to continue with the
image you already have. Scripted runs don't ask — they stop with an error.

A pinned digest ignores this setting, since a pinned image can't change.

## Resources

By default, airlock allocates all available host CPUs and half the system RAM
to the VM. You can override these:

```toml
[vm]
cpus = 4
memory = "4 GB"
```

Memory accepts human-readable sizes like `"512 MB"`, `"4 GB"`, or `"2G"`.
The minimum is 512 MB, and the maximum is the total system RAM.

## Security hardening

The VM boundary is the primary isolation layer, but airlock also applies
some process-level hardening inside the VM: namespace restrictions and
root elevation prevention. To disable the process-level hardening, override
the `harden` field with `false`. Disable it only when a workload genuinely
needs the broader kernel capabilities that `harden` removes.

```toml
[vm]
harden = true   # default = true
```

## Custom kernel and initramfs

The `kernel` and `initramfs` fields point airlock at external kernel and
initramfs files instead of the bundled ones. See
[Custom kernel](../advanced/custom-kernel.md) for when and how to use them.

## Nested KVM (Linux only)

On Linux hosts with KVM support, you can expose `/dev/kvm` into the
guest so VMs running *inside* the sandbox get hardware acceleration:

```toml
[vm]
kvm = true
```

You need this to run, say, `qemu-system-*` or another hypervisor
from inside the sandbox without falling back to software
emulation. It's only available on Linux and requires `/dev/kvm`
access on the host — Apple Virtualization on macOS doesn't expose
nested virt.

