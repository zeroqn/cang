---
label: wayfinder:task
title: Document the mechanism and the hardened-allocator stopgap
status: closed
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: pi session (2026-09-29)
---

## Question

Write down what a user of the cang image needs to know, in the places that
already carry this kind of fact:

- `README.md` "Container environment summary" lists the allocator strategy and
  the image's library story (`/etc/ld-nix.so.preload`, `--alloc`); the native
  addon runtime directory and the `pi` wrapper belong there.
- The stopgap bob chose to document: with the default mimalloc allocator, pi
  extensions carrying glibc native addons (magic-context's `sharp` and
  `onnxruntime-node`) failed with `libstdc++.so.6: cannot open shared object
  file`, and `cang --alloc=hardened` unblocked them because that allocator's
  library carries libstdc++ itself. Say plainly that this is incidental and that
  the wrapper is the fix, so nobody concludes the allocator is the mechanism.
- If a whole topic page fits better than a README line, `docs/diagnostics.md`
  (failure modes) or `docs/images-and-storage.md` (allocator/library story) is
  the alternative.

## Resolution (2026-09-29)

Two edits, both small:

- `README.md`, "Container environment summary": a bullet for the `pi` wrapper and
  the cang-owned runtime directory holding only `libstdc++.so.6`, saying why it
  exists (prebuilt extension addons open a C++ runtime at load time that Pi's
  bundled Bun cannot supply).
- `docs/diagnostics.md`, "Troubleshooting FAQ": the failure mode
  (`libstdc++.so.6: cannot open shared object file`), the reason the guest cannot
  supply it (Bun has no RUNPATH and no libstdc++ in `DT_NEEDED`; `/lib` and
  `/usr/lib` are compatibility farms off the loader's default path; the default
  `mimalloc` preload does not link it), the wrapper as the fix, and the stopgap
  labelled as such - on an image predating the wrapper, `cang --alloc=hardened`
  works because that allocator library carries `libstdc++` in its own `DT_NEEDED`,
  which is incidental rather than a fix, since `--alloc=glibc` removes the same
  preload file and fails identically. The entry also records the `node` trap: node
  links `libstdc++` itself and loads the addon even when the guest cannot, so a
  `node`-based check reports a healthy guest.
