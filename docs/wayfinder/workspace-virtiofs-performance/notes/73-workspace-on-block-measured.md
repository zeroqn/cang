# /workspace on virtio-blk, measured: raw image vs the real block device

2026-11-22, ticket 18/28 follow-up. Bob asked for the workspace's own I/O to be
measured on a block device: a **raw btrfs image** (`--workspace-disk <file>`) and the
**real block device** (`--workspace-disk /dev/sdc`), with the host directory over
virtio-fs skipped.

Artifacts: `cang` `/nix/store/yj2zg8iwvhxm0ic9kb3kx3nnwdcyi6la-cang-0.22.0` (the
`result` symlink after `38f3595d`), image `localhost/cang:latest`. Images:
`/home/dev/disp/wsbench/ws.raw` (24 GiB sparse, seeded via `cang workspace init
--from` the synthetic workload, 24 008 entries) and `/dev/sdc` (32 GiB, formatted
with `--workspace-format`, same seed). `/dev/sdc` is `QEMU HARDDISK` - a real block
**device** for cang, but this host is itself a VM, so it is not physical media.

Workload: the harness's synthetic tree (2401 crates), built **cold in
`/workspace/target`** (cargo's own default, since ticket 27 removed
`CARGO_TARGET_DIR`), `CARGO_BUILD_JOBS=26`, `RUSTC_WRAPPER=` empty,
`CARGO_INCREMENTAL=0`, timer inside the guest around `cargo build --offline` only.
Every run verified the same work: **2401 `Compiling`**, `target=172M`, `fs=btrfs`,
`cpus=26`.

| run | raw image (24 GiB sparse on `/dev/sdb`) | real `/dev/sdc` (32 GiB) |
| --- | --- | --- |
| 1 | 48.7 s | 59.7 s |
| 2 | 61.5 s | 56.5 s |
| 3 | 48.2 s | 60.0 s |
| 4 | 48.0 s | 43.4 s |
| 5 | 26.7 s | 24.3 s |
| 6 | 21.8 s | 21.2 s |
| **median** | **48.1 s** | **49.9 s** |

Results, in the order they matter:

1. **The two block-device backings are indistinguishable** (medians 48.1 vs 49.9 s,
   4% apart, inside a 21-62 s spread that both arms show). A sparse image on the
   host's own btrfs and a separate 32 GiB device build this workload at the same
   rate, which is what the design predicted: both are virtio-blk over a btrfs the
   guest owns, and neither is a storage-medium experiment.
2. **The spread, again, is run-scale and not device-scale**: the same arm on the same
   image went 48.0 s at 17:26 and 21.8 s at 17:40, and 21-27 s in the last two runs
   of both arms. This is the same non-stationarity note 72 §7 recorded for the
   harness's guest arms (and it is now visible *without* any of the harness's
   plumbing, on a plain `cang --workspace-disk ... -- bash -lc 'cargo build'`), so it
   is a property of this host's guest execution, not of the benchmark tool.
3. **Against virtio-fs the block device is ~3x**, from the same day's harness arm
   that builds in `/workspace` on the share (`guest-virtiofs`: 176.0 / 149.2 / 76.7 s,
   median **149.2 s**, note 72 §7) - 149.2 s against 48-50 s. **Caveat**: that arm
   ran in a different window (16:20-16:55) and its own runs span 2.3x, so this ratio
   is cross-window and is stated as approximate, not as a measurement of the share's
   cost.

What this does not establish: a per-operation cost for the workspace (no `readdir`/
`stat`/`write` histogram was taken on either backing), and any comparison at equal
host conditions - the host could not be held still long enough for one. What it does
establish is the shape the map wanted: a cold 2401-crate build writing into
`/workspace` on a block device completes in tens of seconds on this host, the
backing device (image vs real disk) makes no difference, and the run-to-run variance
dominates everything else.
