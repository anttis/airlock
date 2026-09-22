# Nested virtualization on Apple silicon

## Motivation

Running Airlock inside an Airlock sandbox needs a guest kernel with KVM
and virtual CPU support for hosting another VM. Previously, `vm.kvm = true`
failed configuration parsing on macOS with `kvm is only supported on Linux`.
The Apple backend did not enable nested virtualization, and the bundled
ARM64 kernel did not include KVM. Creating a device node alone cannot supply
either capability.

## Implementation

- Accept `vm.kvm` during configuration parsing and pass it to both VM backends.
  Keep its default false. Host capability checks belong in the backend.
- On macOS, check API availability for macOS 15 before calling
  `VZGenericPlatformConfiguration::isNestedVirtualizationSupported`.
  Reject unsupported hosts with a clear error. Enable nested virtualization
  only after both checks pass. Apple's API is the authority for support,
  including hardware and policy restrictions.
- Enable `CONFIG_VIRTUALIZATION=y` and `CONFIG_KVM=y` in the ARM64 kernel.
  The existing supervisor setup exposes `/dev/kvm` to the sandbox only when
  `vm.kvm` is true. No broader device exposure is necessary.
- Update the manual with macOS requirements and restart instructions.

Reusing `vm.kvm` keeps one explicit opt-in for CPU virtualization support and
guest device access. The Linux backend retains its existing behavior.

## Validation

- All 344 existing Rust tests passed on Linux ARM64 during implementation.
- Two configuration regression tests cover accepting `vm.kvm = true` and
  leaving it disabled by default. All 66 configuration tests passed after
  removing the macOS rejection. Temporarily enabling that rejection on Linux
  reproduced the exact error before the fix.
- Formatting and manual lint passed. Manual lint reported only four advisory
  findings in unchanged text.
- The user ran `mise lint` successfully on the macOS host before commit.
- The ARM64 kernel built successfully with the sandbox CA bundle mounted into
  the Docker build container. Its embedded configuration contains both KVM
  settings.
- The user built the macOS binary on an M5 running macOS 26.5.1. The opt-in
  `AIRLOCK_TEST_NESTED_KVM=1 mise x -- bats tests/vm/kvm.bats` test passed:
  the Linux guest opened `/dev/kvm`, obtained KVM API version 12, and created
  an empty VM. The test remains opt-in because not every host supports nesting.
- After installing that host binary and restarting the outer sandbox, the
  released Linux Airlock v2026.9.2 booted an inner Alpine 3.23.6 VM with one
  CPU and 512 MiB RAM. It executed a command as root and exited successfully.
- A boot-suite regression test checks that `/dev/kvm` stays hidden by default.
  That new assertion has not yet been run on the macOS host.

## Ramifications

- Enabling nesting increases the attack surface available to processes that
  can open `/dev/kvm`: they can exercise guest KVM code and Apple's nested
  virtualization implementation. The outer VM boundary remains, but it must
  handle these additional workloads. This is a security tradeoff, not evidence
  of a known vulnerability or a host escape.
- Inner VMs consume CPU, RAM, and disk from the outer sandbox's allocation.
  Heavy workloads can slow other sessions or exhaust available resources.
  Performance overhead and sustained resource use have not been benchmarked.
- `vm.kvm` remains false by default. On macOS, requesting it now succeeds only
  if the OS version and Apple's support API permit nesting. Unsupported hosts
  receive an error instead of silently running without acceleration.
- KVM is compiled into every bundled ARM64 kernel, including kernels used by
  sandboxes that leave nesting disabled. The kernel change also applies to
  Linux ARM64 hosts, although the Linux backend's configuration logic is
  unchanged. Enabling the kernel feature and granting sandbox access remain
  separate steps.
- In the verified environment, `/dev/kvm` is owned by root with mode `0600`.
  Root can use it, but switching a session to the `agent` user does not grant
  access. This change does not adjust device ownership or permissions.

## Limits and remaining checks

Unsupported Macs and older macOS versions were not available for runtime
testing. The KVM API test creates an empty VM; the separate Alpine smoke test
establishes that an inner guest can boot and execute commands.

Before upstreaming, run the new default-off device visibility test on the host.
Additional coverage should include unsupported Macs, older macOS versions,
Linux ARM64 hosts, and sustained nested VM workloads. The successful M5 smoke
test does not establish compatibility or performance across those cases.

The inner smoke test also encountered the existing requirement that project
state and the image cache share a filesystem. A project under `/tmp` failed
with an invalid cross-device hardlink error. A disposable project beside the
image cache succeeded and was removed afterward. This change does not alter
that cache requirement.
