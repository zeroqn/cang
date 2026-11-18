---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# The "backing slower than the page cache" arm: instrument built, blocked by a privileged boundary

The open risk from notes 35-38 is that every read measured so far came from a backing the host
served in microseconds, where parallel preads have nothing to queue behind. This is what I built to
test it, what it proved, and where it stopped.

## 1. The instrument works

`slowfuse.c` - a libfuse3 passthrough that serves a directory and sleeps `delay_us` per read, with an
optional `seek_us_mb` term proportional to the distance from the previous read's end (which models a
device that pays for seeks: sequential readers stay cheap, parallel random readers thrash). Measured
directly on the host, same 64 MiB file, 16 KiB reads:

| path | mean | p50 | p90 |
|---|---|---|---|
| backing file directly | 3 us | 2 us | 7 us |
| through slowfuse, `delay_us=3000` | **3 170 us** | 3 153 us | 3 237 us |

So a backing with controlled, ~1000x slower service time is available, and the `seek_us_mb` knob
makes the penalty concurrency-dependent, which is exactly the shape the question needs.

## 2. It cannot be mounted where the VM worker can open it

Two independent blockers, both confirmed rather than assumed:

- **FUSE's uid rule.** The worker sets its filesystem identity to namespace root
  (`configure_vm_worker_filesystem_identity`, `identity.rs`) - uid 0 inside its user namespace, which
  is not the mount owner. FUSE refuses every `open` from a different uid unless the mount carries
  `allow_other`, and mounting with it fails here: `option allow_other only allowed if
  'user_allow_other' is set in /etc/fuse.conf`. Making the backing files 0666 changed nothing, which
  is how I know it is the mount-owner check and not the file mode.
- **The sandbox read set.** A cang worker only opens paths inside its rule set, and a state root
  outside it fails with `AttachDevice("block: Permission denied")`. Mounting the slow image over the
  *real* `nix-lower` directory does satisfy that (the run gets as far as mounting,
  `mounted=fuse.slowfuse`), so this half is solved - the uid rule is what remains.

One privileged line would unblock it: `echo user_allow_other >> /etc/fuse.conf` (root). Everything
else is staged: `blkread/run-slow.sh` mounts the slow image over the live `nix-lower` directory,
moves the real image aside for the duration and restores it in a `trap`, and takes `DELAY_US`,
`SEEK_US_MB` and `SERIAL`.

## 3. The root-free alternative also stalled, and why

`CANG_BLOCK_DIRECT_IO=1` (already in the fork's API) bypasses the host page cache, which is a real
"not cached" backing at ~50-150 us per read. It needs an uncompressed image, because btrfs falls back
to buffered I/O on compressed extents - I verified that earlier by reading 64 MiB with `O_DIRECT` and
still finding 49 MB resident. I built such an image (`mkfs.btrfs --rootdir --shrink`, no reflink: 303
extents, 0 `encoded`) and swapped it into the state root, and the arms then failed for an unrelated
reason: the guest did not compose the block lower (`stat -f /nix` = fuse, `vdc=none`) and the probe's
`PATH` was empty, so every arm timed out on missing coreutils. A diagnostic boot showed `nix-prep`
still at `state=running` when the probe started - the same guest-init race noted earlier, which this
runner does not guard against by waiting for the prep status file.

I reverted the image swap (the compressed image is back) and re-ran the standard arm to prove the
state root is healthy: `find` 392/410/396 ms, 128 MiB read 415.6 ms - the normal numbers.

## 4. What I would do next, in order

1. Ask for the one `user_allow_other` line, then run `run-slow.sh` with `DELAY_US=2000` and
   `SEEK_US_MB=200` for both `SERIAL=1` and `SERIAL=0`. That is the decisive arm: with a
   seek-penalised backing, serial should win if parallel preads really do queue.
2. Or fix the probe to wait for `nix-prep.status` to say ready before arming (a two-line change) and
   retry the uncompressed-image/O_DIRECT route, which needs no privileges.

## Evidence

`slowfuse.c` (compiled against fuse-3.17.4; `allow_other` path in place but unusable without
`user_allow_other`), `blkread/run-slow.sh`, `blkread/run-od.sh`, `blkread/run-diag5.sh`;
`run-diag5-s0d0-20261118T130204Z` (`diag.txt`: vdc present with `LABEL="CANG_NIX_LOWER"`,
`nix-prep` `state=running`), `run-nixread-restore-20261118T*` (state root healthy after the revert).
The uncompressed image was removed; the compressed one is back at
`…/nix-lower/ec5b74e0….raw`.
