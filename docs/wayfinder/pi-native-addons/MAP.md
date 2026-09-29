---
label: wayfinder:map
title: Pi native extension addons load in the guest and on the host
---

## Destination

A pi extension's glibc-linked native addon loads in a cang guest **and** on the
NixOS dev host. First consumers: magic-context 0.44.1's
`@img/sharp-linux-x64/lib/sharp-linux-x64-0.35.5.node` (NEEDED
`libvips-cpp.so.8.18.7`, `libstdc++.so.6`) and
`onnxruntime-node/bin/napi-v6/linux/x64/onnxruntime_binding.node` (NEEDED
`libonnxruntime.so.1`, `libstdc++.so.6`) - then whatever dlopen'd addon comes
next.

The mechanism is **cang-side only** (this repo: the image and/or
`nix/pkgs/pi-coding-agent.nix`), proven by **live evidence from a real guest**
plus a repo invariant that fails if the wiring regresses. The host gets the same
change through the cang flake input in `/home/dev/nix/disp`.

## Notes

- **Domain**: the extension host is the bun-compiled `pi` binary built by
  `nix/pkgs/pi-coding-agent.nix` (`pins.piCodingAgent`, v0.87.1 pinned; this host
  runs 0.85.1 of the same derivation) and installed at
  `$out/lib/pi-coding-agent/pi`. Extension packages are installed **at runtime**
  under `~/.pi/agent/{npm,git}` (magic-context 0.44.1 =
  `pi-magic-context-prebuilt`, generated from `vendor/magic-context` in
  `github.com/zeroqn/pi`), and cang grafts `~/.pi` host -> guest
  (`README.md` "Persistent host mounts"), so one un-patched tree is loaded in
  both places. Image Env comes from `nix/image/config.nix`; the image build and
  its `/etc` payloads from `nix/image/container.nix`; layers and wrappers from
  `nix/image/layers.nix`; the guest allocator preload file
  `/etc/ld-nix.so.preload` is written by the image and rewritten at boot by
  `crates/cang-guest-init/src/guest_init/components/hardening/allocator.rs`
  (`--alloc=mimalloc|hardened|glibc`). The existing convention for prebuilt
  glibc **executables** is `autoPatchelfHook`
  (`nix/pkgs/monty-prebuilt.nix`, `nix/pkgs/beads-prebuilt.nix`) - it patches
  files at image-build time and never sees a runtime-installed addon.
- **Charting decisions (bob, 2026-09-29)**:
  - the destination is **implemented + verified**, not a decision-only map;
  - environments: **the cang guest and the NixOS dev host**;
  - class: **any dlopen'd glibc addon**, with pi's extensions as the first
    consumer rather than the definition;
  - the mechanism is **open** - a wrapper is one candidate, not the answer;
  - scope is **cang only**: a fix that needs upstream pi or `github.com/zeroqn/pi`
    is an out-of-scope handoff note;
  - the host may require a cang flake-input bump + host rebuild in
    `/home/dev/nix/disp`.
