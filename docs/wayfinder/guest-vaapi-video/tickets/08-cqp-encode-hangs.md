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
