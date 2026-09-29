---
label: wayfinder:task
title: Document the mechanism and the hardened-allocator stopgap
status: open
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: ""
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
