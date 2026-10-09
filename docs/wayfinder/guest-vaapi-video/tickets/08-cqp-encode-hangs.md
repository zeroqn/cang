---
label: wayfinder:research
title: The guest VA encode hangs in CQP mode on complex 640x360 content
status: closed
blocked_by: []
claimed_by: bob + pi session (2026-10-04)
---

# 08 - the guest VA encode hangs in CQP mode on complex 640x360 content

> **Renamed 2026-10-09:** the host-facing package named below as
> `packages.<system>.mesa-rbsp-bounds` is now `packages.<system>.mesa-cang`.

Measured 2026-10-04 with the image that finally ships the guest VA driver
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

## The stall is *before* `virgl_encode_begin_frame` (2026-10-04, marked image)

The marks were finally measured by building them into the image (temporary `src` override in
`nix/lib/systems.nix`: copy `${old.src}`, `patch -p1 < mesa-virgl-encode-raw-headers.patch`, `sed` in
`fprintf(stderr, "CMARK …")` at the anchors, assert `grep -c CMARK >= 3`, then `nix build .#container`,
`podman load`, run). Guest confirmed the marked driver:
`/nix/store/xanld3z3y94rq20a5vhgpr3s20b6q4d5-mesa-26.1.8/lib/libgallium-26.1.8.so`.

| arm (5 frames, `-bf 0`) | result | CMARK lines in its log |
| --- | --- | --- |
| `-qp 26` (stalls) | rc=137, 0 B | **0** |
| `-b:v 200k -maxrate 250k -bufsize 500k` | rc=0, 10 578 B | **40** (including `resource_wait enter/done` pairs) |

So in the failing arm the guest never reaches `virgl_encode_begin_frame` -
`vaRenderPicture` blocks *earlier* in the VA frontend's handling of the buffer list, which is exactly
what the shim showed (it stops after `RENDER n=10`, with no `EXECBUFFER` and every host thread idle).
The next marks belong in `src/gallium/frontends/va/` (picture/buffer/surface handlers) and on
`virgl_resource_wait` / the winsys fence wait, which are the entry points reachable before
`begin_frame`.

## Narrowed to one call: the first H.264 packed-header *data* buffer (2026-10-05)

