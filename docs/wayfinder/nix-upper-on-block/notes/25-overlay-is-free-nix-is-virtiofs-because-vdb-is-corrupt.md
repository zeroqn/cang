---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Overlayfs is free; `/nix` is 2.4x slower than the lower because it is virtiofs, and the
# reason it is virtiofs is a corrupt build disk (EUCLEAN)

## 1. Overlayfs costs nothing over the block lower

Composed the overlay **in the guest** and walked the same 3 897-file tree:

| root | upper | walk | device reads |
|---|---|---|---|
| `/run/cang/nix-lower/store` (raw) | - | 875 / 422 / 526 / 559 ms | 941 per walk |
| overlay over the lower | **tmpfs** | **442 / 472 ms** | 941 |
| overlay over the lower | build disk | **could not mount** (see 3) | - |

So an overlay over the block lower is indistinguishable from the raw lower (442-472 against
422-559): the union layer is not where any latency lives, and it does not add device traffic
(941 reads either way). Whatever a Nix-side overlay store would save, it is not this.

## 2. What *is* 2.4x slower: `/nix` on virtiofs

Same boot: `find /nix/store -maxdepth 3 -type f` = **1 239 ms** against the lower's 526-559 ms.
`stat -f /nix` is **fuse**, `findmnt /nix` is empty and `/proc/self/mountinfo` has **no line**
for `/nix` - it is a plain directory inside the guest's virtiofs root (`/dev/root / virtiofs`),
i.e. the *host-composed* overlay served over the share, not a guest overlay at all. That
matches note 56's 864-2 033 ms for `find` on virtiofs.

## 3. Why the guest did not compose it: `/dev/vdb` will not mount

```
mount /dev/vdb /mnt/build
mount: /mnt/build: fsconfig() failed: Structure needs cleaning.
```

`blkid` sees it (`/dev/vdb: LABEL="CANG_BUILD" TYPE="btrfs"`), but btrfs refuses it with
EUCLEAN. Every candidate upper for the guest overlay was therefore a **fuse** path
(`stat -f /home/dev/.cache/cang-build` = `fuse`), and overlayfs rejects a fuse upper:

```
mount -t overlay overlay -o lowerdir=/run/cang/nix-lower/store,upperdir=<fuse path>/up,...
mount: /mnt/ovl-vdb: wrong fs type, bad option, bad superblock on overlay, ... rc=32
```

So cang's guest-composed `/nix` is blocked **not by a design limit but by a broken build
disk**: no block-backed writable layer, no overlay, and the host-composed virtiofs overlay
stays. The disk is very likely damaged by this session's repeated `kill -9` of microVMs
(dozens of boots today, several SIGKILLed mid-run), which is the sort of thing `EUCLEAN`
looks like.

## 4. Answering "can Nix do the overlay itself?"

Nix's overlay store (`store = local-overlay://...`, `nix-store --store local-overlay`) is
implemented **with overlayfs** - it moves the union from cang to Nix, not out of the kernel -
and it has the same upper-filesystem requirement, so a fuse upper is refused there too. It
would not remove the layer that this measurement shows costs nothing, and it would still need
a block-backed upper. The lever remains the transport (`/nix` over virtiofs at 1 239 ms against
526-559 ms on the block device), which is what `--nix=lower:blk` exists to fix.

## 5. Next step, and it needs a decision

Rebuild the build disk so the guest can hold an upper, then re-measure `/nix`:
`--nix=upper:blk` should compose an overlay over the block lower and the `/nix` walk should
land near the 442-472 ms measured here rather than 1 239 ms. Rebuilding discards only the
build disk's contents (its `/nix` upper generation, which cang re-seeds from the import
share), but it is a destructive step on a disk this session may have damaged, so it wants an
explicit go-ahead.

## Evidence

`run-ovl-20261116T123851Z` (overlay + tmpfs upper: 442/472 ms, `mount.log` with the fuse-upper
EINVAL), `run-ovl2-20261116T124416Z` (`mount.log`: `mount_vdb rc=32 ... Structure needs
cleaning`, `/nix` walk 1 239 ms, raw 526/559), `run-nixwhat-20261115T233220Z` (the mount table
with no `/nix` and no btrfs entry). Probes `probe-ovl.sh`/`probe-ovl2.sh`, runners
`run-ovl*.sh`, control `control11.sh`.
