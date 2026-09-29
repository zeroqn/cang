---
label: wayfinder:research
title: Can the image make extra shared libraries loader-visible without an inherited environment variable?
status: open
blocked_by: []
claimed_by: pi research child-2 (2026-09-29)
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