Markers built into the image (same `src`-override route; the image ran the marked driver, confirmed by
the probe's `PROBE driver:` line) traced the encode buffer list of the stalling CQP arm against the
working bitrate arm. `CMARK` markers were placed at: the entry of `vlVaHandleEncBufferType`, inside
`vlVaRenderPicture`'s loop (after the buffer lookup, and after the encode dispatch), and in the H.264
packed-header parser.

Stalling arm (`-qp 26`, 30 frames, killed at 60 s):

```
CMARK loop i=0        CMARK encbuf type=22   CMARK after enc type=22 status=0
CMARK loop i=1        CMARK encbuf type=27   CMARK after enc type=27 status=0
CMARK loop i=2        CMARK encbuf type=23   CMARK after enc type=23 status=0
CMARK loop i=3        CMARK encbuf type=25   CMARK after enc type=25 status=0
CMARK loop i=4        CMARK encbuf type=26   <-- and nothing further
```

Working arm (`-b:v 200k -maxrate 250k -bufsize 500k`) runs the same sequence to `i=9` and then
dispatches type 24 (the slice) for every frame - 30 frames, 10 578 B.

So the guest blocks **inside the handler for the first `VAEncPackedHeaderDataBufferType`
(`vlVaHandleVAEncPackedHeaderDataBufferTypeH264`, i.e. the 40-byte SPS data buffer)**, with every
previous buffer returning success. `i=5` (the next packed-header *parameter* buffer) is never looked up.
That is consistent with everything measured before: no `EXECBUFFER` leaves the guest, the shim stops at
`RENDER n=10`, the host never sees a frame, and the host is idle while the guest is not making progress.

Whether that handler is *spinning* (an unbounded `vl_vlc` loop in `parseEncSpsParamsH264` /
`vlVaAddRawHeader`, both reached from `picture.c:465`/`picture_h264_enc.c:801`) or *blocked* on a
userspace wait is the one question left; the next attempt should answer it with a real patch file
rather than `sed`-injected markers, which proved brittle (a statement before the first `case` is
dropped by `-O2`, and inserting into multi-line calls breaks the build). Cheap alternative that needs
no image rebuild: run the arm under the guest's `strace -f -p` to see whether the stalled process makes
any syscall at all (no syscalls over 60 s = a spin, not a wait).

## Spin or wait? (2026-10-05, strace inside the guest)

`strace -f -p <ffmpeg> -o …` attached to the stalling CQP arm 10 s in and held for 4 s. Output: a
*single* line, `rt_sigsuspend([], 8 <detached ...>)` (plus attach/detach notices), i.e. over four
seconds the process entered one syscall and stayed in it. That is evidence *against* the "unbounded
`vl_vlc` scan" reading (a spin would make no syscall at all and strace would show `???`/nothing), but
it is not conclusive on its own: strace also reports the syscall a tracee is parked in at attach time,
and an earlier shim run saw the same arm's main thread returning from `pthread_cond_timedwait` every
~0.5 s. Both readings agree on the important part: the guest is **not** blocked on the host - nothing
is submitted (`no EXECBUFFER`), the host is idle, and the stack never leaves the VA frontend's
packed-header handling.

So the handler either waits on a userspace primitive inside `parseEncSpsParamsH264` /
`vlVaAddRawHeader`, or loops without syscalls in a way strace cannot see from a single attach. Next
step is a code-level read of that parser (`vl_rbsp`/`vl_vlc` loops in
`src/gallium/frontends/va/picture_h264_enc.c`) plus a host-side reproduction feeding the *exact* SPS
bytes the CQP arm sends (the shim recorded them: `00 00 00 01 67 64 0c 1e ac 2b 40 50 17 fc b8 0b 50
10 10 14 00 00 fa 00 03 6c a3 c2 01 0a 80`), which needs no VM at all.

## Which arms stall, and the host-side loop skeleton (2026-10-05)

Varying the client's own configuration on the same clip, 5 frames, `-bf 0` (45 s in-guest cap):

| arm | result |
| --- | --- |
| `-qp 26` (High, default GOP) | **stall** |
| `-profile:v main -qp 26` | ok, 10 514 B |
| `-profile:v baseline -qp 26` | rc=234 (ffmpeg refuses: baseline has no CABAC) |
| `-level 4 -qp 26` | **stall** |
| `-g 1 -qp 26` (all-intra) | ok, 32 514 B |
| `-bf 1 -qp 26` | **stall** |
| `-b:v 200k -maxrate 250k -bufsize 500k` | ok, 10 578 B |

So the trigger is the *High-profile, reference-frame* configuration under constant-QP; the Main
profile and the all-intra GOP both avoid it, as does bitrate mode.

The packed-header *data* buffers those arms send (captured with the shim):

```
stall  (-qp 26, High, 40 B): 00000001 67640c1e ac2b4050 17fcb80b 50101014 0000fa00 036ca3c2 010a8000 000168ee 38b0
ok     (-g 1,   High, 40 B): 00000001 67641c1e ac2b8140 5ff2e02d 40404050 00003e80 000e9b28 f08042a0 000168ee 38b0
ok     (main,         39 B): 00000001 674d4c1e 95a0280b fe5c05a8 08080a00 0007d000 01d3651e 10085400 000168ee 3880
```

(The PPS tail `00000001 68ee38b0` is identical in all three; only the SPS differs.)

A host-side skeleton of the handler's scan loop over mesa's own `vl_vlc.h`
(`notes/08-cqp-loop-repro.c`, no VM needed) shows two things and proves nothing more:

- with assertions on, the *stalling* SPS trips
  `vl_vlc_peekbits: Assertion 'vl_vlc_valid_bits(vlc) >= num_bits || vlc->data >= vlc->end' failed`
  (`src/util/vl_vlc.h:227`), i.e. the loop consumes past the end of the 40-byte buffer;
- with `-DNDEBUG` it then iterates past 1e6 with `vl_vlc_bits_left()` underflowed to 3 750 968 912
  (`= (unsigned)(32 - invalid_bits)` after `eatbits` ran past the end) - **but it does so for all
  three buffers**, including the two that work in the VM.

So the skeleton is not faithful enough to claim the infinite loop: it omits `vl_rbsp_init(&rbsp, &vlc,
…)` and the parse calls that follow, which advance the outer `vlc` between iterations. A faithful
reproduction (that plus the parse helpers, `-DNDEBUG`, bounded iteration count) is the next step, and
it is host-only work - no image rebuild, no VM.

### Correction after the faithful copy (2026-10-05)

Repeating the host-side work with the buffers extracted *exactly* from the shim's hex dump (40 B for
the stalling arm and for `-g 1`, 39 B for the Main arm) and with the loop's `vl_rbsp_init(&rbsp, &vlc,
~0, …)` call included - the piece the first skeleton left out - the outer scan loop **terminates**
for all three buffers at both emulation-byte settings (2 iterations with emulation bytes on, 1 with
them off, `bits_left` reaching 0). So the earlier "spins for all three" reading was an artifact of the
incomplete skeleton, and the spin is *not* in the outer loop: it is inside the parse helpers that loop
calls (`parseEncSpsParamsH264`, `parseEncPpsParamsH264`, `vlVaAddRawHeader`) or in `vl_rbsp_init`'s
emulation-byte handling for the real context configuration.

Decoding the SPS fields of the three buffers with mesa's own `vl_rbsp.h` (`notes/08-cqp-buffer-bytes.c`)
shows no single field that separates the stalling arm from the working ones:

| arm | profile | level | chroma | poc_type | max_num_ref | size |
| --- | --- | --- | --- | --- | --- | --- |
| `-qp 26` (stalls) | 100 | 30 | 1 | 2 | 1 | 640x368 |
| `-g 1` (works) | 100 | 30 | 1 | 2 | 0 | 640x368 |
| `-profile:v main` (works) | 77 | 30 | - | 2 | 1 | 640x368 |

Next step is therefore a standalone harness that carries the *whole* handler plus its parse helpers
(they are ~200 lines) rather than a skeleton, or an instrumented mesa build with
`-fsanitize=address,undefined` and an iteration counter in the parse helpers - both host-only.

### What the host-side work ruled out (2026-10-05)

With the exact 40/40/39-byte buffers, the faithful scan loop terminates (see above); the header sizes
it computes are all sane (`31/32` then `9/8` bytes), so no `size - nal_start` underflow reaches
`vlVaAddRawHeader`; and the parse helpers read by that handler are bounded - `parseEncSpsParamsH264`
has no loop that the captured SPS can drive (its `poc_type` is 2, so the
`num_ref_frames_in_pic_order_cnt_cycle` loop is never entered; the scaling-matrix branch returns
early), and `vlVaAddRawHeader` (`picture.c:465`) is a simple bounded copy.

So the *host* side of this defect is exhausted: nothing in the handler's own control flow explains a
non-terminating loop for these buffers. What remains is measured only inside the guest - the handler
is entered for the first `VAEncPackedHeaderDataBufferType` and never returns, the guest issues no
`EXECBUFFER`, and the host is idle - and the one syscall strace caught in four seconds
(`rt_sigsuspend([], 8)`) is more consistent with a *wait* than with a spin, so "userspace spin" should
not be treated as established either.

Next step, if this is picked up again: instrument the *guest's* parse helpers and
`vl_rbsp_init` directly (one image round, the marker recipe works), or attach a debugger inside the
guest, since the host-side reconstruction has now been tried and does not reproduce the hang.

## ROOT CAUSE: `vl_rbsp_ue()` reads past the end of the RBSP forever (2026-10-05)

Proven host-side, no VM, with `notes/08-vl-rbsp-ue-spin.c` (mesa 26.1.8's own `vl_vlc.h`/`vl_rbsp.h`).

`src/util/vl_rbsp.h`:

```c
static inline unsigned vl_rbsp_ue(struct vl_rbsp *rbsp)
{
   unsigned bits = 0;
   vl_rbsp_fillbits(rbsp);
   while (!vl_vlc_get_uimsbf(&rbsp->nal, 1)) {   /* no end-of-NAL check */
      ++bits;
      if (bits == 16)
         vl_rbsp_fillbits(rbsp);
   }
   return (1 << bits) - 1 + vl_rbsp_u(rbsp, bits);
}
```

Once the packed-header data buffer's RBSP is exhausted, `vl_rbsp_fillbits()` cannot add bits, the next
bit reads as 0 forever, and `bits` increments without bound - the loop never terminates.
`parseEncSpsParamsH264()` calls `vl_rbsp_ue()` for `seq_parameter_set_id` and many fields after it, so
`vlVaRenderPicture()` never returns and the guest hangs: no syscalls, no `EXECBUFFER`, idle host -
exactly what the guest-side measurements showed.

Reproduction with the exact 40-byte buffer the stalling arm sends (SPS+PPS, recorded with the shim),
consumed the way the parser does and then read further:

- `-O2` (asserts on): `vl_vlc_get_uimsbf: Assertion 'vl_vlc_valid_bits(vlc) >= num_bits' failed`
  (`src/util/vl_vlc.h:251`) - the reader has consumed past the end of the buffer;
- `-O2 -DNDEBUG` (as mesa ships): `vl_rbsp_ue()` does not return at all - caught by a 3 s alarm, at
  both emulation-byte settings.

This also explains the arm matrix: whether the parse needs bits past the end of that buffer depends on
the bytes. The stalling arm's High-profile constant-QP SPS walks off the end (markers: `CMARK rbsp
type=7` then nothing, with `CMARK iter 1`, so the hang is inside `parseEncSpsParamsH264`); the Main
profile SPS and the all-intra High SPS land exactly and succeed.

Fix direction (upstream mesa, `src/util/vl_rbsp.h`): bound the readers, for example

```c
while (vl_vlc_bits_left(&rbsp->nal) > 0 && !vl_vlc_get_uimsbf(&rbsp->nal, 1)) {
```

(or clamp `bits`) in `vl_rbsp_ue()` and `vl_rbsp_se()`. Because the guest driver is built from
`pkgs.mesaVaApi`, cang can carry this as a guest-side patch immediately - the host's own mesa parses
packed headers through the same code path, so it is worth sending upstream as well. Note that the
client's own SPS/PPS still reach the host as packed headers (ticket 02's patch), so a truncated parse
should not affect the encoded stream.

## The instrumentation round's outcome, and the attempted fix (2026-10-06)

Markers built into the image (the `src`-override route, committed as a note in this ticket) plus the
host-side reproduction produced the following, in order:

1. **Where the first stall is, confirmed in-guest.** Markers on the packed-header handler, its outer
   scan loop, `vl_rbsp_init`, and the SPS/PPS parsers show the CQP arm stopping *inside*
   `parseEncSpsParamsH264` for the first packed-header data buffer:

   ```
   CMARK encbuf type=22 / 27 / 23 / 25 / 26     (buffer dispatch: seq, misc, pic, packed-param, packed-data)
   CMARK hdr size=40
   CMARK iter 1
   CMARK rbsp type=7                            (SPS)  -- and then nothing
   ```

   No `CMARK sps done`: the SPS parse never returns, the outer loop has iterated once, and
   `vl_rbsp_init` returned. That matches the host-side measurements exactly (no `EXECBUFFER`, idle
   host, one `rt_sigsuspend`).

2. **A genuine, reproducible bug found and proven host-side** (`notes/08-vl-rbsp-ue-spin.c`):
   `vl_rbsp_ue()` in `src/util/vl_rbsp.h` has no end-of-NAL check, so once the packed-header RBSP is
   exhausted the reader consumes zeros forever. In a release build (`-DNDEBUG`, as mesa ships) the
   call does not return at all; in a debug build the `vl_vlc_get_uimsbf` assertion fires. The same
   shape exists three times in `parseEncSliceParamsH264` as unbounded `while (true)` loops over
   client bitstream values (`modification_of_pic_nums_idc == 3`, `memory_management_control_operation
   == 0`), each writing into a fixed 32-entry array.

3. **The attempted fix, and why it is not wired.** `nix/pkgs/patches/mesa-virgl-rbsp-bounds.patch`
   (kept in-tree, unwired) adds an RBSP-exhaustion check to `vl_rbsp_ue`, to those three
   `while (true)` loops, and to the packed-header handler's outer scan (whose
   `vl_vlc_bits_left()` underflows to a huge unsigned once the buffer is exhausted). With it the
   markers show the parse **progressing further** - `sps done`, `pps done`, the SEI buffer, and the
   IDR slice (`CMARK iter 4 / rbsp type=5`) - but the arm still hangs, and, decisively, the
   **previously-working bitrate arm hangs too**. A fix that breaks the working case cannot be wired,
   so it is unwired again (image rebuilt clean and the bitrate arm re-verified) and recorded here for
   the next attempt.

Next step, precisely bounded: instrument `parseEncSliceParamsH264` (entry, each of its three loops,
and the return) *with* the RBSP bounds applied, and log the loop counters without a cap - the hang
that remains is either one of those loops or a consequence of the patched parse leaving a parameter
set the host cannot encode. The regression on the bitrate arm should be bisected the same way (it is
either the `vl_rbsp_ue` bound changing a legitimate value, or the outer-scan bound truncating a
buffer that the client legitimately delimits further on).

## With the bounds wired, the hang moves to the host's vaRenderPicture (2026-10-06)

Markers in `parseEncSliceParamsH264` plus the RBSP bounds show the guest side is **uncorked**:

```
CMARK slice first_mb=0
CMARK slice done          <- the slice parse returns, frame after frame
```

No `l0/l1/marking loop enter` - those loops never run for this content - and the SPS/PPS/SEI buffers
all complete. So the RBSP bounds genuinely fix the guest-side stall.

The hang then reappears *one layer up*, on the **host**. Running the same arm with the host-side libva
interposer (patched into cang's VM worker, watchdog armed at 20 s):

```
WATCHDOG fired during vaRenderPicture
  ... libc epoll_wait ... cang(+0x3d110f) ...
BEGIN=31  END=30  RENDER_in=193  RENDER_ret=5
```

The host's `vaRenderPicture` never returns, on an early frame, with the guest having submitted
normally. That is a *different* defect from the one this ticket started with, and it is where the
remaining CQP hang lives.

Why the host would block once the guest parses differently: with the bounds, the guest's parse bails
out early (that is what "stop at the end of the RBSP" means), so the parameter structs the guest's
driver sends over the wire are the truncated/default ones - and the host's real VA driver then blocks
on that inconsistent parameter set rather than encoding. That also explains the bitrate arm's
regression: it is the same truncated-parameter path.

So the next step is not "bound the parse" but "parse the buffer correctly": the packed-header data
buffer is the client's own SPS+PPS, 40 bytes, and mesa's `packed_header_emulation_bytes` setting
decides whether `vl_rbsp_init` strips emulation-prevention bytes before the field parse. With
emulation bytes handled (`emu=1`) the stalling SPS decodes as an *HRD* parameter set
(`nal_hrd=1, cpb_cnt_minus1=9`) while the working arms do not - so the question to answer next is
whether `context->packed_header_emulation_bytes` is being set the way the client's
`VAEncPackedHeaderParameterBuffer` asks for, and whether the parse is running off the end because of
it. That is a small, checkable hypothesis, and it points at the parse rather than at the bounds.

### The client's own packed-header parameter buffers (2026-10-06)

Decoding the type-25 (`VAEncPackedHeaderParameterBuffer`) buffers captured for each arm settles what the
*client* asks for:

| arm | packed header | bit_length | bytes | has_emulation_bytes |
| --- | --- | --- | --- | --- |
| all arms | SPS (type 1) | 320 | 40 | 1 |
| all arms | SEI (type 4) | 1200 | 150 | 1 |
| all arms | PPS (type 3) | 64 | 8 | 1 |

So `has_emulation_bytes = 1` is what ffmpeg asks for and what mesa honours, for every arm including the
working ones - the flag is not the differentiator. What the table *does* show is that the *SPS* data
buffer is 40 bytes while the SPS NAL itself is 27: the buffer contains the SPS **plus** the PPS that
follows it (`00 00 00 01 67 …` then `00 00 00 01 68 …`). mesa's handler parses the whole buffer per NAL,
and the SPS parse reads past the end of its own NAL into the following PPS bytes - which yields the
nonsense sequence parameters (`nal_hrd = 1`, `cpb_cnt_minus1 = 9` in the emulation-byte walk) that the
host's VA driver then blocks on. With the bounds wired the parse at least returns; the fields it returns
are wrong, which is why the host hangs instead of encoding.

That makes the next step concrete: the SPS parse must not consume bits past the SPS NAL, which is what
`vl_rbsp_init()`'s `vl_vlc_limit()` call is supposed to arrange for a NAL-terminated RBSP
(`src/util/vl_rbsp.h`), and that limiting is the thing to check next.

### Measured: the NAL-boundary limiting is fine, so that is not the fix (2026-10-06)

`notes/08-rbsp-window.c` runs mesa's `vl_rbsp_init()` over the exact 40-byte packed-header data buffer
(SPS+PPS) the stalling arm sends and reports the resulting window:

```
emu=1  RBSP window: data-data=12  end-data=31  bits_left=208  removed=0 escaped=16
       profile=100 level=30 sps_id=0  chroma=1 bit_depth_luma_minus8=0 bit_depth_chroma_minus8=0
emu=0  RBSP window: data-data=8   end-data=40   bits_left=280  removed=0 escaped=0
```

With emulation-byte handling on - which is what the client asks for (`VAEncPackedHeaderParameterBuffer`:
bit_length 320, has_emulation_bytes 1, decoded above) - the RBSP ends at byte 31, i.e. at the start code
of the **following** PPS, and the SPS fields decode sanely (High profile, level 30, sps_id 0,
chroma_format_idc 1). So:

- mesa's limiting already stops the SPS parse at its NAL boundary; **"limit the RBSP at the NAL
  boundary" is not the fix**, and the truncated-parameter explanation of the host hang is refuted;
- the earlier `nal_hrd=1, cpb_cnt_minus1=9` reading came from my hand-written field walk, not from
  mesa's parser - it is an artifact and should not be used as evidence;
- what remains established is the unbounded `vl_rbsp_ue()` read (proven), the guest stall it causes
  (markers), and a *separate* host-side `vaRenderPicture` block that appears once the read is bounded.

The most likely reason the bounds patch regresses the previously-working arm is that both guards fire
during *valid* parses as well (the first at the end of every NAL, the second for a long-but-legal
`ue`), returning wrong field values. The next attempt should therefore bound the read *without*
changing values - e.g. stop the search loop and let the caller's own RBSP-end handling decide - and
must be validated against the working arm first, before the CQP arm.

### Both RBSP-bound variants regress the working arm (2026-10-06)

After the NAL-boundary hypothesis was refuted, a second, explicitly *value-preserving* variant was
tried: keep the loop, stop only when nothing is left at all - `invalid_bits >= 32 && data >= end &&
bytes_left == 0` (bytes exhausted *and* nothing buffered), in `vl_rbsp_ue`, in the slice parser's three
`while (true)` loops and in the packed-header handler's outer scan. It behaves like the first variant:
the CQP arm still hangs and the previously-working bitrate arm hangs too.

So a bound that reads as value-preserving still changes what the parse produces - which means the
divergence is not in *when* the loop stops but in what the parse reads on the way, and the next step
must be an image built with **both** the patch and markers (per-guard hit counts plus the field values
the parser writes), compared against the unpatched parse of the same buffer. Both patch variants stay
in-tree and unwired; `nix/pkgs/patches/mesa-virgl-rbsp-bounds.patch` currently holds variant 2.

### Third variant, same outcome - and what is now settled (2026-10-06)

Variant 3 added the two things the analysis pointed at: `vl_rbsp_ue()` **returns 0** when nothing is
left (instead of inventing a value out of consumed zero bits), and
`parseEncHrdParamsH264`'s `for (i = 0; i <= hrd_params->cpb_cnt_minus1; ++i)` is bounded by
`ARRAY_SIZE(hrd_params->bit_rate_value_minus1)` - that loop writes into **32-entry arrays** with an
unbounded, client-controlled `cpb_cnt_minus1`, so it is an out-of-bounds write for corrupt or truncated
input, and for a garbage `cpb_cnt_minus1` it is also effectively an infinite loop. The variant keeps the
guard in the three slice-parser loops and in the packed-header handler's outer scan.

Result: the CQP arm still stalls **and the previously-working bitrate arm stalls too** - the same
outcome as variants 1 and 2.

So the picture is:

1. the *first* defect is proven - `vl_rbsp_ue()` has no end-of-NAL check and spins forever on the
   client's packed header (`notes/08-vl-rbsp-ue-spin.c`, host-only reproduction);
2. bounding it (any of the three variants) **uncorks the guest** - markers reach `CMARK slice done`
   frame after frame - and then a **second, downstream stall** appears, on the host side: with the
   bound wired, the host-side libva interposer shows `WATCHDOG fired during vaRenderPicture` with
   `BEGIN=31 END=30` (frames are submitted, the host's driver then blocks);
3. the HRD-loop bound is worth keeping as a correctness fix regardless (out-of-bounds write), and is
   in the unwired patch;
4. mesa's NAL-boundary limiting is *fine* (`notes/08-rbsp-window.c`), so the earlier
   "truncated parameters" explanation of the host stall is withdrawn.

Nothing is wired. The guest VA encode remains usable with a bitrate cap; constant-QP High-profile
content hangs. The next session's first job is the second stall: with the RBSP bound wired, mark the
submission path (guest winsys → host front door → vrend → host VA) to see exactly which call blocks,
using the same shim/marker toolkit that found the first one.

## The submission-path round: the second stall is the *same* defect, one level up (2026-10-08)

Setup: cang's virglrenderer built with submission-path markers (stderr *and* a file log at
`virgl_renderer_submit_cmd`'s context lookup, `vrend_video_encode_bitstream`'s entry, and the line
immediately before `return virgl_video_encode_bitstream(cdc->codec, src->buffer, &desc);` - the hand-off
to the host VA driver), plus the mesa RBSP bound wired so the guest gets past its parse.

Two things came out of it:

1. **vrend hands the frame over and never returns from the call.** The markers show
   `VMARK encode_bitstream enter` and `VMARK handoff to host VA` once, for the first encode frame, and
   *nothing* after them for the rest of both arms. So the stall is inside the host-side
   `virgl_video_encode_bitstream` -> host VA encode, not between the guest submission and vrend.
   Corroborating: the guest now *submits* (past the RBSP parse) and polls the host with
   `waiting got error - 16` (EBUSY), creeping to frame 1-2 before the in-guest kill.
2. **On this host the host-side VA driver is the same code.** `/dev/dri/renderD128` here is virtio-gpu
   and its driver dir carries `virtio_gpu_drv_video.so -> ../libgallium-26.1.8.so`, i.e. the host's own
   VA encode goes through mesa's *virgl* VA driver - the same unbounded `vl_rbsp_ue()`. Measured
   host-side, with no VM involved: the host's own `ffmpeg -vaapi_device /dev/dri/renderD128` encode of
   the same clip **stalls with the system mesa** (rc=137, 0 bytes) and **completes with cang's patched
   mesa** (`LIBVA_DRIVERS_PATH=/nix/store/yd0f5gw...-mesa-26.1.8/lib/dri`: rc=0, 857 994 bytes,
   599 frames).

So the guest stall and the surviving host stall are **one defect in mesa**, met twice on a virtio-gpu
host. That also explains why bounding the read alone does not make the CQP arm produce a stream: the
bound uncorks the guest and the host then blocks in the very same parser.

### Why the host cannot simply be pointed at cang's patched mesa

The host-side VA encode runs in the **VM worker** (the render server is venus-only - it never maps
libva/libgallium, verified over 55 samples), and the VM worker is exec'd through
`unshare --keep-id` with a changed uid, so glibc is in secure-execution mode and libva's
`secure_getenv()` ignores `LIBVA_DRIVERS_PATH`. A run with the override set therefore still used the
**unpatched system mesa** and stalled (`m-qp.h264` and `m-br.h264` both 0 bytes, 0 frames). cang cannot
fix the host's VA driver by configuration; the host-side half has to come from the *host's* mesa.

### Where this leaves the fix

- cang's own patches remain the right shape for the **guest** half (raw headers over the wire, the
  upload fence, and - still unwired pending the review below - the RBSP bounds).
- the **host** half is upstream work, in mesa: bound the RBSP readers (`vl_rbsp_ue`/`vl_rbsp_se`) and
  the HRD loop (`parseEncHrdParamsH264`'s `for (i = 0; i <= cpb_cnt_minus1; ++i)` writes into 32-entry
  arrays with a client-controlled count - an out-of-bounds write for truncated input, and an
  effectively infinite loop for a garbage count). A host running any mesa VA driver (virtio-gpu,
  radeonsi, ...) is exposed to the same hang whenever a client hands vrend a packed header whose parse
  walks off the end.
- the remaining open question for shipping the bound in cang is the *bitrate arm*: with the bound wired
  it stalls too, and that regression is **not** explained yet (the host used there is the same unpatched
  system mesa, so a host-side cause is possible but unproven). Resolve it by re-running both arms with
  the host pointed at a patched mesa through some route the VM worker honours (its secure-exec mode
  rules out `LIBVA_DRIVERS_PATH`).

## Shipping the fix (2026-10-08)

The fix now ships for both sides of the vrend video path:

- **guest**: `nix/lib/systems.nix`'s `mesaVaApi` (the image's VA-API driver) applies
  `nix/pkgs/patches/mesa-virgl-rbsp-bounds.patch` alongside the raw-headers patch. The patch is written
  up as an upstream submission: it stops `vl_rbsp_ue()`'s exponential-Golomb scan at the end of the
  RBSP, bounds the three `while (true)` loops in `parseEncSliceParamsH264` and the packed-header
  handler's outer scan by the end of their buffers, and bounds
  `parseEncHrdParamsH264`'s `for (i = 0; i <= cpb_cnt_minus1; ++i)` by the 32-entry arrays it fills
  (an out-of-bounds write for a truncated stream, and an effectively infinite loop for a garbage
  count).
- **host**: `flake.nix` exports `overlays.default` (and `packages.<system>.mesa-rbsp-bounds`) applying
  the same override to `mesa`, so a downstream host can give its *own* VA-API driver the fix:
  `nixpkgs.overlays = [ cang.overlays.default ];`. Both patches are applied together so guest and host
  agree on the vrend video wire format. This route exists because the host cannot be redirected by
  configuration: the host-side VA encode runs in the VM worker, which `unshare --keep-id` puts into
  glibc secure-execution mode, where libva's `secure_getenv()` ignores `LIBVA_DRIVERS_PATH` (measured -
  a run with the override set still used the system mesa and stalled).
- `nix/lib/mesa-patched.nix` holds the single definition both consumers use.

Evidence the patch is safe for well-formed input: on the host, the same clip encodes with the patched
mesa (`rc=0`, 857 994 bytes, 599 frames) where the system mesa hangs (`rc=137`, 0 bytes); and the SPS
field decode of the captured buffers is bit-identical with and without the patch. The one open item
from the earlier rounds - the bitrate-capped guest arm stalling with the bound wired against an
*unpatched* host - is re-measured with this shipping configuration; until it is explained, that arm's
behaviour with the bound is the thing to watch.

### Prebuilt mesa for downstream hosts (2026-10-08)

Building mesa from source for the fix is a ~20-minute detour for every downstream host, so cang also
ships a prebuilt:

- `nix/lib/mesa-patched.nix` (the source override) now applies three patches: cang's
  `mesa-virgl-encode-raw-headers.patch` and `mesa-virgl-rbsp-bounds.patch`, plus
  `mesa-headless-virtio-modifiers.patch` copied from the sibling `headless` flake (the AMD virtio-gpu
  DMA-BUF modifier fix cang's host vrend needs on such a host - the same patch headless publishes
  prebuilt, so this keeps the two stacks aligned).
- `nix/lib/mesa-cang.nix` picks, per system, either cang's **prebuilt** mesa release asset
  (`nix/pkgs/mesa-prebuilt.nix`, which fetches the tarball and reconstructs it with
  `autoPatchelfHook`, modelled on headless's `mesa/prebuilt-package.nix`) or the source build with the
  same patches; `nix/pins.nix`'s `mesaPrebuiltRelease` is the pin the release workflow fills in, and
  while a platform is absent the source build is used, so nothing breaks before the first publish.
- both consumers go through it: the guest image's `mesaVaApi` and the host-facing
  `overlays.default` / `packages.mesa-rbsp-bounds`. `flake.nix` also exposes
  `packages.<system>.mesa-release-build` - the source build - which is what
  `.github/workflows/build-mesa.yml` builds, tars as `mesa-<version>-<system>.tar.gz`, attests and
  publishes, then updates the pin with `scripts/update-mesa-prebuilt.sh`.

Both branches are verified to evaluate: the fallback (source build, `rkw128ll...-mesa-26.1.8.drv`)
and the prebuilt branch (with a synthetic pin: `0rc47qa5...-mesa-26.1.8.drv`, whose fixed-output
derivation path is computable without fetching the asset).

## The fix IS verifiable in the nix dev shell (2026-10-08)

No VM is needed to verify that the patch cures the hang: on this machine the *host's own* VA-API
encode goes through the same mesa code, on the same render node the guest's frames are handed to.
Recipe (all local; the clip is any 640x360 H.264 file):

```sh
cd /home/dev/cang/cang
nix build .#mesa-rbsp-bounds -o /tmp/mesa-fix          # source build with cang's patches
FF=/nix/store/wbskc2agldpv2q9sqx7nqi57n201vzqy-ffmpeg-headless-8.1.2-bin/bin/ffmpeg
# system mesa (unpatched) - the hang:
timeout -k 5 180 $FF -nostdin -y -hide_banner -loglevel error \
  -vaapi_device /dev/dri/renderD128 -i clip360.mp4 -vf format=nv12,hwupload \
  -c:v h264_vaapi -qp 26 -bf 0 -f h264 /tmp/sys-qp26.h264
# cang's patched mesa - completes:
timeout -k 5 180 env LIBVA_DRIVERS_PATH=/tmp/mesa-fix/lib/dri:/run/opengl-driver/lib/dri $FF \
  -nostdin -y -hide_banner -loglevel error \
  -vaapi_device /dev/dri/renderD128 -i clip360.mp4 -vf format=nv12,hwupload \
  -c:v h264_vaapi -qp 26 -bf 0 -f h264 /tmp/pat-qp26.h264
```

Measured (2026-10-08, x86_64-linux, virtio-gpu render node):

| arm | driver | args | result |
| --- | --- | --- | --- |
| `sys-qp26-bf0` | system mesa | `-qp 26 -bf 0` | **rc=137, 0 bytes, 0 frames** - `frame= 0` for ~185 s, killed by `timeout -k 5` |
| `pat-qp26-bf0` | cang's patched mesa | `-qp 26 -bf 0` | **rc=0, 0.7 s, 857 994 bytes, 599 frames** |
| `pat-qp26-bf1` | cang's patched mesa | `-qp 26 -bf 1` | rc=0, 3.8 s, 599 frames |
| `sys-bitrate` | system mesa | `-b:v 200k -maxrate 250k -bufsize 500k` | rc=0, 2.6 s, 493 121 bytes, 599 frames |
| `pat-bitrate` | cang's patched mesa | same | rc=0, 2.7 s, **493 121 bytes - byte-identical size to the unpatched arm** |

Two conclusions beyond the cure itself:

1. the trigger is exactly **constant-QP with `-bf 0`** on the client side; a bitrate-capped encode does
   not reach the bug even with the unpatched driver, which is why bitrate-capped guest encodes always
   worked while `-qp` ones hung;
2. a well-formed encode produces the **same size** with and without the patch, so the bound does not
   change what a valid stream parses to - the earlier "the bound regresses the working arm" reading was
   the *host's* copy of the same bug, not a regression from the bound.

Still not verified anywhere: the complete cang guest path with **both** halves patched (guest driver
from the image + a host whose system mesa comes from `cang.overlays.default`); on this dev box the VM
worker necessarily loads the system mesa.

### Ground truth for that dev-shell A/B (2026-10-08)

The two drivers the arms actually loaded, from `/proc/<pid>/maps` (distinct inodes, so the A/B is not
two symlinks to one library):

- system arms: `/run/opengl-driver/lib/dri/virtio_gpu_drv_video.so` ->
  `dqdfhilmkqpijpa5jhmyqpjgh4mgpzlp-mesa-26.1.8/lib/libgallium-26.1.8.so` (no cang patches);
- patched arms: `k9zqmyiw5db2829s9ail53gn8g1h5vfx-mesa-26.1.8/lib/dri/...` -> that build's
  `libgallium-26.1.8.so`.

Full arm matrix from that run (`-k 5 180`, `timeout` SIGKILL after a SIGTERM the spinning ffmpeg never
acted on, hence rc=137):

| arm | driver | args | rc | secs | bytes |
| --- | --- | --- | --- | --- | --- |
| `sys-qp26-bf0` | system | `-qp 26 -bf 0` | 137 | 185.1 | 0 |
| `pat-qp26-bf0` | patched | `-qp 26 -bf 0` | 0 | 3.1 | 857 994 |
| `pat-fallback-qp26-bf0` | patched (`path:path`) | `-qp 26 -bf 0` | 0 | 0.7 | 857 994 |
| `sys-qp26-bf1` | system | `-qp 26 -bf 1` | 0 | 8.9 | 656 913 |
| `pat-qp26-bf1` | patched | `-qp 26 -bf 1` | 0 | 3.8 | 656 913 |
| `sys-bitrate` | system | `-b:v 200k -maxrate 250k -bufsize 500k` | 0 | 2.6 | 493 121 |
| `pat-bitrate` | patched | same | 0 | 2.7 | 493 121 |

Every non-empty output decodes to 599 frames, and the two pairs that complete on both drivers are
**byte-identical** (`sys-qp26-bf1` == `pat-qp26-bf1`, `sys-bitrate` == `pat-bitrate`) - so the patch
provably changes nothing for well-formed input.

Attribution caveat, stated because it matters for the upstream claim: that patched build carries
*both* cang mesa patches (raw headers + RBSP bounds), so this run proves "cang's patched mesa fixes
it", not "rbsp-bounds alone fixes it". The single-cause attribution comes from the standalone
reproducer (`notes/08-vl-rbsp-ue-spin.c`: the unpatched reader never returns) and from the earlier
image runs where the bound alone moved the guest from `CMARK rbsp type=7` to `CMARK slice done`.

## Shipped: the VM worker reaches cang's driver through a `dlopen` interposer (2026-10-09)

The host half is now proven end to end. cang interposes `dlopen` in its own
binary (`crates/cang/src/va_driver.rs`, exported to `.dynsym` by
`crates/cang/build.rs` with `-Wl,--export-dynamic-symbol=dlopen`) and rewrites
libva's `<name>_drv_video.so` request to `CANG_VA_DRIVER_PATH` (or the
package-relative `<prefix>/lib/cang/dri`); every other `dlopen` is forwarded to
libc through `dlsym(RTLD_NEXT, "dlopen")`. The VM worker *is* the cang binary,
so the executable's definition wins libva's global symbol lookup, and the
redirect depends on nothing the dynamic linker or `secure_getenv` reads.

### The worker really does ignore `LIBVA_DRIVERS_PATH` (measured)

Three `--gpu=drm` guest runs, one 640x360 clip (the first 10 s), the same
fully-patched guest driver (`qgghkr5k58i1azw0180vbc4c5nvdsxhi-mesa-26.1.8` from
`.#mesa-rbsp-bounds`), differing only in the host override. The worker's own
`/proc/<pid>/maps` is the ground truth; its `/proc/<pid>/status` is
`Uid: 165536 165536 165536 165536`, `CapEff: 000001ffffffffff`,
`NoNewPrivs: 0` - a full capability set on a mapped uid, a capability gain at
exec, which is what puts glibc in secure-execution mode.

| run | host override | worker `libgallium` | `-qp 26 -bf 1` | `-b:v 200k … -bf 1` | `-qp 26 -bf 0` |
| --- | --- | --- | --- | --- | --- |
| baseline | none | `dqdfhil…` (system) | ok, 386 079 B | ok, 251 395 B | **stall** rc 137, 0 B |
| `libva` | `LIBVA_DRIVERS_PATH=<qggh>/lib/dri` | `dqdfhil…` (system) | ok, 386 079 B | (poisoned by the stall) | **stall** rc 137, 0 B |
| interposer | `CANG_VA_DRIVER_PATH=<qggh>/lib/dri` | `qggh…` **and** `dqdfhil…` | ok, 386 079 B | ok, 251 395 B | **ok, 447 133 B** |

- the `libva` run loads the *system* mesa and stalls exactly like the baseline,
  so the variable is genuinely ignored by the worker, while the same value under
  cang's own variable reaches the interposer;
- the worker still maps the system mesa's EGL/GBM (`libEGL_mesa.so`,
  `dri_gbm.so`): only the `*_drv_video.so` request is rewritten, so the
  VA-encode driver is patched and the GL surface path is untouched;
