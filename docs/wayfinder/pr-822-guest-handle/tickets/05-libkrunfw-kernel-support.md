---
label: wayfinder:task
title: Land the kernel config and patch in deps/libkrunfw and boot-test it locally
status: open
blocked_by: ["02-kernel-patch-set", "04-carried-design", "11-kernel-base"]
---

## Question

Turn ticket 02's patch set into a working `deps/libkrunfw`:

1. Add `CONFIG_UDMABUF=y` (all six configs) and the guest-side patches from
   ticket 02, numbered after `patches/0036`: (a) Vivek's `[PATCH v5 0/5]`
   PRIME-import-as-guest-blob base (re-author the `virtgpu_plane.c` hunk), then
   (b) Val's six `guest-handle` commits (`a412cc915ec7`, `828df4d292f2`,
   `2a41d37ca1d0`, `953b78600d13`, `1c148608fe3a`, `3e6a365d2ac9`, which are
   `../notes/02-raw-val-0{1..6}-*.patch`), re-authoring the
   `virtio_gpu.h` bit-table, `uapi` param-10 and `kms.c`/`drv.h` hunks, and
   (c) the **fork-authored conditional PRIME-import unlock** the revert left
   behind. Keep `5 = FENCE_PASSING`, add `6`/`7`, never import upstream's
   `BLOB_ALIGNMENT = 5`.
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
