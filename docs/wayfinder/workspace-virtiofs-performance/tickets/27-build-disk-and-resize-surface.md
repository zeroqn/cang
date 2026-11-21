---
label: wayfinder:grilling
title: What happens to cang-build.raw and the `cang disk resize --build` surface once its tenants move
status: open
blocked_by: [18, 24, 26]
claimed_by: "pi + bob (2026-11-21)"
---

## Question

`cang-build.raw` carries three tenants today: `target/`, `CARGO_HOME` and the
`/nix` upper (`nix_lower.rs:88`). The redraw moves the first two into the
workspace image and removes the third, which leaves the build disk with nothing
to hold - and `cang disk resize --build` / `cang disk reset --build` with nothing
to act on.

Decide:

1. **Delete or keep.** Remove the build disk entirely (state migration: what
   happens to the 96-100 GiB images real users have, and to a launch that still
   finds one), or keep it as the home of something that has not been named yet.
2. **What the commands become.** `cang disk resize --build` currently grows the
   host file and then runs a maintenance VM (`runtime/maintenance/disk_resize.rs:124`);
   if the build disk goes, does the command grow the *workspace* image instead,
   or disappear with a named refusal pointing at the workspace surface?
3. **Grow-on-demand.** The stated pain was "manually grow disk"; say plainly
   whether the workspace image grows itself (and under what headroom rule) or is
   still grown by hand - and if it still is, whether that contradicts the redraw's
   "fewer manual grows" claim.
4. **Container store stays** (`--container-store raw-disk`, `cang-containers.raw`):
   confirm it is untouched by this, since "fewer blk" is a rule about *new* disks,
   not a licence to remove a shipped one.

## Round answers (2026-11-21, bob)

- **1 - retire the build disk AND delete an existing one automatically.** Not the
  gentle option: `cang-build.raw` stops being created and attached, and a launch
  that finds one reclaims its space. Safety limits this ticket already fixes:
  the deletion is logged with the reclaimed bytes, happens only when nothing else
  holds the image (no attachment, no lock), and `cang disk reset --build` stays as
  the explicit command. **What this costs a user**: the warm `target/` and
  `CARGO_HOME` in that file are gone - so the deletion message says so, and says
  what replaces them (the workspace image's own tree).
- **2 - `cang disk resize --workspace-disk PATH`**, and `--build` becomes a named
  refusal pointing here. `--containers` stays.
- **3 - the workspace image grows itself by default**: before a launch or serve
  mounts it, grow it when the free space recorded by the previous run was under
  ~10 % and the host keeps >= 2x headroom; `--workspace-size` stays the creation
  size; a named warning when the cap or the host headroom refuses.
- **4 - generations of the `/nix` upper stay where ticket 26 put them**; nothing to
  do now (they are subvolumes, hardlinked from the image, and `cang nix gc`
  collects stale ones).

## Resolution

(to follow)