- the two arms that already worked are **byte-identical** with and without the
  override (`md5sum` equal), so the redirect changes nothing for well-formed
  input.

### The CQP arm now matches the host control

Host control on the real render node, same clip, no VM:

| arm | system mesa | cang's patched mesa |
| --- | --- | --- |
| `-qp 26 -bf 1` | ok, 344 965 B | ok, 344 965 B (identical) |
| `-b:v 200k -maxrate 250k -bufsize 500k -bf 1` | ok, 234 592 B | ok, 234 592 B (identical) |
| `-qp 26 -bf 0` | **stall** rc 137, 0 B, 185 s | **ok, 447 145 B** |

With both halves patched the guest's `-qp 26 -bf 0` is **447 133 B, 300 frames,
y-PSNR 43.198433** - the same PSNR/SSIM to six decimals as the patched host
control's **447 145 B** (12 bytes apart, the encoder's SEI string). The `-bf 1`
arms carry real B-frames (`B=147 I=2 I,=1 P=150`; y-PSNR 43.30 guest vs 43.06
host); `-bf 0` is `I=2 I,=1 P=297`, as expected. The `qp26bf0-unpatched`
contrast arm (guest on the prebuilt `fdw09…` mesa, host patched) still stalls at
frame 0, so the guest bound remains load-bearing.

