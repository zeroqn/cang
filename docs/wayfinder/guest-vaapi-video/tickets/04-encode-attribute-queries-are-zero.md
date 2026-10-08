> **2026-10-04 - the caps regression verdict is also void as to the driver:** the 1.5x bitrate regression that
> justified leaving the caps patches unwired was measured with the guest running the pinned prebuilt mesa (the
> image had no `/usr/lib/cang-va-runtime`), i.e. an unpatched guest driver against a patched host. Re-measure
> before relying on it. See ticket 08.

---
label: wayfinder:research
title: The guest's encoder attribute queries are all zero (and cost B-frames)
status: closed
blocked_by: []
claimed_by:
---

## Resolution (2026-10-09) - both bugs fixed

1. **Encoder attributes reach the guest** (caps pair wired) and **the reference/DPB state crosses
   the wire** (rewritten `virglrenderer-encode-reference-frames.patch`): the guest emits real
   B-frames (`B147 P150 I3`) and matches the host control at matched QP - 43.302 dB y vs 43.058,
   where it was 10.818 dB and I/P-only before.
2. **Guest rate control works** (below): the host's VA config is created from the client's own
   `rate_ctrl_method` instead of always `VAConfigAttribRTFormat`, so `-b:v 200k` and `-b:v 2M` now
   produce different streams that each track their target (1.07x / 1.04x the host's size, slightly
   better PSNR), while `-qp 26` stays byte-identical to the pre-fix output.

The residual 3.5-7% size difference is a rate-control-efficiency difference at equal nominal rate,
not a defect. Ticket closed; the measurements are in the sections above.

## Question

`vainfo` in the guest advertises `VAEntrypointEncSlice` for H.264/HEVC, but every
encoder-attribute query behind it answers 0, so a client picks its encoder
settings blind. Is there a route for the host's answers to reach the guest, and
does anything in the guest actually depend on them?

## Evidence already gathered

- `mesa-26.1.8/src/gallium/drivers/virgl/virgl_screen.c:139-159`
  (`virgl_get_video_param`) implements ten decode caps and `default: return 0`;
  there is no `PIPE_VIDEO_CAP_ENC_*` case at all.
- The virgl caps wire structure carries no encoder-attribute fields, so the host's
  answers cannot be forwarded even though the host's libva has them.
- It is not only a log line: it costs B-frames. ffmpeg's VAAPI encoder decides
  its GOP from `vaGetConfigAttributes(VAConfigAttribEncMaxRefFrames)`
  (`libavcodec/vaapi_encode.c:1638-1665`, `ref_l0 = value & 0xffff`,
  `ref_l1 = value >> 16`), mesa's VA frontend answers that attribute from
  `PIPE_VIDEO_CAP_ENC_MAX_REFERENCES_PER_FRAME` and falls back to `value = 1`
  when the cap is `<= 0` (`src/gallium/frontends/va/config.c:320-329`, i.e. past
  references only, no future references), and virgl answers every encoder cap
  with 0 - so the guest gets `ref_l0 = 1, ref_l1 = 0` while the host gets
  `1 / 1`. The two runs say it outright:

  ```
  guest: [h264_vaapi] Using intra and P-frames (supported references: 1 / 0).
  host:  [h264_vaapi] Using intra, P- and B-frames (supported references: 1 / 1).
  ```

  The guest's stream is therefore `type:I`/`type:P` where the host control emits
  `type:B` (measured both before and after ticket 03's fence, which changes
  nothing about it), i.e. the silent cost is B-frames' bitrate/quality win.
- Encoding itself works (ticket 02), so this is a completeness gap rather than a
  blocker - but a gap with a measurable price, not just a missing log line.
- Entrypoints are unaffected: they come from the host's profile/entrypoint table
  (`virglrenderer-1.3.0/src/vrend/virgl_video.c` `virgl_video_fill_caps`), which
  is why `EncSlice` is listed while the attributes are empty.

## Deliverable

