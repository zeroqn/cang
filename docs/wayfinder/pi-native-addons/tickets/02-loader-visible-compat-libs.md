---
label: wayfinder:research
title: Can the image make extra shared libraries loader-visible without an inherited environment variable?
status: closed
blocked_by: []
claimed_by: pi research child-2 (2026-09-29); completed by the charting session after the child was killed by the host disk filling
---

## Question

Ticket 03 has to choose between an `LD_LIBRARY_PATH` wrapper and something the
loader sees without any environment change. Establish which env-free mechanisms
actually exist in this environment, per mechanism with primary evidence.

1. **`/etc/ld.so.cache`.** Does the nixpkgs glibc dynamic loader - the one in the
   image and on the host - consult `/etc/ld.so.cache` at all? What is the exact
   search order it uses there (`LD_LIBRARY_PATH`, `DT_RUNPATH` of the object and
   of the loading scope, cache, default dirs), and is the cache path compiled in
   or disabled by nixpkgs? If it is consulted, can a layered image ship one (via
   `ldconfig`, or a hand-built cache) and would it be found inside a cang guest?
2. **`/etc/ld.so.preload`.** Is that a real glibc hook (as opposed to the
   NixOS-specific `/etc/ld-nix.so.preload`), and would listing a library there
   work in the guest? What are the failure modes when the file is present and a
   listed library is missing?
3. **RUNPATH rewriting.** Could the image or the pi derivation make the addons'
   own dependencies resolve without touching the addon files (e.g. a directory
   listed in the loader's default path, an FHS `/lib64` layout, or a symlinked
   "compat lib dir" placed where the loader already looks)? Which of these
   actually reach a *dlopen'd* object's NEEDED entries rather than only an
   executable's?
4. **Guest-rootfs reality.** The task rootfs is a prepared root with the host
   `/nix` as a read-only overlay lower plus an upper (`docs/internals.md`,
   `docs/images-and-storage.md`). Which `/etc` entries survive into a task, and
   which does guest-init write or rewrite (it writes `/etc/ld-nix.so.preload`
   per `--alloc`)? A mechanism that depends on an image `/etc` file must survive
   that.
5. **Upstream expectation.** What do sharp and onnxruntime-node expect from the
   host system (do they assume a distro `/usr/lib` with libstdc++ present, or do
   they carry their own)? This says how far from "normal" this environment is.

## Deliverable

`../notes/02-loader-visible-compat-libs.md`: a verdict per mechanism - works /
does not work in a cang guest, with the command or source that establishes it -
plus a shortlist ticket 03 can weigh. Raw command output beside it as
`02-raw-*.txt`.

## Constraints for whoever resolves this

- Read-only with respect to the repository except the notes file above; do not
  edit the map or other tickets.
- Cheap experiments first (a host-side `LD_DEBUG`/`ldd`/cache probe, an image
  filesystem inspection); a live guest only if one can be booted cheaply, using
  the house recipe (isolated config/state, hermetic container storage, never the
  ambient `~/.config/containers/storage.conf`).
- Do not modify the host system (`/etc`, NixOS configuration, nix-ld).

## Resolution (2026-09-29)

**Answer: the loader knows `/etc/ld-nix.so.preload` and `/etc/ld.so.cache` (both
literal paths are compiled into the nixpkgs glibc loader, verified with `grep -a`;
`strings` is not installed here), there is no `/lib`, `/usr/lib` or earlier
search path to extend, and `/lib64/ld-linux-x86-64.so.2` is a NixOS-only nix-ld
shim that does not exist in the image.** Full mechanism table with verdicts:
`../notes/02-loader-visible-compat-libs.md`.

Measured facts:

- `LD_LIBRARY_PATH=<gcc-15.3.0-lib/lib>` makes both addons load under the guest
  condition - the env mechanism works.
- `LD_PRELOAD` of a library that itself NEEDs libstdc++ also works (that is the
  host's accidental mechanism, and the guest's `--alloc=hardened`); but the guest
  file that carries that preload (`/etc/ld-nix.so.preload`) is the **allocator's**
  file - `--alloc=glibc` deletes it, so extending it is a correctness trap rather
  than a mechanism.
- The only default search path is the loader's own read-only store `lib` dir, so
  an FHS-style compat directory on the default path is ruled out.
- An env-free `/etc` hook is plausible but unprobed: `/etc/ld.so.preload` (a real
  glibc feature here) or a shipped `/etc/ld.so.cache` over a compat directory
  built at image time. Nothing in the image generates a cache today.
- RUNPATH rewriting of the addons works in principle but the addon tree is
  installed at runtime under `~/.pi`, so it means mutating user state at start.
- No new library content is needed: `gcc` is in the image
  (`cToolchainImagePackages`), so `libstdc++.so.6` already exists in the guest's
  store.

**Shortlist handed to the mechanism ticket:** (1) `LD_LIBRARY_PATH` from a `pi`
wrapper in `nix/pkgs/pi-coding-agent.nix` (reaches host and guest, proven);
(2) the same path in the image Env (guest-wide, not the host); (3) an env-free
`/etc` hook at image build (unprobed); (4) extending the allocator's preload file
(ruled out above).
