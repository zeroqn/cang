# 08 - the guest VA encode hangs in CQP mode on complex 640x360 content

Status: open. Measured 2026-10-04 with the image that finally ships the guest VA driver
(`localhost/cang:latest` id `60c470a1a2e3`, `/usr/lib/cang-va-runtime/dri/virtio_gpu_drv_video.so ->
../lib/libgallium.so` confirmed in-guest by the probe's own `PROBE driver:` line). All arms below are
one VM, `--gpu=drm`, `h264_vaapi`, `-bf 0`, 30 frames of the same 640x360 clip, 60 s in-guest cap
(`timeout -k 5 60`, SIGKILL) - a stall is `rc=137`, 0 bytes, `frame= 0` for the whole cap.

| arm | input | encode args | result |
| --- | --- | --- | --- |
| `qp26-va` | clip360.mp4 | `-qp 26` | **stall** |
| `qp45-va` | clip360.mp4 | `-qp 45` | **stall** |
| `bitrate-va` | clip360.mp4 | `-b:v 200k -maxrate 250k -bufsize 500k` | **ok, 41 548 B, 30 frames** |
| `ffv1-va` | clip360's first 30 frames re-coded losslessly (ffv1), 640x360 | `-qp 26` | **stall** |
| `ffv1-small-va` | the same pixels scaled to 320x240 | `-qp 26` | **ok, 21 449 B** |
| `testsrc-va` | lavfi testsrc2 640x360 | `-qp 26` | **ok, 48 570 B** |
| `black-va` / `white-va` | lavfi flat colour 640x360 | `-qp 26` | **ok, 442 B / 437 B** |
| `trans-va` | clip360 re-encoded in-guest by libx264 | `-qp 26` | **stall** |
| `tagged-va` | testsrc2 mp4 tagged tv/bt709 | `-qp 26` | **ok, 139 232 B** |
| `mp4-soft` | clip360.mp4 | libx264 | **ok, 599 frames** (control) |

What the table says:

- not the container, the stream, the colour metadata or the decoder: the clip's own pixels re-coded
  losslessly (ffv1) stall just the same, and a locally generated mp4 of the same geometry and frame
  count is fine;
- not the size alone: testsrc2 at the same 640x360 encodes, and the clip's pixels at 320x240 encode;
- it is **content x size in constant-QP mode**: complex 640x360 content stalls under `-qp`
  (both 26 and 45), while the same content at the same size under a **bitrate cap** encodes fine.

That points at the rate-control path rather than at buffer sizing (an earlier reading): a capped
bitrate works, `-qp` does not, and the failing arms hang *inside the encoder* with no output at all.

Consequence for the older evidence: every ticket 05 measurement (PSNR, the 2.4x bitrate, the chroma
error) and every ticket 04 caps measurement were taken while the loaded image had no
`/usr/lib/cang-va-runtime` layer at all, i.e. while the guest was silently running the pinned prebuilt
mesa on the *old* wire format. Those measurements must be re-taken before they are used again; the
"caps forwarding regresses bitrate 1.5x" verdict in ticket 04 is among them.

Next: instrument what reaches the host in the failing `-qp` case (the vrend parameter dump, logged to
the shared workspace so the render server's private /dev/shm cannot hide it) and compare it with the
working bitrate arm and with the host's own `-qp` control, which succeeds on the same content.

## Where the hang is (2026-10-04, second round)

Method: a small libva-interposing shim (`vaparam.c`, built `-shared -fPIC -ldl`, logging to the
shared workspace) injected two ways - into cang's VM worker with `patchelf --add-needed` plus a
mirrored prefix, and into the guest's `ffmpeg` with `LD_PRELOAD` + `CANG_VAPARAM_LOG`. It logs
`vaInitialize`, `vaCreateBuffer`, `vaBeginPicture`, `vaRenderPicture` (with a hex dump of every
rendered buffer), `vaEndPicture`, `vaSyncSurface`, `vaMapBuffer`, `ioctl` (named for virtgpu) and
arms a `SIGALRM` watchdog that dumps a backtrace if a call blocks.

Measured, CQP arm (stalls) versus bitrate arm (works), same VM, same clip, 30 frames:

- the guest renders, for frame 1: sequence parameter (1132 B), a rate-control misc (28 B), picture
  parameter (648 B), three packed-header pairs (SPS `00 00 00 01 67 64 0c 1e…`, SEI
  `…Lavc62.28.102 / VAAPI 1.23.0…`, PPS `00 00 00 01 65 88 80 4f`) and the slice (3140 B) -
  `RENDER n=10` - and then **never returns from `vaRenderPicture`** (`RENDER ret=` and
  `END enter` never appear). The bitrate arm renders the same buffers (n=12, the extra two being
  rate-control misc buffers) and completes 30 frames.
- the host-side shim sees `vaInitialize` once and **zero frame submissions** for the CQP arm, while
  the bitrate arm produces 30 complete `BEGIN/RENDER/END/SYNC/MAP` sequences.
- the guest's `ioctl` trace for the CQP arm ends at `RENDER n=10`: after it, two non-DRM `ioctl`s and
  one `VIRTGPU_GET_PARAM`, and **no `VIRTGPU_EXECBUFFER` at all** - the frame is never submitted to
  the host.
- the watchdog's backtrace lands in `pthread_cond_timedwait` from an ffmpeg frame, i.e. in a thread
  that is idle-waiting - it is not necessarily the hung thread (SIGALRM is delivered to an arbitrary
  thread), so treat it as weak evidence only.
- during the stall every host thread is in `futex_wait`/`epoll`, the render server is idle-polling
  and the vCPU is in `kvm_vcpu_block`: nothing is computing anywhere.

So the stall is a userspace wait in the guest, inside `vaRenderPicture`, before the frame is ever
submitted. Falsified along the way: ffmpeg threading (`-threads 1`, `-filter_threads 1` still
stall), the guest driver as the *sole* factor (the pinned prebuilt mesa stalls too, though that arm
is confounded by the wire-protocol mismatch: the host still carries the patched wire), and the host
encoder (it is never called).

Next step for a full root cause: instrument the guest's mesa `virgl_video.c`/VA frontend (a marker
build of `pkgs.mesaVaApi` plus an image rebuild) to see which buffer or fence `vaRenderPicture` is
waiting on. Until then the practical workaround is a bitrate cap (`-b:v`, `-maxrate`, `-bufsize`),
which encodes the same content successfully.

## Upstream virglrenderer main does not fix it (2026-10-04)

A cang built against **virglrenderer main HEAD** (`aafa9bd234a43c31004ec768ce000b21cf7b99ca`, the
commit that prompted the question - it is only `venus: fix the vulkan_metal.h include on macOS`)
behaves byte-for-byte like the pinned 1.3.0:

| arm | 1.3.0 (baseline) | upstream main |
| --- | --- | --- |
| `-qp 45` | rc=137, 0 B, frame 0 | rc=137, 0 B, frame 0 |
| `-b:v 200k -maxrate 250k -bufsize 500k` | rc=0, 41 548 B, 30 frames | rc=0, 41 548 B, 30 frames |
| `-qp 26` | rc=137, 0 B, frame 0 | rc=137, 0 B, frame 0 |

All five wired cang patches apply cleanly to that revision (the raw-header wire patch included, so the
patched host and the guest image stay compatible), nixpkgs' own
`1001-virglrenderer-amdgpu-Use-inttypes-format-defines.patch` is already upstream and must be dropped,
and main's `venus-protocol` meson wrap needs vendoring. Upstream's only vrend-video commits since
1.3.0 are `956b034f` (P210 format, decode), `c71b72b2` (iov refactor) and three `vrend_decode.c`
commits - nothing in the encode path. **Do not chase a virglrenderer bump for this defect.**

## Falsified since: the upload fence, and the next step (2026-10-04)

A cang built with `virglrenderer-encode-upload-fence.patch` **unwired** still stalls in CQP
(`-qp 45` and `-qp 26` both rc=137, 0 B, frame 0; the bitrate arm completes, 37 565 B rather than
41 548 B - the fence does change the picture, which is why it stays wired). So the `glFinish` fence
is not the cause either, and the defect is in the guest's VA driver or in the guest/host video
protocol, not in any patch cang carries.

Next step, with the anchors already worked out (all verified to exist in the patched tree):

- instrument the **guest's** mesa (`pkgs.mesaVaApi`) rather than rebuilding the image: build a marked
  driver and point the probe's `LIBVA_DRIVERS_PATH` at a directory in the shared workspace, so no
  image rebuild or `podman load` is needed (`LIBVA_DRIVERS_PATH=/workspace/vadrv` with
  `libgallium-<ver>.so` plus a `virtio_gpu_drv_video.so -> ../lib/libgallium.so` symlink).
- mark, via `postPatch` + `substituteInPlace --replace-fail` (no source tree needed):
  `virgl_video.c`'s `virgl_encode_begin_frame(vcdc->vctx, vcdc, vbuf);` and
  `vs->vws->resource_wait(vs->vws, vres->hw_res);`; markers go to stderr, which the probe already
  captures into `/workspace/<arm>.log`.
- the same driver tree can also carry a `virgl_drm_winsys` marker around the fence wait, which is the
  prime suspect: the guest waits with no syscall in flight, i.e. likely on a syncobj/fence the host
  never signals.
- build it without touching the flake:
  `nix build --impure --expr 'let pkgs = import (builtins.getFlake "github:NixOS/nixpkgs/<rev from flake.lock>") {}; in pkgs.mesa.overrideAttrs (old: { patches = (old.patches or []) ++ [ /home/dev/cang/cang/nix/pkgs/patches/mesa-virgl-encode-raw-headers.patch ]; postPatch = <markers>; })' -o <root>`

## Last finding of the round: a userspace condvar wait in the main thread (2026-10-04)

Extending the guest-side shim to interpose `pthread_cond_wait` / `pthread_cond_timedwait` /
`pthread_cond_clockwait` (logging a backtrace on each entry) shows the guest's **main** ffmpeg thread
sitting in `pthread_cond_timedwait` on a single condvar, returning `110` (ETIMEDOUT) every ~0.5 s, at
the moment the shim's `vaRenderPicture` is still in progress. No other thread uses a condvar at all.
Combined with the ioctl trace (no `EXECBUFFER` in the whole stalled arm) and the idle host, this says
the guest blocks in a **userspace wait on a condvar/fence inside the VA driver's `vaRenderPicture`**,
with nothing ever submitted to the host - not a GPU wait, not a host-side block.

(Note: the backtrace's innermost non-shim frame is `ffmpeg+0x3a441`; libgallium has no unwind
information, so frames between the shim and ffmpeg are missing. It cannot yet be said whether the
condvar belongs to mesa's virgl winsys submission path or to ffmpeg itself.)

### Attempted marker build, and the recipe to redo it

`nix build --impure --expr 'pkgs.mesa.overrideAttrs (old: { patches = … ++ [raw-headers patch]; postPatch = "…substituteInPlace…"; })'`
builds and produces a usable `libgallium-26.1.8.so`, and `overrideAttrs` + `postPatch` demonstrably
runs (a probe `postPatch = "echo MARKER-OK"` shows up), but the `substituteInPlace --replace-fail`
edits did **not** reach the compiled object (`strings libgallium-26.1.8.so | grep CMARK` = 0) even
though the build reported success. **Redo the markers with a real patch file** (like cang's own mesa
patch, which is visibly applied - the build log prints `patching file src/gallium/drivers/virgl/virgl_video.c`),
anchored on the same lines: `virgl_encode_begin_frame(vcdc->vctx, vcdc, vbuf);`,
`/* Transfer picture desc */`, `vs->vws->resource_wait(vs->vws, vres->hw_res);`, plus a marker in
`src/gallium/winsys/virgl/drm/virgl_drm_winsys.c` around its fence wait. Inject the result through the
shared workspace (`LIBVA_DRIVERS_PATH=/workspace/vadrv`, with
`virtio_gpu_drv_video.so -> <store>/lib/libgallium-26.1.8.so`) - no image rebuild needed, and the
guest can read store paths because it shares the host store.

## Why the driver cannot be injected through the workspace (2026-10-04)

The marker build itself works: overriding `src` with a `runCommand` that copies `${pkgs.mesa.src}`,
applies cang's raw-headers patch and inserts `fprintf(stderr, "CMARK …")` markers (asserted inside the
derivation: `grep -c CMARK` must be >= 3) produces a `libgallium-26.1.8.so` that demonstrably carries
the markers (`grep -rl 'CMARK begin_frame'` finds it; note `strings` is not on PATH here, which made an
earlier `strings | grep -c` read as "no markers" when they were present all along).

But pointing the guest at it fails structurally: `LIBVA_DRIVERS_PATH=/workspace/vadrv` with
`virtio_gpu_drv_video.so -> /nix/store/<new mesa>/lib/libgallium-26.1.8.so` makes the guest's
`vaInitialize` fail (`Failed to initialise VAAPI connection: -1`). The guest's `/nix/store` is the
**image's** store, not the host's live store - cang's `ffmpeg` path works in the guest only because the
image ships that closure. A newly built mesa's store path does not exist inside the guest, and its
absolute rpath dependencies cannot be bind-mounted in.

So instrumenting the guest driver requires **building the marks into the image**: mark `mesaVaApi`
(`src` override via the proven `runCommand` route, with the `patches` list untouched so cang's own
patch still applies in the patch phase and the sed anchors stay pristine lines such as
`virgl_encode_begin_frame(vcdc->vctx, vcdc, vbuf);`, `/* Transfer picture desc */`,
`vs->vws->resource_wait(vs->vws, vres->hw_res);`), `nix build .#container`, `podman load`, then run the
CQP arm and read the `CMARK` lines from the arm's log. The image's mesa must be rebuilt afterwards
without the markers.