- **Starting facts (verified 2026-09-29)**:
  - **On this host both addons already load** (`node -e "require('sharp')"`,
    `require('onnxruntime-node')`). The resolution is *accidental*:
    `LD_DEBUG=libs` shows `libstdc++.so.6` found in
    `/nix/store/2ga5nd1m56n5cx2wh8vbf6nrdhqk2f0q-gcc-15.3.0-lib/lib` through the
    **RUNPATH of the preloaded**
    `/nix/store/ivjm0rhr4lf9ybaj8mr2m8ablr3wpymr-malloc-provider-graphene-hardened/lib/libhardened_malloc.so`
    (`/etc/ld-nix.so.preload`) - not through the executable's own search path and
    not through an `LD_LIBRARY_PATH`.
  - sharp's `libvips-cpp.so.8.18.7` resolves itself: the addon's RUNPATH reaches
    the sibling `@img/sharp-libvips-linux-x64/lib`. The piece nothing supplies is
    the **C++ runtime** (`libstdc++.so.6`, `libgcc_s.so.1`).
  - In the guest nothing on that path supplies them as far as inspection goes:
    the image Env sets **no** `LD_LIBRARY_PATH` (`nix/image/config.nix`), and the
    guest's `/etc/ld-nix.so.preload` holds cang's `pkgs.mimalloc`
    (`nix/image/container.nix:99`; guest-init rewrites it per `--alloc`), a
    different build whose RUNPATH is not the host's. **Unverified** - ticket 01.
  - The nix glibc loader's default "system search path" is its own store `lib`
    (seen via `LD_DEBUG` on the host); there is no `/etc/ld.so.cache`, no `/lib`,
    no `/usr/lib`.
  - The host's pi **is** this repo's derivation: `/home/dev/nix/disp/home.nix`
    takes `unstable-inputs.cang.packages.<system>.pi-coding-agent` from
    `github:zeroqn/cang`.
- **Tracker**: local markdown, this directory. `gh` is unauthenticated in this
  session; bob pushes branches.
- **Skills**: `pi-kernel-sandbox-quirks`, `cang-nix-image-package-wiring` (image
  layers, package wiring, checks), `cang-local-validation-gates`, the cang
  live-VM recipe in CONFIG_VALUES #7/#18 with `cang-state-and-storage-root`, and
  `cang-guest-runtime-troubleshooting`.
- **Evidence precedent**: `tools/chromium-cang-smoke/chromium-smoke.sh` is the
  house pattern for a scored live-guest smoke - hermetic container storage,
  isolated XDG config/state, freshness-checked evidence, exit 0 only on proof.

## Decisions so far

<!-- the index: one line per closed ticket, enough to judge relevance, then zoom the link -->

## Not yet specified

- **Where the invariant lives** - a `wrapperContracts` entry in
  `nix/image/checks.nix`, a repository test, or a scored `tools/` smoke.
  Sharpens with ticket 03.
- **The live evidence's harness** - extend an existing smoke versus a new probe
  that asserts both addons import in a real guest. Sharpens with ticket 03.
- **How far the general class reaches** - bob chose "any dlopen'd glibc addon",
  so whether the mechanism must also cover `node`/`npm` runs the agent launches
  (not only pi's tree) has to be pinned down in ticket 03.
- **The host checklist ticket** - bump the `cang` input in `/home/dev/nix/disp`,
  rebuild, prove the addons load there. Graduates when ticket 03 fixes where the
  mechanism lives.
- **Documentation** - `README.md`'s "Container environment summary" already
  lists allocator and library details, so a mechanism that adds libraries or
  environment needs a line there. Graduates with ticket 03.
- **Per-arch coverage** - `@img/sharp-linux-arm64`, onnxruntime `linux/arm64`,
  and whether the image is built for aarch64 at all.
- **Whether magic-context degrades silently today** (both deps are
  `optionalDependencies`), so the user-visible symptom may be a quiet fallback
  rather than a load error. Folds into ticket 01.

## Out of scope

- **Upstream pi (`earendil-works/pi`) extension-loader changes** - teaching pi to
  preload its own runtime libraries needs a `pins.piCodingAgent` rev bump and a
  fork rebase; bob scoped this map to cang.
- **Changing `github.com/zeroqn/pi`** - an in-process `dlopen` preload of
  libstdc++ before importing the addon, or patching addons after install, lands
  in the extension repo. Handoff note, not a ticket here.
- **NixOS-side loader configuration** - nix-ld, the malloc provider whose RUNPATH
  accidentally fixes the host today, and `/etc/ld-nix.so.preload` providers. The
  host is fixed by the cang-side mechanism; the system stays as it is.
- **The `autoPatchelfHook` convention for packaged prebuilt executables** - it
  patches files at image-build time (monty, beads) and already works; it is a
  precedent to cite, not a thing to change.
