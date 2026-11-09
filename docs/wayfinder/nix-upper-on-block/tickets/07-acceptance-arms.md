---
label: wayfinder:task
title: The acceptance arms - prove upper:blk end to end in live guests
status: open
blocked_by: ["01-upper-write-cost", "02-overlayfs-btrfs-upper", "05-seeding-parity"]
claimed_by: ""
---

## Question

Ship `upper:blk` only against live evidence, not against a green `cargo test`.
This ticket builds and runs the acceptance arms once the shape is settled, and
records what passed with raw log paths.

Extend `tools/nix-generation-smoke` (its README already asserts the right things)
rather than write a new harness, and add the arms only the new shape needs:

1. **The generation contract, on the disk.** Generation is still a btrfs
   subvolume (inode 256) inside `cang-build.raw`; a store closure imports,
   `verify_bad=0`, the imported binary runs; an image move keeps the previous
   generation's closure registered and `cang-nix-store-repair --dry-run` reports
   no drift; a second boot is a no-op. That is the existing three-boot shape with
   `--nix=upper:blk`.
2. **The write path really is the disk.** Prove the overlay's upper is on the
   block filesystem and not silently on virtio-fs: `findmnt`/`/proc/self/mountinfo`
   of `/nix`, the upper's `st_dev` against the build disk's, and a write whose
   bytes land on `/dev/vdX` (a `strace`/`/proc/diskstats` delta, or the absence
   of FUSE traffic in the guest's request counters).
3. **The inode-identity hazard.** Note 02 places this precisely: the wrong-object
   lookup happens when the two layers are two **subvolumes of one btrfs** (one
   superblock, so overlayfs trusts inode uniqueness and ignores `xino`), not when
   they are two independent filesystems. Assert the *placement*, not a mount
   option: the upper's filesystem is not the lower's, `st_dev` differs between a
   lower-only path and an upper-only path, and the upper and `work` are on one
   mount with the store tree inside a single subvolume. Then assert the store's
   verification (`verify_bad=0`) and a lookup of a path that exists only in the
   lower and one that exists only in the upper.
4. **The metadata number.** `find /nix/store` and `du -s` of the merged `/nix`
   with the upper on the disk, against the map's recorded 206-215 ms / 267 ms -
   the merged walk must not regress the lower's win.
5. **The migration, once.** With a pre-populated host-directory upper, the first
   `upper:blk` launch migrates (the migrate-existing-uppers ticket's shape), a store path that only ever
   existed in the old upper still verifies and runs, and the second launch does
   not copy again.
6. **Failure paths.** No disk space (the headroom rule refuses), a corrupt build
   disk, and a guest image whose `cang-guest-init` predates the contract: each
   must name itself and leave the user on the host-composed overlay, exactly as
   `--nix-lower=auto` does today. The guest-init capability gate keeps that
   honest (`nix_lower.rs:407-430`).

Run every arm delegated, one guest at a time, with fresh-log checks; write
evidence under `/workspace/evidence` and the run report to
`docs/wayfinder/nix-upper-on-block/notes/07-acceptance.md`.

## Resolution

(to follow)
