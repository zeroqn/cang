---
label: wayfinder:grilling
title: What happens to cang-build.raw and the `cang disk resize --build` surface once its tenants move
status: closed
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

Shipped 2026-11-21, commit `6c2680d5`, verified live
([note 68](../notes/68-build-disk-retired-and-auto-grow.md)).

1. **Retired.** No launch creates or attaches `cang-build.raw`;
   `CARGO_TARGET_DIR`/`CARGO_HOME` are no longer exported (`target/` is cargo's own
   `./target` in the tree, the cargo home is the host's `~/.cargo` mount). An
   existing build disk is **reclaimed at launch**, once, under an exclusive
   `flock`, never outside the state root, with one line naming what it held and
   where that lives now. Measured on a planted 96 GiB disk:
   `cang: reclaimed 96.0 GiB from '…/cang-build.raw' …`.
2. **The command moved.** `--build` parses and refuses by name pointing at
   `cang disk resize --workspace-disk PATH SIZE`; `--containers` unchanged
   (verified growing 64 GiB -> 65 GiB, and the grow-only refusal before that).
3. **Grow-on-demand is real, on `workspace serve`.** The guest records
   `cang-workspace-free-bytes<TAB>free<TAB>total` in `/workspace/.cang/free-bytes`
   on every mount; a serve reads it through one short read-only maintenance VM
   before mounting and grows when free < 10 % with 2x host headroom. Measured: 2 GiB
   -> 20 GiB, guest reporting `20G`; the headroom refusal prints both numbers, the
   step and the manual command; a marker above 10 % does nothing. A one-shot
   `--workspace-disk` launch does not auto-grow (a VM boot per launch is not worth
   it) - the refusal line names the manual command for that case.
4. **The container store is untouched** (`--containers` resizes as before).
5. `/nix` upper generations stay where ticket 26 put them; `cang nix gc` collects
   stale ones.