An extra `-b:v 2M -maxrate 2.5M -bufsize 5M -bf 1` arm in the interposer run
produces **2 285 511 B** in 1 s (`B=147 I=2 I,=1 P=150`, y-PSNR 51.87), so the
multi-MB stream with real B-frames the acceptance asked for is a guest output,
not just a host control.

### New hazard: a stalled arm poisons the rest of the VM

A stall is not cleaned up by killing the guest client - the worker stays stuck
in the host driver, so every later arm in the same VM stalls too. The first
baseline run stalled on `-qp 26 -bf 0` and its bitrate arm then also read 0 B;
re-running with the stalling arm last gave a clean `-b:v 200k` (251 395 B).
Order a stalling arm last, or start a new VM.

### Wired, and not

- **wired**: the interposer, the `CANG_VA_DRIVER_PATH` knob and the
  `<prefix>/lib/cang/dri` default are in cang (nix packaging unchanged);
- **not wired**: installing the patched driver into `$out/lib/cang/dri` so the
  default applies with no environment variable. A host that applies
  `cang.overlays.default` already has a patched system mesa and needs no redirect
  at all; `CANG_VA_DRIVER_PATH` is the escape hatch for a host that cannot, and
  is what this verification used. The upstream mesa submission (bound
  `vl_rbsp_ue`/`vl_rbsp_se`, the three slice-parser `while (true)` loops, the HRD
  loop) is still worth making.

