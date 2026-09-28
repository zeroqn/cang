---
label: wayfinder:task
title: Land the kernel config and patch in deps/libkrunfw and boot-test it locally
status: open
blocked_by: ["02-kernel-patch-set", "04-carried-design"]
---

## Question

Turn ticket 02's patch set into a working `deps/libkrunfw`:

1. Add `CONFIG_UDMABUF=y` and the virtio-gpu guest-side patches (numbered after
   `patches/0036`), keeping the feature bits from ticket 04 and never renumbering
   `VIRTIO_GPU_F_FENCE_PASSING = 5`.
2. Regenerate the derived x86_64 seed configs (`-lto`, `-kvm`, `-kvm-lto`) from
   the plain seed, since they are seed + explicit toggles.
3. **Build and boot-test locally before any tag** (bob's condition): every
   shipped variant builds, a cang guest boots on the locally built firmware,
   `uname -r` is still `6.12.109-hardened1`, and the new kernel-side checks
   (`/dev/udmabuf` present, `VIRTGPU_PARAM` 10 answering, the blob flag accepted)
   can be probed from inside the guest. IKCONFIG is off, so verify behaviour in
   the booted guest rather than `/proc/config.gz`.
4. Note what could not be verified locally, and the per-arch story ticket 06 /
   the map's *Not yet specified* needs.

## Deliverable

The `deps/libkrunfw` commits, `notes/05-libkrunfw-kernel-support.md` with the
build/boot evidence, and the exact commands ticket 06 re-runs.
