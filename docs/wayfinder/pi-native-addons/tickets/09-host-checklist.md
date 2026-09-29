---
label: wayfinder:task
title: Host checklist - bump the cang input in /home/dev/nix/disp and prove the addons load there
status: open
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: ""
---

## Question

Bob's half of the destination, as a precise checklist rather than agent work:

1. Update the `cang` flake input in `/home/dev/nix/disp` (`nix flake update cang`
   in that flake, or bump to the cang revision carrying ticket 05) and rebuild:
   pi comes from `unstable-inputs.cang.packages.<system>.pi-coding-agent`.
2. Confirm `command -v pi` resolves to the wrapped `$out/bin/pi` and that the
   wrapper exports the native addon runtime directory in `LD_LIBRARY_PATH`.
3. Prove it the honest way: the host masks this failure today because NixOS's
   malloc provider carries libstdc++ (`../notes/01-glibc-addon-resolution.md`),
   so the proof must reproduce the guest's loader condition - run `bun -e
   "require('onnxruntime-node')"` and `require('sharp')` under
   `bwrap --dev-bind / / --bind <empty-dir> /etc` (which removes the NixOS
   preload file) and require LOAD_OK for both.
4. Record the store path of the wrapper as evidence in this ticket.

Resolved when step 3 shows both addons loading without the malloc provider's
help.