Raw logs, worker maps and arm files for every run above are under
`/home/dev/cang/disk/nctx/t08/run-{baseline2,libva,patched,patched2}/`
(console.log, maps.sample, status.sample, g-*.h264) and the host control
under `t08/hostctl/`.

## Closed as an opt-in (2026-10-09)

Ticket 08 is closed. The maintainer's decision is that cang does **not** bundle
mesa. The host half ships as an opt-in redirect that is now defensive, covered by
a repository guard and documented, rather than an undocumented environment
variable.

### What is wired, what is opt-in

| half | how it ships | who needs it |
| --- | --- | --- |
| guest VA driver | image-local `mesaVaApi` (raw-headers + caps + RBSP bounds), linked at `/usr/lib/cang-va-runtime`, first in guest-init's `LIBVA_DRIVERS_PATH` | every `--gpu=drm` encode; already wired |
| host VA driver | `cang.overlays.default` / `packages.<system>.mesa-rbsp-bounds` patch the *system* mesa | a host that can apply an overlay; needs no redirect |
| host VA driver escape hatch | `CANG_VA_DRIVER_PATH=<mesa>/lib/dri` in cang's environment | a host that cannot change its mesa; no cang rebuild |

Nothing is bundled: cang's package installs no driver into
`<prefix>/lib/cang/dri` and carries no mesa in its closure (mesa's VA driver
closure is roughly 1 GiB).

