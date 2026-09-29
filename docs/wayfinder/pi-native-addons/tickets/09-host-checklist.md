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

## Prerequisite found while checking (2026-09-29)

The fix is **local only**. The `cang` flake input in `/home/dev/nix/disp` is
`github:zeroqn/cang`, and local `main` is nine commits ahead of
`origin/main` (`9793dc7`), with none of them pushed:

```
927a21b image: assert the pi native addon runtime wiring, and document it
4f950c4 docs(wayfinder): close ticket 04 - the addon failure is confirmed in a real guest
1d8272d pi: wrap the pi binary with a native addon runtime directory
ea2faf7 docs(wayfinder): decide the pi addon C++ runtime delivery (ADR 0009) ...
fd8c0a1 docs(wayfinder): resolve the two pi addon research tickets ...
d1d666b docs(wayfinder): chart the pi native addon library map
5e67f8b cang: bump the version to 0.10.1
d64577e chore: flake lock
eaffa99 pins: update monty to 1.0.0 and pi to 0.87.1
```

So step 1 of the checklist needs a **step 0**: push (or otherwise make the
revision reachable), since `nix flake update cang` fetches from GitHub. Note the
three oldest commits are not from this effort - a version bump to 0.10.1, a flake
lock refresh, and the monty/pi pin bumps the wrapper fix builds on - so the push
publishes those too, which is why it is bob's call rather than an unattended step.
