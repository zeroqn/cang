---
label: wayfinder:map
title: Guest VA-API video through vrend (decode + encode)
---

## Destination

A `cang --gpu=drm` guest does hardware video the way the host does: `vainfo`
reports the host's profiles, a decoder client (`ffmpeg -hwaccel vaapi`, mpv
`--hwdec`) consumes them into VAAPI surfaces, and a VA-API encode produces a
stream any decoder accepts - all of it through the vrend video path, with the
wiring owned by cang, libkrun's fork and the rutabaga_gfx fork, and no user
environment setup.

**Status (2026-10-01): decode works, encode fixed and verified.** Decode
was enabled by carrying cang's `VIRGL_RENDERER_USE_VIDEO` through the rutabaga_gfx
fork and libkrun's fork plus a guest-init `LIBVA_DRIVERS_PATH` (ticket 01).
Encode was root-caused and fixed by ticket 02: the guest's coded-buffer read-back
is faithful, and the stream the host hands over was already broken because cang's
vrend submitted no packed parameter sets and a zeroed H.264 sequence parameter
buffer. cang now carries one patch per side of that wire extension (host:
`.#virglrenderer`; guest: the image-local `cang-va-runtime` VA driver) and a
`--gpu=drm` guest's H.264 and HEVC encodes software-decode cleanly. The encoder
entrypoint is still advertised while every encoder-attribute query in the guest
is 0, and the first ~5 frames of a stream still encode a surface that does not
hold the frame.

## Notes

- Domain: the `--gpu=drm` vrend video path - cang's virgl flags word
  (`crates/cang/src/runtime/vm/libkrun/launcher.rs`) to libkrun's `krun-devices`
  to virglrenderer 1.3.0's `vrend_video`/`virgl_video`, plus the guest's mesa
  `virtio_gpu_drv_video.so` and the host's libva on the render node.
- Skills worth consulting: `diagnosing-bugs`, `research`, `domain-modeling`.
- Evidence base: `docs/vaapi-video-investigation.md` (Resolution and the encode
  finding). The live-run logs and the hermetic podman store the probes used live
  outside the repo, on the host btrfs disk.
- Standing preferences: every claim comes from a live `--gpu=drm` guest running
  the packaged `nix build .#cang`; each probe carries a contrast that can fail (a
  host-side identical command, or a software decode/encode control); a decode
  probe proves the venus path too, because a render server that fails to start
  drops the whole backend to 2D and takes virgl, venus and video with it.
- Versions at the decode landing: virglrenderer 1.3.0 (`-Dvideo=true`), the
  image's mesa 26.1.8, libva 1.23.0, libkrun fork `2855f4d1`, rutabaga_gfx fork
  `d8479a1`, guest kernel 7.2.7-hardened1.

## Decisions so far

<!-- one line per closed ticket, gist plus link -->

- [Enable the vrend VA-API video path (decode)](tickets/01-enable-vrend-video-decode.md): the request died in rutabaga, which never carried `VIRGL_RENDERER_USE_VIDEO`, so `virgl_renderer_init` never enabled vrend's video path; the fork carries the bit, libkrun forwards it with `set_use_video`, guest-init exports `LIBVA_DRIVERS_PATH`, and the guest then reports the host's profiles and decodes into VAAPI surfaces.
- [Where the guest VA-API encode loses its bitstream](tickets/02-encode-coded-buffer-readback.md): not in the read-back - an interposed `vaMapBuffer` in the VM worker returns exactly the bytes the guest's `ffmpeg` writes. The host's own VA encode was the break: virglrenderer 1.3.0 submitted no packed parameter sets (`src/vrend/virgl_video.c:1440`) and left the H.264 sequence parameter buffer's geometry/level/flags zero (`src/vrend/virgl_video.c:1243`), over wire structs with no fields for them (`src/virgl_video_hw.h:150-171`). Fixed by the two patches described in the ticket's Resolution; verified by software-decoding a guest H.264 and HEVC encode.

## Not yet specified

- The ~5-frame warm-up in the host's input copy (`vrend_video.c:210/283`): the
  first frames are still encoded from a surface that does not hold the frame now
  that encoding itself works. Needs its own ticket.
- Whether the encoder entrypoint should stay advertised while every
  `PIPE_VIDEO_CAP_ENC_*` in the guest is 0 (the caps gap is item 2 of ticket 02).
- Client-level hardware video in the guest (mpv `--hwdec=auto` picking VA-API,
  Chromium `<video>` decode) - not measured; may deserve its own map.

## Out of scope

- DRM native context (guest-native RADV/radeonsi over amdgpu): ruled out for this
  host, whose only render node is itself a virtio-gpu node, so virglrenderer's
  native renderer cannot initialise and libkrun strips the DRM capset - see
  `docs/gpu-native-context-investigation.md`. This map is the vrend video path
  only.
- VA-API in `--waypipe --software-renderer` mode: llvmpipe ships no VA-API
  driver, so there is nothing to enable there.