### The exact host recipe

A host that can apply the overlay:

```nix
nixpkgs.overlays = [ cang.overlays.default ];
# or install cang.packages.<system>.mesa-rbsp-bounds as its mesa
```

A host that cannot:

```sh
CANG_VA_DRIVER_PATH=/path/to/patched/mesa/lib/dri cang --gpu=drm -- <command>
```

`LIBVA_DRIVERS_PATH` is not an alternative: the VM worker is exec'd through
`unshare --keep-id` (`--keep-caps`), so glibc is in secure-execution mode and
libva's `secure_getenv("LIBVA_DRIVERS_PATH")` ignores it. cang reads its own knob
with a plain `getenv` inside its `dlopen` interposer and rewrites only libva's
`<name>_drv_video.so` request; the value is inherited unchanged through
`cang` -> `unshare --keep-id` -> VM worker -> the sandboxed child that forks
libkrun. When the knob is unset the package-relative `<prefix>/lib/cang/dri` is
used instead.

### The redirect is defensive

`crates/cang/src/va_driver.rs` now refuses a candidate unless it both `dlopen`s
and exports a `__vaDriverInit_1_<minor>` entry point (libva probes
`__vaDriverInit_<major>_<minor>` down from its own version; the interposer probes
minors 0..=64). On refusal it forwards the caller's original path and prints one
line to stderr, so all four non-happy cases keep the system driver: the knob
unset, a configured directory without `<name>_drv_video.so`, a file that cannot be
loaded, and a valid ELF that is not a VA driver.

