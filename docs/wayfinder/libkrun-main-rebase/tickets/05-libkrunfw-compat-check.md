---
label: wayfinder:research
title: Does the pinned libkrunfw boot libkrun 2.0.0?
status: open
blocked_by: ["09-port-cang-to-v2-api"]
claimed_by: unclaimed
---

## Question

**The ABI half is answered by ticket 01** (`notes/01-upstream-main-delta.md`,
section 4): main's firmware contract is unchanged - `krunfw_get_kernel(u64*,
u64*, usize*) -> *mut c_char`, soname `libkrunfw.so.5`, `KernelBundle` and
`DEFAULT_KERNEL_CMDLINE` byte-identical, virtio-mmio + MP tables still the
default (ACPI opt-in), no new `ACCESS_PLATFORM` or packed-ring requirement. So
the pinned `libkrunfwRelease.tag = v5.6.2-cang.1` (kernel 6.12.109-hardened1)
should still be the right fw.

What remains is the **live leg**, which cannot run until cang boots again
(ticket 09):

1. Boot a guest on the rebased libkrun + the pinned fw and confirm it reaches
   userspace: the kernel unpacks, the fw hand-off works, and the guest's PID 1
   is the injected init blob (ticket 09's `krun_init_config_apply_in`).
2. Confirm the guest's own assumptions still hold: `uname -r` is 6.12.109-hardened1,
   the console/status path works (`cang-guest-init` writes its status), and the
   virtio devices cang configures (block, net-unixstream, vsock, console, gpu)
   all appear.
3. Say explicitly what was *not* verified if any leg cannot be run here.

## Deliverable

`notes/05-libkrunfw-compat.md`: the live-boot evidence (kernel version, device
list, PID 1, guest status output) and a verdict - *pinned fw is fine* or *needs a
fw rebase* (which redraws the destination and is its own effort).
