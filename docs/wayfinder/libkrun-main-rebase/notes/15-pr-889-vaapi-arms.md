# PR 889 x VA-API decode: the arms

Raw evidence for
[`../tickets/15-pr-889-breaks-vaapi-decode.md`](../tickets/15-pr-889-breaks-vaapi-decode.md).
All arms ran 2026-10-14 on the btrfs loop disk, cang 0.12.0, guest kernel
`7.2.7-hardened1`, image `localhost/cang:latest` in
`/home/dev/cang/disk/virglrenderer-store`, guest-init
`jfbmpg69z41c607bh0q2dgkz7h8i9ycb-cang-static-x86_64-unknown-linux-musl-0.12.0`,
virglrenderer `4332hsp1zciwrjkj5ai1jgmwblpxrchi-virglrenderer-1.3.0`, host AMD
Radeon RX 7600M XT (navi33, Mesa 26.1.8).

Command (per arm), with `--cang` the store path from the table:

```
export CONTAINERS_STORAGE_CONF=/home/dev/cang/disk/virglrenderer-store/storage.conf
nix develop --command tools/virglrenderer-cang-smoke/virglrenderer-smoke.sh \
  --cang <path>/bin/cang --guest-init <guest-init> \
  --container localhost/cang:latest --out-dir <out> \
  --mem 4 --timeout 900 --frames 60 --skip-chromium
```

| libkrun source (rev) | cang store path | out-dir | verdict |
| --- | --- | --- | --- |
| fork tip `2855f4d1` | `979rqmyriqnwcjzcx4drgrcdhiw5g1gj` | `vr-oldlibkrun-current-tree` | PASS |
| fork tip `2855f4d1` + PR 889 (`f494ef63`) | `kpz1nyvlhnr4d3ksajj9kqzc0m5slaxd` | `vr-exp-old-pr889` | FAIL |
| re-based `f8c18a1c` | `rnjgn88arsx38r8gl2b9270d9qqrqvvc` | `vr-smoke-rebase6c` | FAIL |
| re-based minus PR 889 (`95a653bf`) | `99ygja92lkmylsfk7g98dzn5k416gi3y` | `vr-exp-e9` | PASS |
| re-based, GPU `51367cfa` reverted (`a1d33469`) | `n0nbdjwavxa15i4gn4p08476abl6q9b1` | `vr-exp-e5` | FAIL |
| re-based, ACPI `5bc7468a` reverted (`ecbfaa7a`) | `vfldaidw9ywwk1pfnycmv2zimaw3nbs1` | `vr-exp-acpi` | FAIL |
| re-based, GPU + ACPI reverted (`a04ea97c`) | `4zas0k40jdmfm8bqv3yw8prcgwfxqxwd` | `vr-exp-e6` | FAIL |
| fork tip `2855f4d1` (night session's build) | `7bvlhx3wspvqgn0jfn4k9dvvpqfqzp6l` | `smoke-head/vaapi` | PASS |

PASS numbers are identical everywhere (`y:46.633758 u:44.227307 v:44.408519`,
round trip `y:46.537574 u:43.627419 v:42.909682`); FAIL numbers are identical
everywhere too (`y:5.372522 u:9.994820 v:9.662058`), which is the all-zero-luma
signature rather than a shared code path.

## What each failing picture is made of

Frame 0 of `decoded-vaapi.nv12` (640x360 NV12, 345600 B/frame):

```
bad   f0   Y nz=     0/230400 | UV nz=     0/115200 | Y[0:8]=[0,...] UV[0:8]=[0,...]
bad   f1   Y nz=     0/230400 | UV nz=115200/115200 | UV[0:8]=[128,128,128,128,...]
good  f1   Y nz=230400/230400 | UV nz=115200/115200 | Y[0:8]=[78,76,76,76,...] UV=[92,241,111,227,...]
```

i.e. luma cleared to 0, chroma cleared to the neutral 128, first surface untouched
(all zero) - the guest's own cleared VA surfaces.

## Reproducing the experiment revs

`f494ef63`, `95a653bf`, `a1d33469`, `ecbfaa7a` and `a04ea97c` live on local
branches in `deps/libkrun` (`exp-old-plus-pr889`, `exp-drop-pr889`,
`exp-no-blob-align`, `exp-revert-acpi-irq`, `exp-e6`); they are not pushed, and the
branch is back on the pushed tip `f8c18a1c`. Rebuild each with
`nix build .#cang --no-link --print-out-paths --override-input libkrun-src
"git+file://$PWD/deps/libkrun?rev=<full sha>"`.

## Cross-checks

- Compiled source per arm: each cang store path's `cang-workspace-source` was
  diffed against a worktree of the rev it claims. `a1d33469`/`a04ea97c` have
  `src/devices/src/virtio/gpu` byte-identical to `2855f4d1`; `95a653bf` has no
  `drain_rxq`; `f494ef63` differs from `2855f4d1` in `muxer_thread.rs` only.
- Runtime closures of `979rqmyr` (PASS) and `rnjgn88` (FAIL) differ in their own
  store path only - same `libkrunfw`, virglrenderer, mesa, krun-init.
- The two krun-init outputs (`9nanrh02...`, `91zh9iy1...`) are byte-identical
  (`adfb25d46ca899e0b4a31e3ea49eb65f94309735efa339bcd28c0037194ddf05`).
- Guest probe (both bases, `/home/dev/cang/disk/probe2-{old,oldpr889}`): guest
  `date` tracks the host (05:56:52 vs 05:56:44; 05:57:06 vs 05:56:57) and
  `/dev/vsock` is present, so PR 889 does not stall timesync delivery.
- Chromium/venus arm (`smoke-rebase6`, waypipe mode) passes with PR 889 carried,
  so the regression is specific to the vrend decode read-back, not the whole GPU.

## PR 889 split, after the re-cut (2026-10-14)

Both halves of the PR blank the decode on their own, and CPU load does not flip
either arm:

| build | arm 2 |
| --- | --- |
| branch tip `421247f6` + `drain_rxq()` with the signal removed (`900cce0a`) | FAIL `f22b572f` |
| branch tip `421247f6` + `signal_used_queue()` each iteration, no drain (`db052624`) | FAIL `f22b572f` |
| PR 889 + three guest CPU hogs | FAIL `f22b572f` |
| pre-rebase tip + three guest CPU hogs | PASS `3006d5a7` |
| pre-rebase tip + 28 host CPU hogs | PASS `3006d5a7` |
| PR 889, nothing else on the host | FAIL `f22b572f` |

Guest-side facts (probe3, `/home/dev/cang/disk/probe5-*`, `probe6-*`, `probe3-*`):

- the software decode of the same staged clip is byte-identical in every arm
  (`3006d5a7…`), so the input, the file path and the guest's userspace are the same;
- the hardware decode reports no error and produces 60 frames / 20736000 bytes in
  both states (`null_rc=0 frame=60`), and the guest kernel logs nothing during it;
- at `-loglevel debug` the ffmpeg/libva logs of a PASS and a FAIL arm are the same
  size (48435 B) and differ only in where `Map surface` / `Unmap surface` / `Slice
  ... data buffer` interleave, i.e. the decode does the same work and only the
  read-back's ordering moves;
- the virtio-gpu feature word the guest negotiates is identical in both states,
  including bit 5 absent (the cang kernel's `FENCE_PASSING`), so the guest-handle /
  fence-passing bits are not in play;
- the failing picture is the guest driver's cleared surface: luma 0, chroma 128,
  the first frame's surface entirely zero.