`crates/cang-repository-tests`'
`cang_va_driver_escape_hatch_is_named_forwarded_and_documented` pins the knob
name, `<prefix>/lib/cang/dri`, the `_drv_video.so` suffix, the `__vaDriverInit_`
prefix, the `.dynsym` export and the README/`docs/build.md` recipe together.
`README.md` and `docs/build.md` document the recipe and say cang does not bundle
mesa (roughly 1 GiB).

### Measured arms (2026-10-09)

Host: x86_64-linux, virtio-gpu `/dev/dri/renderD128`. System mesa
`dqdfhilmkqpijpa5jhmyqpjgh4mgpzlp-mesa-26.1.8` (no cang patch); override mesa
`qgghkr5k58i1azw0180vbc4c5nvdsxhi-mesa-26.1.8` (`.#mesa-rbsp-bounds`). cang under
test `/nix/store/hb3lm4hz3hwn6biyqldgnsml3yc8n16j-cang-0.11.2` (the worktree build
carrying the check). Guest image `image-04`
(`rcl1dhxd4jw3r304ags6c0lblnknmin1`, guest driver `qggh`), 10 s of the same
640x360 clip, `--gpu=drm --seccomp=off --landlock=off`, one VM per override kind
(the worker reads the knob once per process). Ground truth is the worker's own
`/proc/<pid>/maps`.

