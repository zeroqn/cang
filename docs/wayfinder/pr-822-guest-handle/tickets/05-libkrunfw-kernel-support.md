---
label: wayfinder:task
title: Land the kernel config and patch in deps/libkrunfw and boot-test it locally
status: closed
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

## Resolution (2026-09-29, pi)

**Landed and boot-tested.** `deps/libkrunfw` `3fdbb59` carries the three carried
patches (`0037` Vivek's PRIME-import base, `0038` Val's six `guest-handle`
commits with the `virtgpu_plane.c`/uapi/`kms.c`/bit-table hunks re-authored,
`0039` the fork-authored conditional PRIME-import unlock plus the
`CREATE_GUEST_HANDLE`-requires-`BLOB_CTX_ID_FIX` gate), and all six configs have
`CONFIG_UDMABUF=y`; the cang submodule pointer moved in `f62b3ca`. All 39 patches
plus `linux-hardened v6.12.109-hardened1` still apply cleanly to a pristine
`linux-6.12.109` and reproduce the authored tree.

The evidence, the exact probe and what could not be verified are in
[`notes/05-libkrunfw-kernel-support.md`](../notes/05-libkrunfw-kernel-support.md).
In short, a cang guest on the locally built firmware (both the plain
`x86_64-kvm` Nix build and the shipped `libkrunfw-x86_64-kvm-lto.tgz`) reports
`uname -r` = `6.12.109-hardened1`, a guest `/dev/udmabuf`, and

- without the gate: `VIRTGPU_PARAM_CREATE_GUEST_HANDLE` = **0**, negotiated bits
  `0-4 + VERSION_1`;
- with `--zero-copy-shm`: the same param = **1** and bits **6**
  (`CREATE_GUEST_HANDLE`) and **7** (`BLOB_CTX_ID_FIX`) additionally set.

So the host's feature advertisement and the guest driver's acceptance are both
demonstrated, and the gate really is one decision: it withholds the bit, the
param and the file-backed RAM together.

One host prerequisite came out of it, recorded in the note and the README:
`--zero-copy-shm` needs the host's `/dev/udmabuf` openable **from inside cang's
keep-id user namespace** (this host had it `0660 root:kvm`, whose gid is
unmapped there, so the probe failed until it was made `0666` like `/dev/kvm`).

Not shown here: an actual `BLOB_FLAG_CREATE_GUEST_HANDLE` (0x8) blob - that is
ticket 09's client - and any arch other than x86_64 (ticket 06/CI).
