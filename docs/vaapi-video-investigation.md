# VA-API hardware video in the cang guest — investigation log

Status: **resolved**. `--gpu=drm` guests now get the host's hardware VA-API
profiles, decode with them, and encode decodable H.264/HEVC (verified live — see
Resolution). The Blocker 1/2
narrative further down is the pre-fix history, written in the libkrun `dlopen`
era (`dynamic.rs`, mentioned in Blocker 1, no longer exists: cang links libkrun's
Rust API now).

## Symptoms

- In-guest `vainfo --display drm --device /dev/dri/renderD128` loads Mesa 26.1.8's
  `virtio_gpu_drv_video.so` ("for virgl") but reports only
  `VAProfileNone : VAEntrypointVideoProc` — no H.264/HEVC/VP9/AV1 profiles.
- In-guest `mpv --hwdec=auto` falls back to software decode:
  `h264: Failed setup for format vaapi: hwaccel initialisation returned error`.
- Host virglrenderer is built with `-Dvideo=true -Dvenus=true` and links libva 2.23.0;
  guest Mesa contains virgl video symbols. Both sides are video-capable in principle.

## Resolution

Root cause: the request never reached the renderer. cang sets
`VIRGL_RENDERER_USE_VIDEO` (`1 << 11`) in its virtio-gpu virgl flags word, and
libkrun's GPU device code had no bit for it either — but the drop that mattered
was in rutabaga: `VirglRendererFlags` stopped at `VIRGLRENDERER_DRM` (`1 << 10`)
and `RutabagaBuilder` had no setter for the video bit, so `virgl_renderer_init`
never received it, vrend never called `virgl_video_init`/`vaInitialize`, and the
guest's virtio-gpu VA driver was told the device has no video caps — hence
`VAProfileNone` and nothing else. Blocker 1 is moot under the Rust-API binding.
Blocker 2 was the render-node fd: `get_drm_fd` hands out an `O_RDWR` fd, and
`vaInitialize` then succeeds inside the VM worker (its `libva:` and
`VA-API version:` lines appear in the worker log).

The fix has three parts:

- `zeroqn/rutabaga_gfx`, branch `cang` (`d8479a1`): add
  `VIRGLRENDERER_USE_VIDEO`, `VirglRendererFlags::use_video` and
  `RutabagaBuilder::set_use_video`.
- `zeroqn/libkrun`, branch `cang` (`2855f4d1`): pin rutabaga_gfx at that rev and
  forward cang's bit with `set_use_video`.
- cang guest-init: export
  `LIBVA_DRIVERS_PATH=/usr/lib/cang-va-runtime/dri:/usr/lib/cang-mesa-runtime/lib/dri`
  for `--gpu=drm`. libva's default search paths (`/run/opengl-driver/lib/dri`,
  `/usr/lib*/dri`) do not include the mesa runtime directory, so without it
  `va_openDriver()` finds no driver and `vaInitialize` fails before any of the
  above matters. The first directory is the image's own VA driver build (see the
  encode paragraph below); the second, the prebuilt mesa runtime, is the
  fallback.

Verified live (cang 0.11.2, libkrun v2.0.0-cang.5, libkrunfw 7.2.7-hardened1, a
`--gpu=drm` guest on a host whose render node is a NAVI33 AMD GPU):

- `vainfo` in the guest reports the host's profile set:
  `VAProfileH264ConstrainedBaseline`/`Main`/`High` (VLD + EncSlice),
  `VAProfileHEVCMain`/`Main10` (VLD + EncSlice), `VAProfileVP9Profile0`/`Profile2`
  (VLD), `VAProfileAV1Profile0` (VLD), `VAProfileJPEGBaseline` (VLD) — instead of
  `VAProfileNone` alone.
- `ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128` in the guest:
  `Reinit context ... pix_fmt: vaapi`, output stream `vaapi(...)`, 30 frames, 0
  decode errors; with `-hwaccel_output_format vaapi` the frames stay in VAAPI
  surfaces. Decode throughput is at parity with software decode in this nested VM
  (13 fps vs 11 fps on a 1080p clip) — what the flag buys is the codec path, not
  speed through three stacked virtio-gpu layers.
- No regression: venus Vulkan (`DRIVER_ID_MESA_VENUS`) and virgl GL 4.6 are
  unchanged. If the sandboxed render server fails to start, the whole backend
  falls back to 2D and the guest loses virgl, venus *and* video, so a video probe
  always proves the venus path too.