- The encoder attributes carried over the virgl caps, which is the natural place
  for them: the caps are already a per-(profile, entrypoint) array
  (`virgl_video_caps` in `src/virgl_video_hw.h`, filled by
  `virgl_video_fill_caps`, `src/vrend/virgl_video.c:692-730`), so the host can
  fill `max_references_per_frame` (and whatever else the VA frontend consumes)
  from `vaGetConfigAttributes` per entry and the guest's `virgl_get_video_param`
  can return it for `PIPE_VIDEO_CAP_ENC_*` - the same two-sided patch shape as
  ticket 02, in the host-to-guest direction, with no synchronous query needed.
- Verification that B-frames then actually work end to end, and the size of the
  win: the acceptance evidence is the guest's own log line switching to
  "intra, P- and B-frames" plus a same-quality bitrate comparison against the
  host control. If the loss of B-frames turns out to be deeper than the
  attribute (for instance the wire's DPB/`ref_pic_list` handling cannot express
  a B-frame's references), that belongs in its own ticket.
- Whatever is not carried stays documented, with the reason.

## First attempt, 2026-10-01: the attribute is not the whole loss

The caps extension was built and tested (host `virglrenderer-encode-caps.patch`:
fills `max_past_references` / `max_future_references` per caps entry from
`vaGetConfigAttributes(VAConfigAttribEncMaxRefFrames)`; guest
`mesa-virgl-encode-caps.patch`: returns them from
`PIPE_VIDEO_CAP_ENC_MAX_REFERENCES_PER_FRAME`, both in the `reserved:20` space of
`struct virgl_video_caps` so the struct size does not change). The guest's
encoder immediately agreed: `Using intra, P- and B-frames (supported references:
1 / 1)` - but the stream got *worse*, not better:

| 10-frame testsrc 640x360, QP 20 | sizes | file |
| --- | --- | --- |
| before the caps change | `287, 18, 18, 18, 18, 14227, 3064, ...` | 26 517 B |
| after (B-frames enabled) | `14531, 3896, 14144, 13916, 13881, 13959, 13885, ...` | 129 821 B |
| host control (native, same command) | `31, 8, 162, 5030, 828, 193, ...` | 8 723 B |

Still decodable (exit 0) and the guest's VA decode is unaffected, but ~5x the
host's size and ~1.5x the guest's *previous* size: the client now codes
B-frames, and the price it pays is that a B-frame's references do not survive
the trip. The picture type crosses the wire, the reference *lists* a B-frame
needs (`ref_pic_list0/1`, the DPB state, the reordering) do not - so the host's
encoder codes each frame almost as if it were intra.

So the fix is two-layered: this ticket's attribute forwarding is the *enabler*
(kept, but not shippable on its own), and the reference/DPB plumbing is the part
that makes it pay - a follow-up of the same kind as ticket 02, to be found by
diffing what the host's VA submission carries for a B frame against what vrend
submits (the method that found the packed headers). Until that lands, the
attribute should not be advertised: forwarding it alone makes a guest encode
1.5x bigger.

## First implementation attempt (2026-10-03): caps land, the DPB fill hangs the encoder

The reverted caps patches were recovered from scratch and a third patch written for the piece the
caps alone cannot deliver: vrend leaves every `VAEncPictureParameterBufferH264.ReferenceFrames`
entry invalid, so a driver has no DPB to mark and a B-frame's future reference cannot be predicted
from - the reason a caps-only build made streams *larger* instead of smaller. The new host patch
(`nix/pkgs/patches/virglrenderer-encode-reference-frames.patch`) tracks each ring slot's
picture-order count (`ref_pic_poc[32]`, set when that frame is encoded) and fills `ReferenceFrames`
from the client's own `ref_idx_l0_list`/`ref_idx_l1_list` plus the active counts.

Measured, guest `--gpu=drm`, 6 frames of testsrc 320x240 (45 s cap per arm):

| build | `-bf 0` | `-bf 2` |
| --- | --- | --- |
| caps + reference-frames | **hangs** (rc=124; a 20 s 640x360 encode never finished either) | - |
| caps only | 4659 B in 2 s, rc=0 | 16895 B in 1 s, rc=0 |

(2026-10-04: the parenthetical "a 20 s 640x360 encode never finished either" in the first row is
confounded with the host-side `-bf 0` hang documented in the next section and is *not* evidence
against the DPB patch; the testsrc `-bf 0` hang in the same row is not confounded - testsrc with
`-bf 0` completes on the host - and remains attributable to the patch.)

So the caps forwarding is sound (and the encoder stays healthy), while the DPB fill as written
stalls the encoder outright - even for `-bf 0`, which never asks for a future reference. The
patch is therefore **not wired** in `nix/lib/systems.nix` (it stays in-tree, unwired, with the
reason recorded here), and the caps patch is.

Next attempt, in order: (1) fill `ReferenceFrames` only for references whose POC vrend actually
recorded (a `known` flag per ring slot) and never with a stale/zero POC; (2) set `frame_idx`
alongside `picture_id`, since VA's DPB entry carries both; (3) if the stall persists, check
whether the *guest* client's `ref_idx_*_list` values are frame numbers or surface ids - vrend
treats them as frame numbers (`get_enc_ref_pic(codec, frame_num)`), and a mismatch there would
create surfaces per reference and confuse the driver's DPB.

Measurement still to take once a non-stalling DPB exists: the real-content bitrate against the
host control on the same command (`~/cang` clip, 640x360, QP 26) - the host's own numbers are
1 671 174 B at `-bf 0` and 1 123 666 B with B-frames, i.e. the 33% prize this ticket is after.

## Second measurement round (2026-10-03): the hangs, and which patch causes them

All verification was moved into bounded probes: 6 frames of testsrc 320x240 (`tiny`) versus the 20 s
640x360 clip (`real`), 45-240 s caps per arm, one `PROBE` line per arm so a stall still yields data,
and the whole run delegated to a sub-agent so the parent stays responsive.

| build | tiny `-bf 0` / `-bf 2` | real `-bf 0` |
| --- | --- | --- |
| caps + DPB (`virglrenderer-encode-reference-frames.patch`) | **hangs** | - |
| caps only | 4659 B / 16895 B, rc=0, 2 s / 1 s | **hangs at `frame= 0`** |
| **no caps, no DPB (old cang) + the same new image** | - | **hangs at `frame= 0`** |

**Retraction (2026-10-04): the trigger is `-bf 0` on the host, not the patches.** The three rows above
were all produced by `probe-bf5.sh`, which passes `-bf 0`. The same 640x360 clip encoded by a plain
host `ffmpeg`, with no cang involved at all, hangs identically: `-vaapi_device /dev/dri/renderD128 -i
clip360.mp4 -vf format=nv12,hwupload -c:v h264_vaapi -qp 26 -bf 0` never returns - 0 bytes, `frame= 0`
throughout, the process spins CPU, SIGTERM at 120 s is ignored and only SIGKILL ends it. Isolation on
the same clip: no flag -> rc=0, 13 430 B; `-qp 26` -> rc=0, 7 542 B; `-bf 1` -> rc=0, 13 430 B;
**`-bf 0` -> hangs**; `-bf 0` with a lavfi `testsrc2` source -> rc=0; `-bf 0` with a raw nv12 input ->
rc=0. The hang therefore needs `-bf 0` *and* a decoded (mp4) input, and it reproduces without cang.

That also matters for what "host" means here: `/dev/dri/renderD128` on this machine is virtio-gpu
(`vendor 0x1af4 device 0x1050`), i.e. the machine is itself a VM whose own VA encode runs through
mesa's virgl VA driver. The row "no caps, no DPB (old cang) + the same new image" and the row "caps
only hangs on real content" are both the host's `-bf 0` hang seen end-to-end through the guest - the
guest arms added nothing to it. **The claim that the guest-side caps half is implicated is withdrawn**;
the guest arms must be re-measured with `-bf 1` (`probe-bf7.sh`). The `-bf`-free design objection
below - a patched peer reading `reserved` bits of an unpatched one - still stands on its own.

**State after this round:** all three patches (host caps, guest caps, host DPB) are unwired in
`nix/lib/systems.nix` (the caps+DPB wiring is on hold until the `-bf`-free re-measurement settles
whether the forwarding is sound); cang and the image are being rebuilt
without them and the real-content encode re-verified, so the repository returns to the known-good
baseline. The B-frame work needs a different shape: the caps must be negotiated so an unpatched peer
cannot misread them (a new caps field with a version/flag, or the value carried in the existing
`reserved` bits only when a feature bit says so), and the DPB fill must use only picture-order counts
vrend actually recorded, with `frame_idx` set alongside `picture_id`.

## Re-measurement with both caps halves wired (2026-10-08)

Both halves were wired (`mesa-virgl-encode-caps.patch` for the guest's `mesaVaApi`,
`virglrenderer-encode-caps.patch` for the host's vrend), the image and cang rebuilt, the image loaded,
and the arms run on the same 640x360 clip (first 10 s, 300 frames) against a host control. The tree was
reverted and the practice image reloaded afterwards.

**The forwarding itself works.** The guest's ffmpeg now logs `supported references: 1 / 1` (it logged
`1 / 0` before), so the attributes genuinely arrive from the host's `vaGetConfigAttributes` for the
per-(profile, entrypoint) caps array.

**But enabling it is a net regression**, exactly as the ticket predicted:

| arm | where | bytes | frames | PSNR y/u/v | SSIM All |
| --- | --- | --- | --- | --- | --- |
| guest caps, `-b:v 200k` | guest | 2 166 837 | 300 | **10.818 / 6.227 / 6.739** | 0.621 |
| guest caps, `-b:v 2M` | guest | 2 166 834 | 300 | 10.818 / 6.227 / 6.739 | 0.621 |
| guest caps, `-qp 26 -bf 1` | guest | 2 163 174 | 300 | (same) | (same) |
| guest, *no* caps (practice image) | guest | 450 772 | 300 | 43.198 / 48.735 / 48.173 | 0.982 |
| host control, `-b:v 200k` | host | 234 592 | 300 | 40.8 / … | … |
| host control, `-b:v 2M` | host | 2 207 702 | 300 | 51.5 / … | … |
| host control, `-qp 26 -bf 1` | host | 344 965 | 300 | 43.06 / … | … |

Per-frame PSNR on the caps arm reads 45.3 / 12.4 / 44.9 / 10.2 / 12.4 and then ~10 dB onward: the
stream is decodable but its pictures are wrong after the first few, and it is ~5x the size of the
pre-change guest output at 32 dB worse luma. Rate control is still ignored (`-b:v` 200k and 2M produce
the same size and the same PSNR), and the pictures are still I/P only.

**Verdict: do not wire the caps. The blocker is the reference plumbing, not the attribute query.**
A B-frame needs `ref_pic_list0/1` and the DPB/reorder state to cross the wire, and `vrend`'s fill still
(a) leaves `RefPicList0`/`RefPicList1` commented out, (b) invents `ReferenceFrames` from its own
`frame_num % 32` ring, and (c) sets `param->CurrPic.picture_id` from `get_enc_ref_pic()` while
discarding `source` outright - the surface the upload actually wrote into. Until those are filled from
the desc, advertising the attribute only makes the client build a GOP the host cannot honour.

Two side-findings for whoever picks this up:

- the guest **ignores rate control** in every configuration measured so far (`-b:v` 200k/400k/2M give
  one size; `-rc_mode CBR` and `-qp … -bf 0` hang). The caps forwarding did not change that, so the
  rate-control parameters are lost somewhere other than the attribute query - a second, independent
  bug to chase after the reference plumbing.
- `-qp` with `-bf 0` still hangs, but that is the RBSP defect (ticket 08), not this one; `-bf 1` arms
  complete on both drivers.

### Build detail for that re-measurement (2026-10-08)

- caps image: `ky3kd0sv98dypmghiz34mcfwahmdbch8-cang.tar.gz`; its guest mesa `qgghkr5k58i1azw0180vbc4c5nvdsxhi`
  (its drv references `mesa-virgl-encode-caps.patch`), in-guest driver md5 `e5694b8a968d75d865bd14caeb0d5f36`
  (the practice image's driver is md5 `7bf2ab7bc2d477e1c617d5fd41406ebc`).
- caps cang: `nlhszk53zbfg8bk7sd0yw1jy1bra3mnj-cang-0.11.2`, whose render server is
  `msq2sary7qw9n0yc4mds5s29al6q85pd-virglrenderer-1.3.0` (references `virglrenderer-encode-caps.patch`).
  The first `.#cang` build hit an unrelated rustc-1.95 ICE while compiling vendored `unicode-ident`; the
  retry was clean.
- with the caps wired, `-qp 26 -bf 1` **also stopped hanging** (rc=0, ~2 s) - so the caps change which
  client configuration reaches the host, and the earlier `-qp` hang of that arm is not merely the
  ticket-08 RBSP defect. The stream is still corrupt and still I/P only, i.e. advertising the
  attributes without the reference plumbing buys a wrong stream rather than a better one.

## Reference/DPB fix: the B-frame stream is correct and matches the host (2026-10-08)

The DPB fill was re-implemented and, with it, the caps are now shippable. The
patch is `nix/pkgs/patches/virglrenderer-encode-reference-frames.patch` (rewritten)
and it is **wired** together with `virglrenderer-encode-caps.patch` (host) and
`mesa-virgl-encode-caps.patch` (guest, in `nix/lib/mesa-patched.nix`).

What the new patch does in `h264_fill_enc_picture_param()`:

- `CurrPic.frame_idx` and `CurrPic.flags` are set (not left commented out);
- the picture-order count is recorded only for **reference** pictures, so a
  non-reference B-frame (which carries the frame number of the following
  reference) can no longer overwrite its entry;
- `ReferenceFrames[]` is filled from the client's own `ref_idx_l0_list` /
  `ref_idx_l1_list` (bounded by the active counts and by the 16-entry array),
  with `picture_id` from `get_enc_ref_pic()`, `frame_idx`, the recorded POC and
  `VA_PICTURE_H264_{SHORT,LONG}_TERM_REFERENCE` from `l0/l1_is_long_term`.

The earlier "the DPB fill hangs a 6-frame testsrc `-bf 0` encode" verdict was a
measurement artifact: the tiny probe decoded a *file* (`tiny-src.mkv`), i.e. the
`-bf 0` + decoded-input host hang of `notes/encode-measurement-hazards.md` §1.
Re-measured with `-bf 1`, the patch completes and fixes the stream.

### Arms: 640x360 clip, first 10 s (300 frames), `-bf 1`, guest image `image-04`

Guest (`--gpu=drm`, guest driver md5 `e5694b8a...`, `supported references: 1 / 1`):

| arm | rc | bytes | frames | PSNR y/u/v | SSIM All | ffprobe types |
| --- | --- | --- | --- | --- | --- | --- |
| `-b:v 200k -maxrate 250k -bufsize 500k` | 0 | 389 739 | 300 | 43.302 / 48.855 / 48.274 | 0.982030 (17.45) | B=147 P=150 I=3 |
| `-b:v 2M -maxrate 2.5M -bufsize 5M` | 0 | 389 742 | 300 | 43.302 / 48.855 / 48.274 | 0.982030 (17.45) | B=147 P=150 I=3 |
| `-qp 26` | 0 | 386 079 | 300 | 43.302 / 48.855 / 48.274 | 0.982030 (17.45) | B=147 P=150 I=3 |

Host control (native `ffmpeg` on cang's patched mesa, `mesa-ship`, `1 / 1`):

| arm | rc | bytes | frames | PSNR y/u/v | SSIM All | ffprobe types |
| --- | --- | --- | --- | --- | --- | --- |
| `-b:v 200k -maxrate 250k -bufsize 500k` | 0 | 234 592 | 300 | 40.835 / 47.903 / 47.058 | 0.973845 (15.82) | B=147 P=150 I=3 |
| `-b:v 2M -maxrate 2.5M -bufsize 5M` | 0 | 2 207 702 | 300 | 51.539 / 56.058 / 55.339 | 0.996459 (24.51) | B=147 P=150 I=3 |
| `-qp 26` | 0 | 344 965 | 300 | 43.058 / 48.799 / 48.208 | 0.981820 (17.40) | B=147 P=150 I=3 |

Acceptance: the guest stream decodes (`measure_rc=0`), the guest emits real
B pictures (147 of 300, same histogram as the host control), and at matched
quality it matches the host control - guest `-qp 26` 43.30 dB vs host `-qp 26`
43.06 dB (0.24 dB), 386 079 vs 344 965 bytes. Before the reference plumbing the
guest was 10.818 dB y and I/P only.

### Bug 2 is located: the VA config declares no rate-control mode

The second bug (all guest `-b:v` arms produce one stream) is *not* in the
guest-to-vrend description. A temporary `virgl_error` in
`h264_encode_bitstream()` dumped the wire desc for each arm:

```
br200k  rc0={method=4(VARIABLE) target=200000  peak=250000  vbv=500000}
br2M    rc0={method=4(VARIABLE) target=2000000 peak=2500000 vbv=5000000}
qp26    rc0={method=0(DISABLE)  target=0       peak=0       max_qp=51}
```

So the parameters arrive per arm; the loss is on the host side. Root cause:
`virgl_video_create_codec()` (`src/vrend/virgl_video.c`) calls
`vaCreateConfig(va_dpy, profile, entrypoint, &attr, 1, &cfg)` with **only**
`VAConfigAttribRTFormat`. Without `VAConfigAttribRateControl` the host VA driver
leaves the encode context's `rate_ctrl_method` at
`PIPE_H2645_ENC_RATE_CONTROL_METHOD_DISABLE`, and the encoder then ignores every
RC buffer vrend submits - so the guest encodes at a fixed QP regardless of `-b:v`.

Proof: adding `VAConfigAttribRateControl = VA_RC_VBR` to that `vaCreateConfig`
call (a throw-away test patch, **not wired**) made the guest honour the bitrate:

| guest arm, test config | bytes |
| --- | --- |
| `-b:v 200k -maxrate 250k -bufsize 500k` | 251 395 (host 234 592) |
| `-b:v 2M -maxrate 2.5M -bufsize 5M` | 2 285 511 (host 2 207 702) |
| `-qp 26` (CQP) | 25 854 (wrong: the config says VBR while the client sends CQP) |

So the correct fix is not a fixed mode but carrying the client's RC mode: the
guest's `virgl_video_create_codec_args` / wire cannot express it today, so the
next attempt should add it (or create the VA context per RC mode) before setting
`VAConfigAttribRateControl` from it. `virglrenderer-encode-rate-control.patch`
(the `target_percentage` reconstruction: the code multiplied where the wire
needs a divide) is a real but insufficient piece of this and is kept **unwired**.

### Build detail

- final cang `5g6wipw56qv2qahcc55ss3dxayv20i33-cang-0.11.2` (its render server
  `msrb0zmdimfp3azaq19insz3xldn99dj-virglrenderer-1.3.0`, drv referencing
  `virglrenderer-encode-reference-frames.patch` and `virglrenderer-encode-caps.patch`).
- image-04 `rcl1dhxd4jw3r304ags6c0lblnknmin1-cang.tar.gz`, guest mesa
  `qgghkr5k58i1azw0180vbc4c5nvdsxhi` (md5 `e5694b8a968d75d865bd14caeb0d5f36`).
- the practice image was reloaded afterwards (`localhost/cang:latest` =
  `1393b4cc6ad5...`, driver md5 `7bf2ab7bc2d477e1c617d5fd41406ebc`).


## Bug 2 fixed: the client's rate-control mode reaches the host VA config (2026-10-09)

The fix is **host-side only**; the guest image and the wire are unchanged. The
VA-API rate-control mode is a *config* attribute, so the host needs it when it
calls `vaCreateConfig()` - but vrend creates the codec at
`VIRGL_CCMD_CREATE_VIDEO_CODEC` and the mode only crosses the wire later, with
the first picture's `rate_ctrl_method`. Rather than grow the create-codec wire,
the encoder half of a codec is now created **lazily**, from the first picture:

- `virgl_video_create_codec()` still validates the RTFormat and creates the
  config/context up front for a decoder, but for `VAEntrypointEncSlice` it only
  records the profile/entrypoint/size and leaves `va_cfg`/`va_ctx` unset.
- `virgl_video_begin_frame()` uploads the picture and records the target but
  **defers `vaBeginPicture()`**.
- `virgl_video_encode_bitstream()` reads `rate_ctrl[0].rate_ctrl_method` from
  the picture description, (re)creates the config with
  `VAConfigAttribRateControl` mapped from it (`DISABLE`->`VA_RC_CQP`,
  `CONSTANT*`->`VA_RC_CBR`, `VARIABLE*`->`VA_RC_VBR`), then calls
  `vaBeginPicture()` and renders. A mid-stream mode change destroys and
  recreates the config + context + coded buffer (the VA surfaces in
  `ref_pic_list[]` belong to the codec, not the context, so they survive). If a
  driver rejects the mode (QVBR is the common one) it retries without the
  attribute, i.e. the old DISABLE behaviour.

Two patches are wired in `nix/lib/systems.nix`:

- `virglrenderer-encode-rate-control-config.patch` (new) - the lazy config
  described above.
- `virglrenderer-encode-rate-control.patch` (was unwired) - the
  `target_percentage` reconstruction (`target*100/peak`, not `target*peak/100`),
  which the host VA frontend multiplies back into the target bitrate for every
  non-CONSTANT method. The earlier throw-away VBR test was config-VBR **plus**
  this patch (its render server drv references both), which is why the VBR
  numbers below reproduce it exactly.

Measured on the same harness as the caps/DPB round: 640x360 `clip360.mp4`,
first 10 s (300 frames), `-bf 1`, image-04 guest (driver md5 `e5694b8a...`),
cang `3xdavg4y4424b4w85qzm1px0wz2i7mzh-cang-0.11.2` whose render server is
`a5yraawnl5b79bjpraf14nzqganrcj5x-virglrenderer-1.3.0` (drv references both
patches). Raw logs in `../notes/` are not committed; the run artefacts are
`/home/dev/cang/disk/nctx/rc2-run/` and the scratch harness is
`rc2-run.sh`.

| arm | where | bytes | frames | PSNR y/u/v | SSIM All | ratio vs host |
| --- | --- | --- | --- | --- | --- | --- |
| `-b:v 200k -maxrate 250k -bufsize 500k` | guest | 251 395 | 300 | 41.296 / 48.178 / 47.330 | 0.975905 | 1.072 |
| `-b:v 2M -maxrate 2.5M -bufsize 5M` | guest | 2 285 511 | 300 | 51.871 / 56.251 / 55.528 | 0.996606 | 1.035 |
| `-qp 26` | guest | 386 079 | 300 | 43.302 / 48.855 / 48.274 | 0.982030 | 1.119 |
| `-b:v 200k -maxrate 250k -bufsize 500k` | host control | 234 592 | 300 | 40.835 / 47.903 / 47.058 | 0.973845 | - |
| `-b:v 2M -maxrate 2.5M -bufsize 5M` | host control | 2 207 702 | 300 | 51.539 / 56.058 / 55.339 | 0.996459 | - |
| `-qp 26` | host control | 344 965 | 300 | 43.058 / 48.799 / 48.208 | 0.981820 | - |

Before this change both guest `-b:v` arms produced one stream (389 739 /
389 742 B); now 200k and 2M differ by ~9x and each tracks its target, landing
3.5-7% above the host control at +0.33/+0.46 dB y (the guest is slightly more
efficient at the same nominal rate). `-qp 26` is **byte- and quality-identical**
to the pre-fix guest (386 079 B, 43.302 dB y), i.e. the CQP arm is preserved -
which is exactly what the throw-away VBR-only config got wrong (25 854 B).

The decode path is untouched: a decoder still creates its config/context in
`virgl_video_create_codec()`, and `virgl_video_begin_frame()` still calls
`vaBeginPicture()` for it; the deferred path is guarded by
`codec->entrypoint == PIPE_VIDEO_ENTRYPOINT_ENCODE`.

Build detail:

- cang `3xdavg4y4424b4w85qzm1px0wz2i7mzh-cang-0.11.2`, render server
  `a5yraawnl5b79bjpraf14nzqganrcj5x-virglrenderer-1.3.0` (drv references
  `virglrenderer-encode-rate-control.patch` and
  `virglrenderer-encode-rate-control-config.patch`).
- guest image-04 (`rcl1dhxd4jw3r304ags6c0lblnknmin1-cang.tar.gz`, config
  `d261fe7f...`) unchanged - the fix needs no guest rebuild; the practice image
  (`1393b4cc...`) was reloaded afterwards.
