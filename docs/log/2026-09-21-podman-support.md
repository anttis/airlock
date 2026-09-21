# Podman support for local image resolution

## Motivation

Local image resolution shelled out to a hardcoded `docker` binary and
parsed only the OCI-layout archive that Docker 25+ emits from `docker
image save` (`blobs/sha256/<hex>`). Podman users always fell through to
the registry, even though every subcommand airlock uses — `images`,
`image inspect`, `image save` — exists with the same flags and output in
the podman CLI.

## Changes

* `resolution` gains a `podman` value. `auto` now means Docker, then
  Podman, then the registry; `docker` and `podman` each pin one engine.
  Nothing sniffs `PATH`: an engine that isn't installed fails to spawn,
  which the resolver already treats as "image not found" and moves on.
* `oci::docker` takes the engine binary as an argument instead of
  hardcoding `docker`.
* The `image save` parser accepts podman's default `docker-archive`
  layout (`<hex>.tar` layers, `<hex>.json` config) alongside the OCI
  layout. One `blob_hex` helper maps either member name to its digest and
  rejects the legacy `<id>/layer.tar` members, which are not
  content-addressed. Content is still hashed against the name before it
  enters the shared layer cache.

## Notes

Podman's `docker-archive` stores layers uncompressed, so their names are
diff IDs rather than compressed registry digests. The same image pulled
from a registry lands under different layer-cache keys. That costs disk
but cannot collide, since both keys are verified by content.
