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
    different build whose NEEDED set has no libstdc++. **Confirmed** by ticket 01 on the
host's loader condition and by ticket 04 inside a real guest (with the sharp/
onnxruntime failure verbatim under `bun`, and `node` passing as a trap).
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

- [What resolves a pi extension's glibc addon libraries today, in a cang guest and on the dev host?](tickets/01-glibc-addon-resolution-today.md):
  nothing in the guest supplies `libstdc++.so.6`, and that soname is the *only*
  missing piece - `pi`/`bun` have no RUNPATH and no libstdc++ in NEEDED, the host
  works solely because NixOS's preloaded malloc provider NEEDs libstdc++ itself,
  and the guest's `pkgs.mimalloc` does not. Reproduced with `bun` + mimalloc under
  the guest's loader condition; `LD_LIBRARY_PATH` or a preload carrying libstdc++
  both fix it.
- [Can the image make extra shared libraries loader-visible without an inherited environment variable?](tickets/02-loader-visible-compat-libs.md):
  the loader knows `/etc/ld-nix.so.preload` and `/etc/ld.so.cache`, there is no
  default search path to extend (no `/lib`, no `/usr/lib`, only the loader's own
  store `lib`), and the image already contains gcc's `libstdc++.so.6`. Env-free
  `/etc` hooks are plausible but unprobed; the allocator's preload file is a trap
  because `--alloc=glibc` deletes it.
- [Which mechanism carries the C++ runtime to pi's addons, and where does it live?](tickets/03-mechanism-and-placement.md):
  a **`pi` wrapper in `nix/pkgs/pi-coding-agent.nix`** prepends a native addon
  runtime directory holding **only `libstdc++.so.6`** to `LD_LIBRARY_PATH`, so the
  guest and the host get it from one change and everything the agent launches is
  covered. `--alloc=hardened` is documented as a stopgap, not promoted to the
  mechanism; evidence is a scored live-guest probe plus a repo wiring check.
  Recorded as [ADR 0009](../../adr/0009-pi-extension-cxx-runtime-delivery.md).
- [Install the pi wrapper and the native addon runtime directory](tickets/05-wrapper-and-runtime-dir.md):
  landed - `cang-native-addon-runtime` exposes only `libstdc++.so.6`, and
  `bin/pi` is now a `makeWrapper` script prepending it to `LD_LIBRARY_PATH`.
  Under the guest's masked-`/etc` condition both addons go from LOAD_FAIL to
  LOAD_OK with that directory alone; image build and wrapper contracts are green.
- [Reproduce the addon failure inside a real cang guest before the mechanism is chosen](tickets/04-live-guest-failure.md):
  confirmed on the target (digest `sha256:b821b52e…`) - `bun` fails both addons
  with `libstdc++.so.6: cannot open shared object file` under the default
  mimalloc allocator and loads both under `--alloc=hardened`, while the `node`
  control passes; `LD_DEBUG` shows the loader's system path never reaching the
  image's `gcc-15.3.0-lib/lib`, and a preloaded object's RUNPATH is confirmed not
  to rescue a later `dlopen`.

## Not yet specified

- **The residual coverage gap** - the wrapper reaches pi's process tree; a
  dynamic process started from the guest task shell outside it (a `node` the
  user runs directly, a nested container) is not covered. That is accepted for
  now; it graduates into a ticket only if someone actually hits it.
- **Future addons with other sonames** - the runtime directory holds
  `libstdc++.so.6` because that is what `sharp` and `onnxruntime-node` lack.
  `libxcb.so.1` (the pi-tui X11 prebuild) has the same shape of dependency and
  will need its own decision when something loads it in the guest.
- **Whether magic-context degrades silently today** - ticket 01 could not
  establish it (the generated bundle's require/import sites are not greppable);
  it matters for how loudly the fix has to be proven.
- **Per-arch coverage** - `@img/sharp-linux-arm64`, onnxruntime `linux/arm64`,
  and whether the image is built for aarch64 at all.

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
