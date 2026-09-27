---
label: wayfinder:research
title: Does the pinned libkrunfw boot libkrun 2.0.0?
status: open
blocked_by: []
claimed_by: unclaimed
---

## Question

This effort rebases libkrun to `main` but **not** libkrunfw (out of scope). The
pin is `libkrunfwRelease.tag = v5.6.2-cang.1` (kernel 6.12.109-hardened1), and
upstream `main` crossed ~369 commits, including virtio, boot/init and display
work.

Determine what `main`'s libkrun *expects of its kernel/firmware side* and
whether our pinned libkrunfw still satisfies it:

1. `libkrun_init.so` / `KRUN_INIT_FULL_VERSION` and any init protocol or blob
   contract between the VMM and the firmware, and whether main changed it.
2. The kernel command line and boot contract (`krun` builds the cmdline from the
   context config) - did main add or require any new kernel parameter, ACPI or
   device that 6.12.109 provides?
3. Virtio device/feature expectations that need kernel support the pinned
   config may lack (this repo's fork config deltas: landlock, nftables TPROXY,
   zram LZO, PSI, virtio-pci, nested-virt host KVM).
4. Whether upstream publishes a known-good libkrun/libkrunfw pairing statement
   (release notes, CI matrix) that pins the answer.

## Deliverable

`notes/05-libkrunfw-compat.md`: verdict - *boots on the pinned fw*, *needs a fw
rebase* (which redraws the destination and is its own effort), or *unknown until
booted* (say which probe would settle it). Cite upstream files at the frozen
SHA.
