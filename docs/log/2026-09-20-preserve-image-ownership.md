# Preserve numeric image ownership during extraction

OCI extraction previously assigned entries to the extracting user, discarding
numeric UID/GID from the image. Enable tar ownership preservation and bump the
layer cache format and cached-image schema to version 3 to force re-extraction.
This is the requested first experiment with host-side ownership preservation.

Add a privileged regression test for distinct ownership on files, directories,
symlinks, and hardlinks. Existing tar fixtures now specify numeric owners. The
regression fails before the flag and passes afterward. All 13 layer tests pass
as root on native Linux tmpfs; 12 pass unprivileged, with the privileged test
ignored. Tests use a standalone harness importing the production modules, built
with Rust 1.94.1, rather than a full workspace test run. The full `mise lint`
check also passes with the pinned Rust 1.97 toolchain.

Unprivileged extraction can fail when chown is not permitted. Apple VirtioFS and
NVM behavior still need end-to-end verification. Existing persistent overlay
uppers are unchanged and can shadow corrected lower ownership; use a fresh
sandbox for verification. Special mode-bit preservation is outside this change.
