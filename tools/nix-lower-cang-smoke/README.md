# /nix Mode Cang Live Smoke

A live smoke for **the two `/nix` modes a launch can get**: that a
`--workspace-disk` launch serves `/nix` from the read-only btrfs image on
virtio-blk with the guest composing the overlay over it, that cold reads lose no
page, and that a launch with no workspace disk keeps the host-composed overlay
over virtio-fs and is not silently the same slow path.

## The failures this exists for

Two, both quiet.

1. **Lost pages.** A virtio-blk request whose buffer spanned two guest-memory
   regions used to be rejected whole by libkrun (the guest-memory map has a
   region boundary at the end of the kernel image, inside RAM the guest is free
   to use), and the request completed with no data and no status - which the
   guest's virtio-blk driver scored as success. btrfs then failed the checksum of
   pages that were never written, zeroed them and returned `EIO`, or `SIGBUS` for
   a mapping over them, which is how a cold chromium used to die in the
   presenting arm. The 7.x guest kernel only made such a buffer common (large
   folios, merged scatter-gather entries); the split is fixed in
   `deps/libkrun`'s `descriptor_utils`, and this smoke does the cold reads that
   used to fail at the disk's own default request size.
2. **The block pair silently not engaged.** A launch that cannot build the lower
   keeps the host-composed overlay and names the reason on stderr instead of
   failing, because there is no flag left to ask for a refusal (the `--nix` axis
   was removed: `--workspace-disk` derives the block lower). Without an assertion
   the run looks clean and is simply the slow path: the last check compares the
   two boots, so a silent regression cannot pass as "no lost pages".

## The two arms

| arm | launch | expected `/nix` |
|---|---|---|
| `block` | `--workspace-disk <out>/workspaces/block.raw --workspace-size 16G` | the guest's overlay, `lowerdir=/run/cang/nix-lower`, upper in `/workspace/.cang/nix/<digest>/` |
| `host` | no workspace disk | the host-composed overlay, `/nix` on virtio-fs, no block pair |

The guest probe is passed to the guest inline (base64) rather than as a file in
`/workspace`, and it prints its results as `RESULT key=value` lines on the
console: a block launch's `/workspace` is the disk image, so a file written there
is invisible to the host, and the host is what scores the run.

## Recreate

```bash
# cang's only implemented task-rootfs backend is btrfs-snapshot, so --out-dir
# must be on btrfs.
nix develop --command tools/nix-lower-cang-smoke/nix-lower-smoke.sh \
  --out-dir /home/dev/disp/nix-lower-smoke/run1

# reuse an image that is already loaded (the load is the slow part):
CONTAINERS_STORAGE_CONF=/path/to/store/storage.conf \
nix develop --command tools/nix-lower-cang-smoke/nix-lower-smoke.sh \
  --container localhost/cang:latest --out-dir /home/dev/disp/nix-lower-smoke/run2
```

Everything else is resolved from the flake (`.#cang`, `.#cang-musl`'s
guest-init). `--no-host-arm` skips the second boot.

## Score

| check | what it means |
|---|---|
| `block-vm-exit` | the `--workspace-disk` guest ran to completion |
| `block-lower-engaged` | `/nix` is an overlay over `/run/cang/nix-lower`, and the lower is mounted |
| `block-prep-ready` | the guest's `/nix` prep worker wrote `state=ready` |
| `block-no-lost-pages` | cold reads of 40 store files logged no new `csum failed` and no `EIO` |
| `host-vm-exit` | the no-workspace-disk guest ran to completion |
| `host-keeps-host-overlay` | with no workspace disk `/nix` is on virtio-fs and the block pair is not engaged |
| `host-no-lost-pages` | cold reads on the host path logged no new `csum failed` and no `EIO` |
| `block-lower-is-faster` | `du -s /nix/store` is at least 1.3x faster on the block lower |

A run prints `VERDICT: PASS` or `VERDICT: FAIL` and exits 0/1.

`CANG_LENIENT_NIX_LOWER_LOSS=1` records a lost page as `KNOWN` instead of a
failure, printing the count either way. It was added while the region-spanning
descriptor fault was live (the release driver still passes it); a run on a kernel
whose reads were clean without it can drop the flag and let the check gate on its
own.