- **Encode works now.** It did not in the first cut: `vainfo` advertised
  `VAEntrypointEncSlice` for H.264/HEVC (the host's profiles), and
  `ffmpeg -c:v h264_vaapi`/`hevc_vaapi` through `-vaapi_device` ran to completion
  and wrote a file, but the file was not a bitstream — the whole 87510-byte
  output of a 30-frame 640x360 encode was one access unit ("missing picture in
  access unit with size 87510") that no decoder accepted. The coded-buffer
  read-back was the first suspicion and was **not** where it broke: interposing
  `vaMapBuffer` inside the VM worker showed cang's vrend receiving exactly the
  bytes the guest's `ffmpeg` writes. The break was one layer above: cang's vrend
  submitted no packed parameter sets at all (the source of a stream's
  SPS/PPS/SEI) and left the H.264 sequence parameter buffer's geometry, level and
  `log2` fields zero, so radeonsi handed back one parameter-set-less "NAL" per
  frame. Both halves are now fixed and carried in this repo:
  `virglrenderer-encode-raw-headers.patch` on the host side (in `.#virglrenderer`,
  so every cang package has it) and `mesa-virgl-encode-raw-headers.patch` on the
  guest side, which the image builds into its own VA driver
  (`/usr/lib/cang-va-runtime`) because the image's mesa is a prebuilt binary drop
  no patch can reach.
  Verified live 2026-10-01 in a `--gpu=drm` guest: a 10-frame 640x360 `testsrc`
  encode now starts with an SPS/PPS/SEI (H.264: `00 00 00 01 67 64 0c 1e ...`,
  `... 01 68 ...`, `... 01 06 ...`) and software-decodes with exit 0, HEVC starts
  with a VPS/SPS/PPS (`00 00 00 01 40 01 ...`, `... 42 01 ...`) and decodes
  cleanly too, and the guest's VA *decode* path is unaffected. One gap remains: no
  `PIPE_VIDEO_CAP_ENC_*` reaches the guest (the encoder is advertised while every
  encoder-attribute query is still 0). The opening frames used to come out black
  in every encoder context; that was a second host-side defect (the upload blits
  and the VA encode share memory with nothing ordering them, fixed by a
  `glFinish()` in the upload path - see ticket 03). Details and raw evidence:
  `docs/wayfinder/guest-vaapi-video/tickets/02-encode-coded-buffer-readback.md`
  and its `notes/02-encode-coded-buffer-evidence.md`.

Two probe traps worth remembering: a guest `ffmpeg` under the managed PTY stops
with `SIGTTOU` unless `-nostdin` is passed, which looks exactly like a hung
hardware decode; and a *debug* `cang` (`target/debug/cang`) cannot start the
render server because the render-server seccomp policy path resolves from the
binary's package prefix — set
`CANG_RENDER_SERVER_POLICY=<cang>/share/cang/seccomp/render-server.json` when
probing a non-package build.
### The encode's chroma planes (fixed 2026-10-02)

The guest's encode used to carry host-identical luma and chroma ~28-40 dB worse than the
host control's at every size, spending 3-9x the bits. Root cause, found with a host-side
reproducer (`docs/wayfinder/guest-vaapi-video/notes/repro-egl-import.c`, seconds per
iteration): vrend's VA video surface was allocated with the driver's default **tiled** DRM
modifier, while the plane-by-plane EGL import of its exported DMA-BUF carries no modifier
and is therefore linear - so GL wrote the picture into a linear view of a tiled buffer and
the encoder read the tiled layout. Only the chroma plane showed it, because plane 0 starts
at offset 0 under both layouts.

Fix: `nix/pkgs/patches/virglrenderer-linear-surface.patch` makes `virgl_video_create_buffer`
ask libva for `DRM_FORMAT_MOD_LINEAR` (with the old call as a fallback). With it, a live
`--gpu=drm` guest's encode matches the host's PSNR to six decimals at 176x144, 320x240,
640x480, 1280x720 and 1920x1080, bitrates within 12 bytes (the encoder-identification SEI),
and guest VA-API decode is unchanged. Details and the full table:
`docs/wayfinder/guest-vaapi-video/tickets/05-chroma-planes-wrong.md`.

Two instrument lessons from that hunt: `vaDeriveImage` is **not** a read of the surface (it
returned zeros for every write); `vaGetImage` is (it is a semantic copy-out in the driver's
own layout), and it is what the reproducer uses as its oracle. And a whole family of plausible
fixes (plane modifiers on the import, plane offsets, pitch, image width) is inert by
construction once the surface itself is what has to change.


## Blocker 1 — libva.so.2 undefined symbol `vaGetDisplayDRM` (FIXED)

`libva.so.2` calls `vaGetDisplayDRM` but only `libva-drm.so.2` defines it, and
`libva.so.2` does NOT declare `libva-drm.so.2` as `DT_NEEDED` (it only needs libc).
This works at process startup, but fails when the VM worker `dlopen`s `libkrun.so.1`
(RTLD_NOW) → `libvirglrenderer.so.1` → `libva.so.2` + `libva-drm.so.2`: glibc
resolves libva.so.2's relocations before libva-drm.so.2 is loaded, giving
`undefined symbol: vaGetDisplayDRM (fatal)`.

Fix (in `crates/cang/src/runtime/vm/libkrun/dynamic.rs`): pre-`dlopen` `libva-drm.so.2`
then `libva.so.2` with `RTLD_NOW|RTLD_GLOBAL` in the VM worker before the libkrun
dlopen. Verified: the VM worker maps now contain both libs.

What did NOT work for Blocker 1:

- `LD_PRELOAD` of the original libva — crashes the exec'd VM worker (signal 11);
  landlock blocks `/tmp` and the preload segfaults in the sandboxed worker.
- `patchelf --add-needed libva-drm.so.2` on a copy of libva.so.2 — **corrupts the
  binary**; the patched copy segfaults on plain dlopen, even in python.
- `LD_LIBRARY_PATH` with the patched lib first — same crash.

## Blocker 2 — `vaInitialize` fails in the VM worker (UNDIAGNOSED)

Even with libva loaded globally (Blocker 1 fix), `vrend_video_init` →
`vaInitialize()` still fails: virgl-debug.log shows `init va library failed`.

Decisive contrast: **`vaInitialize` succeeds in a normal process.** With
`LD_LIBRARY_PATH` = libva store, `LIBVA_DRIVERS_PATH=/run/opengl-driver/lib/dri`,
`LIBVA_DRIVER_NAME=virtio_gpu`, and an fd opened on `/dev/dri/renderD128`, a python
ctypes `vaGetDisplayDRM` + `vaInitialize` returns rc 0 ("va_openDriver() returns 0").
So the host's gallium `virtio_gpu` VA driver CAN initialize with the same DRM node.
The failure is specific to the VM worker's context.

What was tried for Blocker 2 (all still failing or unverified):

- `get_drm_fd` renderer callback added in the libkrun submodule
  (`virgl_renderer.rs`): opens `/dev/dri/renderD128` fresh per call and returns a
  valid fd; logs to `/tmp/virgl-debug.log` and `/tmp/virgl-getdrmfd.log`. The fd is
  valid (the callback's own direct probe on that fd succeeds in a normal process),
  yet video init still fails in the VM worker. The callback is necessary but not
  sufficient.
- Launcher flags changed `0x6c0` → `0x641`
  (`USE_EGL|VENUS|RENDER_SERVER|DRM|USE_VIDEO`, `NO_VIRGL` removed) in
  `launcher.rs`. **UNVERIFIED.** It contradicts the old comment ("native-context GL
  and the venus renderer cannot coexist in one virtio-gpu") and was never shown to
  work or to keep Chromium Vulkan functional.
- The in-callback probe's `dlopen("libva.so.2", RTLD_LOCAL)` can't resolve
  `vaGetDisplayDRM` (it's undefined there), so the probe body is skipped — it never
  logged the VM-worker `vaInitialize` VAStatus. This needs fixing to actually
  capture the failure point.

## Hypotheses for the next experiments (Blocker 2)

- H1 — The gallium `virtio_gpu` VA driver needs a **virgl GL context (VIRGL capset)**
  to initialize; the venus-only `NO_VIRGL` config disables it, so the driver's init
  (screen creation) fails inside the VM worker while it succeeds in a plain process.
  Test: run vaInitialize in a process that has an active virgl context on the same
  node vs one that does not.
- H2 — libkrun's rutabaga virgl context "owns" the DRM node; a second virgl video
  context on the same node conflicts. Test: from a separate process, open the node
  and create a context while the VM worker holds one.
- H3 — The correct architecture is to keep venus for Vulkan and run vrend + VA-API
  video over the virgl GL capset, which requires: virgl capset enabled (no
  `NO_VIRGL`), `VIRGL_RENDERER_USE_VIDEO (1<<11)`, and a working `get_drm_fd`.
  The open question is whether venus and virgl GL can coexist in one virtio-gpu
  device, or whether this needs two GPU devices / two contexts. Verify Chromium
  Vulkan smoke after any such change.

## Reconstruction notes (for future smoke tests)

- Live smoke uses `XDG_CONFIG_HOME=/home/dev/.local/share/containers/cang-smoke-config`
  (btrfs-snapshot backend), `script -qefc` PTY wrapping for guest stdout.
- In-guest Nix closure execution: copy the closure to guest `/tmp` tmpfs with
  `cp -rL --no-preserve=all`, chmod +x bin/, build `LD_LIBRARY_PATH` from the
  closure's lib dirs, set `LIBVA_DRIVERS_PATH=<mesa>/lib/dri` and
  `LIBVA_DRIVER_NAME=virtio_gpu`.
- Host-side note: `/sys/class/drm` exposes only virtio-pci 1af4:1050 (the host is
  itself a VM); the "radeonsi navi33" `vainfo` report is a phantom — there is no
  real GPU codec backend on this machine.
