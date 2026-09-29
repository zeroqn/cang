---
label: wayfinder:research
title: What resolves a pi extension's glibc addon libraries today, in a cang guest and on the dev host?
status: open
blocked_by: []
claimed_by: pi research child-1 (2026-09-29)
---

## Question

Pin down the failure and the resolution chain for glibc-linked pi extension
addons, so ticket 03 can choose a mechanism against facts instead of theory. The
two known consumers are magic-context 0.44.1's
`@img/sharp-linux-x64/lib/sharp-linux-x64-0.35.5.node` and
`onnxruntime-node/bin/napi-v6/linux/x64/onnxruntime_binding.node`, both under
`~/.pi/agent/git/github.com/zeroqn/pi/node_modules`.

1. **What actually happens in a cang guest?** Import both addons in a real guest
   on the current tree and record the outcome verbatim (success, or the loader
   error naming the missing soname). If a guest cannot be booted, say so and
   report the strongest evidence obtainable statically - do not guess.
2. **What is the resolution chain in each environment?** The host's is known and
   accidental: `libstdc++.so.6` resolves through the RUNPATH of the
   *preloaded* `libhardened_malloc.so` from `/etc/ld-nix.so.preload`. Establish
   the guest's chain the same way (`LD_DEBUG=libs`, `ldd`, `readelf`/a small ELF
   reader if binutils is absent): what is on the loader's search path with the
   real `/etc/ld-nix.so.preload` (mimalloc by default, per `--alloc`), and does
   the preloaded mimalloc's RUNPATH contain `gcc-*-lib/lib`?
3. **Minimal library set.** DT_NEEDED of each addon and of its own deps
   (`libvips-cpp.so.8.18.7`, `libonnxruntime.so.1`): which resolve inside the npm
   tree via RUNPATH, which must come from the system, and which are satisfied by
   libc/libm/libdl/libpthread already.
4. **How does pi load an extension addon?** Read the pi 0.87.1 sources (the
   `pins.piCodingAgent` rev; the host has 0.85.1 of the same derivation) and say
   whether extension packages are imported in the pi process, `process.dlopen`ed,
   or loaded in a child. This decides whether an environment variable set for
   `pi` reaches the load at all.
5. **Symptom.** magic-context declares `sharp` and `onnxruntime-node` as
   *optionalDependencies*: does an addon that fails to load degrade quietly
   (embeddings/image handling fall back) or abort the extension? Say what a user
   would actually notice.
6. **Survey the rest of the tree.** Which other addons in the guest's `~/.pi`
   (`@mariozechner/clipboard-linux-x64-gnu`, the `@earendil-works/pi-tui`
   prebuilds, `@pydantic/monty-linux-x64-gnu`) already load native code, and which
   of them need libstdc++/libgcc_s at all?

## Deliverable

`../notes/01-glibc-addon-resolution.md` plus raw evidence files beside it
(`01-raw-*.txt` for command output). Every claim with the command that produced
it. If a live guest ran, the commands, the guest identity, and the verbatim
outcome.

## Constraints for whoever resolves this

- Read-only with respect to `~/.pi` and the repository except the notes file
  above; do not edit the map or other tickets.
- For a live guest use the house recipe: isolated `XDG_CONFIG_HOME`/state and a
  hermetic container storage directory, **never** the ambient
  `~/.config/containers/storage.conf`; see
  `tools/chromium-cang-smoke/chromium-smoke.sh` and the cang live-VM CONFIG_VALUES
  (state/storage root on the btrfs disk).
- Building the container image can be expensive; time-box it and report what you
  learned without it rather than stalling.
