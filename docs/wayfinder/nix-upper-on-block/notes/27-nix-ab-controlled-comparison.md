---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Blk `/nix` vs virtiofs `/nix`: a controlled A/B, 2.1-2.8x - and what Nix's own overlay store is

## 1. Does Nix have a store overlay? Yes, and it *is* overlayfs

Nix manual, "Experimental Local Overlay Store"
(`store = local-overlay`, feature `local-overlay-store`): the store

> "is a variation of the local store designed to leverage Linux's Overlay Filesystem
> (OverlayFS for short). Just as OverlayFS combines a lower and upper filesystem by treating
> the upper one as a patch against the lower, the local overlay store combines a lower store
> with an upper almost-local store."

and, decisively for the question of avoiding a union layer:

> "To use this store, you will first need to configure an OverlayFS mountpoint appropriately
> as Nix will not do this for you (though it will verify the mountpoint is configured
> correctly)."

So it moves the union's *management* from cang to Nix; the kernel overlayfs mount still has to
exist, with the same upper-filesystem requirement (a fuse upper is refused - note 25). It cannot
remove a cost that measures at zero anyway (note 25: overlay over the block lower 442-472 ms
against the raw lower's 422-559).

## 2. The A/B, same probe, same binary, one boot per mode

`find /nix/store -maxdepth 3 -type f` (3 897 files), guest page cache dropped before every arm,
host image dropped (mincore-proved) before every arm, 6 GiB guest, gates passed
(`MemAvailable` 14.7 GB, `AnonPages` 1.4 GB):

| mode | `/nix` as mounted | arms (ms) |
|---|---|---|
| `--nix=lower:virtiofs --nix=upper:virtiofs` | `virtiofs` (its own mount: `mountinfo … / /nix … - virtiofs`) | 1 199 / 940 / 926 / 913 |
| `--nix=lower:blk --nix=upper:blk` | guest-composed overlay over the block lower (verified in `run-nixovl-` and `run-mounts-`: `stat -f /nix` = `overlayfs` dev=45, lower `btrfs` dev=41, `CANG_NIX_LOWER_BLOCK=1`) | 461 / 432 / 479 / 447 |

**2.1-2.8x for the walk**, and a third corroborating blk boot (`run-nixovl-`) measured
478 / 422 / 431. That is the comparison this map was missing; the earlier cross-boot pair
(1 239 ms vs 430-480) was the same gap measured less carefully.

## 3. The mount identity flaps between boots, and that matters

Same flags, different boots: `run-ab-blk-20261116T132302Z` reports `stat -f /nix` = **fuse**
(no `/nix` mount), while `run-nixovl-20261115T125327Z` and `run-mounts-blk-20261116T133004Z`
report **overlayfs**. Its walk times are the same either way (432-479 against 422-478), so the
*time* conclusion is robust, but the *mechanism* claim "the guest composed the overlay" holds
only in the boots where the mount table says so - and something made the guest fall back in the
others. Candidate: the upper generation's seeding step is per image identity and the fallback
is silent in the console (8 debug lines, no warning), which is itself worth a ticket.

This also retracts an error I made mid-session: I read a *truncated* `grep | head -30` of the
mount table and concluded `/nix` was fuse in a boot where the next lines showed `overlayfs`. The
mount identity must be read from the full `mountinfo`/`stat -f` pair for the boot in question,
never from a filtered excerpt.

## 4. What is still open

- Why the guest fell back in some blk boots (seeding race? stale upper generation?) - needs a
  boot with the full mount table and the guest-init's own `/run/cang/nix-prep.{log,status}` read
  before the arms.
- The 2.1-2.8x gap itself is transport, not overlay: virtiofs `/nix` at 913-1 199 ms against
  the block lower's 422-479, which is what `--nix=lower:blk` + `upper:blk` exists to deliver,
  and what the docs already claim ("the guest's overlay is the only /nix mount, so every /nix
  access is on the block devices instead of a covered virtio-fs path").

## 5. Is there another Nix way to union stores? No

`nix help-stores` on the host's Nix **2.34.9** lists exactly ten store types:

```
Dummy Store | Experimental Local Overlay Store | Experimental SSH Store |
Experimental SSH Store with filesystem mounted | HTTP Binary Cache Store |
Local Binary Cache Store | Local Daemon Store | Local Store | S3 Binary Cache Store | SSH Store
```

The only union among them is the local overlay store, which is overlayfs. The rest are single
stores: `local`, `daemon`, `ssh`, and the caches. A **chroot store** (`--store /path`, or the
`local?root=` setting the `auto` heuristic falls back to as `~/.local/share/nix/root`) is a
*separate* local store with its own database and its own physical contents - populating it means
`nix copy`/substitution, i.e. a full copy, with no copy-on-write and no union. Hardlink dedupe
and `auto-optimise-store` share inodes *within* one store on one filesystem; they do not merge
two stores either.

The structural reason: a store path resolves to exactly one store instance, and Nix has no way
to present two store *directories* as one namespace. `--substituters` gets you fetching, not
unioning. So "let Nix do the union" can only mean its overlay store, which still requires the
kernel overlayfs mount - and that mount measured free here, so there is nothing to save by
moving the union into Nix.

For cang the only open transport question is therefore which side carries the lower:
block (432-479 ms) or virtiofs (913-1 199 ms), which is what `--nix=lower:blk` +
`--nix=upper:blk` decides.

## Evidence

`run-ab-virtiofs-20261116T132010Z` (1 199/940/926/913, `/nix` = virtiofs),
`run-ab-blk-20261116T132302Z` (461/432/479/447), `run-nixovl-20261115T125327Z` (478/422/431,
overlay verified), `run-mounts-blk-20261116T133004Z` (`diag3.txt`: `stat -f /nix` = overlayfs
dev=45, full mount table, env contract, `/run/cang/nix-prep.status`).
Probes `probe-ab.sh`/`probe-nixovl.sh`/`probe-mounts.sh`, runners `run-ab.sh` etc.
