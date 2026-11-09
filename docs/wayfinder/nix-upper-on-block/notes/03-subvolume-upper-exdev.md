# A btrfs subvolume root cannot be an overlayfs upperdir (measured)

Status: fault found in a live guest on 2026-11-09, in the first block-upper build
of this effort. The fix is the shape below; it is the reason the *generation*
directory carries the subvolume and `upper`/`work` are plain directories inside it.

## Symptom

Every write through the guest-composed overlay failed, and nix prep reported:

    error=failed to replace '/nix/var/nix/db/db.sqlite': Cross-device link (os error 18)

Not a single failing call: `mkdir`, `touch`, `cp` and `unlink` under
`/nix/var/nix/db` all returned `EXDEV` (Invalid cross-device link), while reads
through the same overlay were fine. `dmesg` carried no overlayfs or btrfs warning
at all - the kernel returned the error silently.

The pair itself was correct in every other respect: `/nix` was
`overlay ... lowerdir=/run/cang/nix-lower,upperdir=<build-disk>/nix-overlay/<digest>/upper,
workdir=...,redirect_dir=nofollow,uuid=on,userxattr`, the lower was `/dev/vdc`
read-only btrfs, the build disk was `/dev/vdb` btrfs, and the upper's directory
tree, sentinel and staged image DB were all in place.

## The A/B that isolates it

Same guest, same lower (`/run/cang/nix-lower`), same mount options
(`redirect_dir=nofollow,uuid=on,userxattr`), only the upper differing:

| upper | mkdir | touch | unlink of a lower file |
|---|---|---|---|
| plain directory on the build disk | ok | ok | ok |
| fresh btrfs subvolume on the build disk | **EXDEV** | **EXDEV** | ok |
| plain directory, `trusted.*` xattrs (no `userxattr`) | ok | ok | ok |

So the option set is not the problem, `userxattr` is not the problem, and btrfs as
a filesystem is not the problem: **a btrfs subvolume root cannot serve as an
overlayfs upperdir.** btrfs returns `-EXDEV` for the rename/link operations
overlayfs performs while copying up (`fs/btrfs/inode.c` returns `-EXDEV` when a
rename or link would cross a subvolume boundary, and a subvolume root is
`BTRFS_FIRST_FREE_OBJECTID`).

## The shape that works

The subvolume moves up one level, which is also the shape the host's own seeding
uses (`runtime/session/nix_overlay.rs`: `root` is the subvolume, `upper`/`work`/
`merged` live inside it):

    <build-disk>/nix-overlay/<digest>/            btrfs subvolume - this is what clones
    <build-disk>/nix-overlay/<digest>/upper       plain dir, overlayfs upperdir
    <build-disk>/nix-overlay/<digest>/work        plain dir, overlayfs workdir (empty)

A new generation is then `btrfs subvolume snapshot <old-gen> <new-gen>` - still
constant time and zero bytes - followed by emptying the clone's `work` (overlayfs
refuses a non-empty workdir) and dropping the sibling's staging dirs and sentinel.

## Why the harness did not catch it

`tools/nix-lower-cang-smoke` scored the run **PASS** while nix prep had failed:
its checks read the mount table (the overlay *was* composed) and the store's speed
and integrity (the read path was fine), but never the guest's own verdict. The
smoke now requires `state=ready` in `/run/cang/nix-prep.status` and prints the
prep worker's `error=` line when it is not, and it polls the status file the way
it already polled the mount table.

Evidence: `/home/dev/disp/nixupper-smoke/probe-ws4/evidence4/matrix.txt` (the
A/B), `probe-ws3/evidence3/` (the failing syscalls and the empty kernel log),
`probe-ws2/evidence2/` (the mount and the seeded generation).
