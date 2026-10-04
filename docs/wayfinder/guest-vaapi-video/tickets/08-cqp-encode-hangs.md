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
