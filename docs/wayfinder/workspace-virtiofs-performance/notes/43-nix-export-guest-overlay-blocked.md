---
label: wayfinder:research
title: A guest-side merge of the /nix layers is blocked by the guest kernel, not by cang
status: decided
blocked_by: []
claimed_by: pi (2026-11-04)
---

# 43 - Exporting the two `/nix` layers and merging them in the guest: measured, blocked

Requirement this was built for (session `01a19f10`, final turn): the store must be two
layers - the image's packages from the *new* cang image as the lower layer, and the
workspace's nix cache as the upper - so a new image adds packages without copying the
cache and without touching what the guest has built.

The design under test: cang stops mounting its host overlay, exports
`lowerdir` (image snapshot store, read-only) and the *parent* of `upperdir`/`workdir`
(read-write) as two virtio-fs devices, and the guest runs

```
mount -t overlay overlay   -o lowerdir=/run/cang/nix/lower,upperdir=/run/cang/nix/state/upper,workdir=/run/cang/nix/state/work,userxattr   /nix
```

It was implemented end to end (tags `/dev/nix-lower`, `/dev/nix-upper`, host `NixExport`
config + codec, second/third device registration, a guest-init step before the first
store reader, host overlay mount removed because overlayfs gives its upper directory to
exactly one mount) and the guest reached the overlay mount with both layer shares
mounted read-only and read-write as intended.

**The guest kernel refuses a FUSE mount as overlayfs upper.** Measured 2026-11-04 in a
`--root --seccomp off` guest built from that tree, `dmesg`:

```
overlayfs: failed to set xattr on upper
overlayfs: ...falling back to redirect_dir=nofollow.
overlayfs: ...falling back to uuid=null.
overlayfs: try mounting with 'userxattr' option
overlayfs: upper fs missing required features.
overlayfs: upper fs does not support tmpfile.
overlayfs: upper fs does not support RENAME_WHITEOUT.
```

and `mount -t overlay` answers `wrong fs type, bad option, bad superblock on overlay`
(ENODEV) for the option string above and for the same string without `userxattr`. The
control arms from the same guest settle where the limit is:

| upper | lower | result |
|---|---|---|
| tmpfs (`/tmp/u/...`) | tmpfs | **mounts** (`RC=0`) - overlayfs itself works in this guest |
| a directory on the root virtio-fs share | tmpfs | ENODEV, same dmesg block |
| same, with `userxattr` | tmpfs | ENODEV, same dmesg block |

So it is not the layering, the option string or the caller: an overlayfs *upper* on
virtio-fs needs three things this pairing does not have - writable `trusted.overlay.*`/
`user.overlay.*` xattrs at mount time, `O_TMPFILE` (the fork's passthrough does not
implement it, so `overlayfs: upper fs does not support tmpfile`), and `RENAME_WHITEOUT`
bookkeeping (the fork forwards `renameat2` flags but has no whiteout support). Those are
fork components, not cang plumbing - the same conclusion note 42 §7 reached as an
unknown, now measured.

Two side findings, both worth keeping:

1. **A read-write export of the merged tree cannot replace the graft either.** With the
   host overlay still mounted and cang exporting its `mergeddir` as a second device,
   guest writes into `/nix` came back `EACCES` (`CREATE` opcode 35 -> `-13`) while the
   same writes on the root share succeeded, the same guest-visible `1000:993 /nix`,
   `0:0 /nix/var` in both arms. The merged directory's ownership reaches the server as
   the worker's namespace root (`--map-users 0:165536:1000 --map-users 1000:1000:1`), and
   the second server's request path does not reach the same credentials as the root
   share's. Not pursued further: with the guest-merge route blocked, the export has no
   job that the graft does not already do.
2. **The host-side shape already satisfies the two-layer requirement.** `prepare_upper`
   keeps the workspace upper across image moves, `seed_from_previous` clones it into the
   new generation, parks the old store DB for the guest and installs the new image's DB,
   and `lowerdir` is the new image's snapshot (`crates/cang/src/runtime/session/nix_overlay.rs`).
   A new cang image therefore adds its packages (new lower) while the cache the guest
   filled stays (cloned upper), which is exactly the requirement - paid for with a
   one-time reflink clone (0 bytes, ~10 s) when the image changes, exactly as the earlier
   session's recommendation described.

**Decision:** the two-layer requirement is met by the shipped host overlay; the export
work is reverted. Reopening it means implementing `O_TMPFILE` + whiteout xattr/rename
support in the fork's passthrough (or the server-side union of note 42 §7), and only then
does the guest-side merge become possible.