| cang env | worker `libgallium` from maps | `-qp 26 -bf 1` | `-b:v 200k -bf 1` | `-b:v 2M -bf 1` | `-qp 26 -bf 0` |
| --- | --- | --- | --- | --- | --- |
| (knob unset) | `dqdfhil...` only | ok, 386 079 B / 300 f | ok, 251 395 B | ok, 2 285 511 B | not run: stalls on the unpatched host |
| `CANG_VA_DRIVER_PATH=/tmp/t08-ovr-empty` | `dqdfhil...` only | ok, 386 079 B | ok, 251 395 B | ok, 2 285 511 B | not run |
| `...=/tmp/t08-ovr-broken` (33-byte text file) | `dqdfhil...` only | ok, 386 079 B | ok, 251 395 B | ok, 2 285 511 B | not run |
| `...=/tmp/t08-ovr-notva` (`libjpeg.so.62` copied over the driver name) | `dqdfhil...` only | ok, 386 079 B | ok, 251 395 B | ok, 2 285 511 B | not run |
| `...=qggh.../lib/dri` (positive control) | **`qggh...` + `dqdfhil...`** | ok, 386 079 B | ok, 251 395 B | ok, 2 285 511 B | **ok, 447 133 B / 300 f** |

The VM started and the encode completed through the system driver in every
fallback row; only the positive row loads the override driver. The two refusal
paths printed exactly one line each, from inside the worker:

```
cang: CANG_VA_DRIVER_PATH candidate /tmp/t08-ovr-broken/virtio_gpu_drv_video.so for /run/opengl-driver/lib/dri/virtio_gpu_drv_video.so cannot be loaded; falling back to the system VA driver
cang: CANG_VA_DRIVER_PATH candidate /tmp/t08-ovr-notva/virtio_gpu_drv_video.so for /run/opengl-driver/lib/dri/virtio_gpu_drv_video.so exports no VA driver entry point; falling back to the system VA driver
```

Env survival per launch path (same override `qggh.../lib/dri`, worker maps as
ground truth):

| launch path | worker `libgallium` from maps | override reached the worker |
| --- | --- | --- |
| default seccomp, `--landlock=off` | `qggh...` + `dqdfhil...` | yes |
| `--seccomp=off --landlock=off` | `qggh...` + `dqdfhil...` | yes |
| `--waypipe` (with `--seccomp=off --landlock=off`) | `qggh...` + `dqdfhil...` | yes |
| the `unshare --keep-id` exec | every run's helper is `unshare --user --mount --fork --kill-child --propagation private --map-users ... --map-groups ... --setuid 0 --setgid 0 --keep-caps .../cang internal libkrun-network-enter`; the override appearing in the worker shows it survived | yes |

The default-**landlock** (no `--landlock=off`) arm never reached the VA path at
all: the guest reported `No virgl contexts available on host` and `vaInitialize`
failed, so the default-seccomp row above keeps the harness's `--landlock=off` and
varies only seccomp. That is a landlock/GPU behaviour independent of this knob.

Raw logs: `t08/run-{fb-unset,fb-empty,fb-broken,fb-notva,pos-override2,waypipe2,normal-seccomp}/`
(console.log, maps.sample, g-*.h264) and the refusal lines under
`t08/fb-{broken2,notva2}-wrapper.log`. `image-04` was garbage-collected after
these runs, so the two refusal-line follow-ups ran the practice image; their
host-side maps and stderr lines are the evidence, and the clean encode rows above
are the `image-04` runs.
