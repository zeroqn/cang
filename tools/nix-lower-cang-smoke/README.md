# /nix Block-Lower Cang Live Smoke

A live smoke for **the `/nix` block lower**: that a launch serves `/nix` from the
read-only btrfs image on virtio-blk, that the guest mounts it
force, that cold reads lose no page, and that `CANG_NIX_LOWER_BLOCK=0` still keeps
the host-composed overlay.

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
2. **The block lower silently not engaged.** The host falls back to the
   host-composed overlay when it cannot build or lock the image, and the launch
   never fails because of it. Without an assertion the run looks clean and is
   simply the slow path: the last check compares the two boots so a silent
   regression cannot pass as "no lost pages".

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
guest-init). `--no-opt-out` skips the second boot; the release driver takes that
path when it only needs the engagement and no-loss checks.

## Score

| check | what it means |
|---|---|
| `block-vm-exit` | the guest ran to completion |
| `nix-lower-engaged` | `/nix` is an overlay over `/run/cang/nix-lower` |
| `no-lost-pages` | cold reads of 40 store files logged no new `csum failed` |
| `no-read-errors` | both sweeps finished without `Input/output error` |
| `opt-out-keeps-host-overlay` | `CANG_NIX_LOWER_BLOCK=0` leaves `/nix` on virtio-fs |
| `block-lower-is-faster` | `find /nix/store` is at least 2x faster on the block lower |

A run prints `VERDICT: PASS` or `VERDICT: FAIL` and exits 0/1.

`CANG_LENIENT_NIX_LOWER_LOSS=1` records a lost page as `KNOWN` instead of a
failure, printing the count either way. It was added while the region-spanning
descriptor fault was live (the release driver still passes it); a run on a kernel
whose reads were clean without it can drop the flag and let the check gate on its
own.
