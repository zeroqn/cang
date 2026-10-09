# Chromium Cang Live Smoke

Three modes. The default proves Chromium's GPU path inside a cang microVM
(headless Chromium on venus). `--waypipe` additionally proves cang's waypipe
transport: a host Wayland compositor and waypipe client, `cang
--waypipe=<socket>`, and a guest Chromium that renders on venus *and* presents
through waypipe, with a no-`--waypipe` control run for attribution. `--waypipe
--software-renderer` runs the same transport on the image's *software* renderer
instead of venus, without `--gpu=drm`, and scores the ICD pin that selects it.
The frozen design of the waypipe mode is specified below.

A single-command, artifact-correct live smoke for the cang GPU path using a
real Chromium inside a real cang microVM.

The browser is **inside the cang image** (`browserImageLayer`, built from the
pinned nixpkgs `ungoogled-chromium` wrapped package), so nothing is mounted
from the host and resource files (`*.pak`, libraries) mmap from the
digest-keyed image rootfs — the host-/nix overlay **lowerdir** — instead of a
fuse-overlayfs/virtio-fs upper, which is what previously made Chromium fail to
load resources. The environment is otherwise reproduced by the repository
inputs; the one host prerequisite is a btrfs output filesystem and an
amdgpu-backed DRM render node (see Prerequisites).

## Baseline status (2026-09-23)

Measured against the pinned `pins.libkrunRelease` (`cang-3307a15f940a`) and the
packaged `.#cang-prebuilt` (release asset `sha-f502ab1346a7`, the same asset the
2026-09-22 baseline used) with `.#cang-musl` built from the tree — the repo's
reproducible starting point, **not** the uncommitted `deps/libkrun` GPU
experiments:

```text
PASS  version       Chromium 153.0.8010.52
PASS  chromium-rc   gpu-dom=0 webgl=0 dom=0  (all three Chromium runs exit 0)
PASS  webgl-vulkan  ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)), venus)
PASS  webgl-png     non-empty screenshot
VERDICT: PASS
```

And with `--waypipe` (same pinned artifacts, 2026-09-23, with weston 15.0.1 and
waypipe 0.11.0):

```text
PASS  version             Chromium 153.0.8010.52
PASS  chromium-rc         gpu-dom=0 webgl=0 dom=0
PASS  webgl-vulkan        headless run: ANGLE/Vulkan on venus
PASS  webgl-png           non-empty screenshot
PASS  waypipe-transport   guest waypipe server connected to the host client
PASS  venus-presenting    waypipe-venus:ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)), venus)
PASS  frame-presented     host-frame-early.png holds 100928 pattern pixels
PASS  renderer-on-frame   host-frame-early.png holds 4616 pixels of the page's renderer overlay, so the screenshot names the renderer
PASS  control-no-frame    without --waypipe the compositor screenshot holds 0 pattern pixels
INFO  compositor          GL renderer: AMD Radeon RX 7600M XT (radeonsi, navi33, ACO, DRM 3.64, 7.2.4-cachyos-lto)
INFO  presenting          mode=waypipe wayland_display=cang-waypipe-0 gbm_backends_path=/usr/lib/cang-mesa-runtime/lib/gbm alive_after_dwell_secs=90 alive=yes
VERDICT: PASS — evidence: <out>/workspace/evidence (fresh, 21 files)
  screenshot: <out>/weston-screenshot.png (weston frame of the presented guest window)
  screenshot: <out>/weston-screenshot-control.png (control: the same guest work without --waypipe)
```

`renderer-on-frame` and the `weston-screenshot.png` copies are new here; the
earlier `frame-presented ... 210047 pattern pixels` line came from the pattern
page of the first baseline, which the renderer overlay replaced.

And with `--waypipe --software-renderer` (2026-09-25, no `--gpu=drm`):

```text
PASS  version             Chromium 153.0.8010.52
PASS  chromium-rc         gpu-dom=0 webgl=0 dom=0
INFO  webgl probe in the software run: renderer=no-webgl (unscored; see software-presenting)
PASS  webgl-png           non-empty screenshot
PASS  waypipe-transport   guest waypipe server connected to the host client
PASS  software-presenting waypipe-software:ANGLE (Mesa, Vulkan 1.4.354 (llvmpipe (LLVM 21.1.8 256 bits) (0x00000000)), llvmpipe)
PASS  software-env        waypipe server pins the lavapipe ICD via VK_ICD_FILENAMES, no VK_DRIVER_FILES
PASS  frame-presented     host-frame-early.png holds 117344 pattern pixels
PASS  renderer-on-frame   host-frame-early.png holds 3995 pixels of the page's renderer overlay
PASS  control-no-frame    without --waypipe the compositor screenshot holds 0 pattern pixels
INFO  presenting          mode=waypipe wayland_display=cang-waypipe-0 gbm_backends_path=<unset> alive_after_dwell_secs=90 alive=yes
VERDICT: PASS
```

### Baseline update (2026-09-26): libkrunfw `cang-362dfe8735d7`, guest kernel 6.12.109-hardened1

Re-measured after rebasing the libkrunfw fork onto upstream `v5.6.2` (guest
kernel 6.12.91-hardened1 -> 6.12.109-hardened1), with the tree-built `.#cang`
(raw ELF) under a render-server wrapper, the rebuilt `.#cang-musl` guest-init
and the same `.#container` archive:

```text
PASS  version       Chromium 153.0.8010.52
PASS  chromium-rc   gpu-dom=0 webgl=0 dom=0
PASS  webgl-vulkan  ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)), venus)
PASS  webgl-png     non-empty PNG screenshot
VERDICT: PASS
```

and with `--waypipe` (weston 15.0.1, waypipe 0.11.0), which reproduces the
2026-09-23 numbers exactly:

```text
PASS  waypipe-transport   guest waypipe server connected to the host client
PASS  venus-presenting    waypipe-venus:ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)), venus)
PASS  frame-presented     host-frame-early.png holds 100928 pattern pixels
PASS  renderer-on-frame   host-frame-early.png holds 4616 pixels of the page's renderer overlay
PASS  control-no-frame    without --waypipe the compositor screenshot holds 0 pattern pixels
INFO  compositor          GL renderer: AMD Radeon RX 7600M XT (radeonsi, navi33, ACO, DRM 3.64, 7.2.4-cachyos-lto)
VERDICT: PASS
```

Attribution for both runs is in the evidence: `gpu-diag.txt` line 1 reads
`uname: Linux localhost 6.12.109-hardened1 ... x86_64`, and the `libkrun-vm-worker`
process maps `...-libkrunfw-cang-362dfe8735d7/lib/libkrunfw.so.5.3.0`, so the
kernel under test is the one that booted. The upstream config additions the
rebase brought in (ACPI, PCI, virtio-pci, nf_tables, PSI) do not disturb the
venus/virtio-gpu path.

So the image's software renderer is pinned and reaches the guest's Vulkan stack:
`waypipe-server-pid=813` with `VK_ICD_FILENAMES=/usr/lib/cang-software-renderer/…/lvp_icd.x86_64.json`,
`VK_DRIVER_FILES` unset, and the presenting page's WebGL renderer reads back as
`llvmpipe`.

So hardware-accelerated Chromium presents through cang's waypipe transport: the
guest's GPU process renders on venus while its window reaches a host-side
compositor, and the venus renderer itself is read off the *host* waypipe client
log (the guest page publishes it as the window title) and printed on the page,
so the screenshot names it too.

**GPU acceleration works**: Chromium in the cang microVM renders WebGL through
the host GPU via `virtio-gpu` venus (`--gpu=drm`), with RADV on the host.

### Baseline update (2026-09-30): libkrunfw `v5.6.2-cang.3` (unreleased at the time), guest kernel 7.2.7-hardened1, clang ThinLTO

Re-measured for the fork's 7.2.7 kernel line (linux-7.2.7 +
linux-hardened v7.2.7-hardened1), on the **`-kvm-lto`** firmware `MakefileLto`
builds - the asset cang's own `libkrunfwRelease` pin consumes, here built
locally with clang 21.1.8 / LLD (`sha256
b92ee738810ad641807dcb077f9ac2696a36b3ad273dba4cb7794ed2830ff1a1`, 24,576,952
bytes), against `.#container` and the tree-built `cang` 0.10.1 with
`CANG_LIBKRUNFW_LIBRARY` pointing at it:

```text
PASS  version       Chromium 154.0.8037.57
PASS  chromium-rc   gpu-dom=0 webgl=0 dom=0  (all three Chromium runs exit 0)
PASS  webgl-vulkan  ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT (RADV NAVI33)) (0x00007480)), venus)
PASS  webgl-png     non-empty PNG screenshot
INFO  gpu-dom       rc=0 captured; unscored (feature table is shadow DOM)
VERDICT: PASS — evidence: <out>/workspace/evidence (fresh, 14 files)
```

The run's own `gpu-diag.txt` line 1 reads `uname: Linux localhost
7.2.7-hardened1 #1 SMP PREEMPT_DYNAMIC ... x86_64 GNU/Linux`, so the venus
renderer above is the 7.2.7 kernel's, not a stale 6.12 guest's. This is the
evidence that the 7.2.7 re-base (23 patches, the bit-5
`FENCE_PASSING`/`BLOB_ALIGNMENT` collision fix, and the re-applied
`CREATE_GUEST_HANDLE`/`BLOB_CTX_ID_FIX` work) keeps the GPU path intact - see
`docs/wayfinder/libkrunfw-kernel-rebase/`.

## Why `--disable-vulkan-surface` is in the guest flags

Without it the GPU process dies with `GPU process exited unexpectedly:
exit_code=6` and `renderer=` stays empty, which reads like "venus is broken".
It is not: tracing the GPU process shows it driving venus successfully (dozens
of `DRM_IOCTL_VIRTGPU_EXECBUFFER` on `/dev/dri/renderD128`, all returning 0,
plus `VIRTGPU_CONTEXT_INIT`/`VIRTGPU_MAP`), then going quiet for ~6 s and
aborting with **no failing syscall and no message** — an unretired completion,
not a crashed ioctl. The abort is in ANGLE's Vulkan **WSI/swapchain** (present)
path; with `--disable-vulkan-surface` ANGLE takes a non-WSI path and the venus
renderer appears. Notes for whoever digs further:

- A plain venus Vulkan workload (`tools/virgl-guest-probe`) passes in the same
  VM, so the host render-server path and venus fence/buffer handling are fine on
  their own. ANGLE's present path is what venus does not complete.
- The uncommitted `deps/libkrun` venus per-context poll/fence work is **not**
  needed to fix this: a source-built libkrun with those changes fails the same
  way without `--disable-vulkan-surface`. (Fence/poll callbacks never fire for
  either workload — submits carry `num_in_fences=0`.)
- Chromium with the same flags on the host (no venus) renders fine, so the
  combination to reason about is venus + ANGLE's WSI.
- The Chromium GPU process's own diagnostics are swallowed (`--enable-logging
  =stderr`, `MESA_DEBUG`, `VK_LOADER_DEBUG` and `--log-file` all yield nothing);
  tracing it with `strace -f -e trace=ioctl` is the way to see it work.

The **presenting** run (`--waypipe`) needs the opposite treatment, and this is
the part that is easy to get wrong:

- It must **not** pass `--enable-features=Vulkan`. That switches the display
  compositor onto Vulkan, which needs a `VkSurfaceKHR`; ozone-wayland does not
  implement `CreateVulkanSurface` and logs `'--ozone-platform=wayland' is not
  compatible with Vulkan`. The message is harmless on its own (the host emits it
  too, with zero GPU crashes) but with the feature enabled the guest's GPU
  process crash-loops and never paints.
- `--use-angle=vulkan` alone is the correct knob: ANGLE (WebGL/raster) runs on
  Vulkan->venus while the compositor stays off Vulkan.
- `GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm` must be set in the
  guest. guest-init's `MESA_ENV` points `LIBGL_DRIVERS_PATH`, the EGL vendor file
  and `VK_ICD_FILENAMES` at the image's mesa but never sets the GBM backend path,
  so ozone searched the NixOS default `/run/opengl-driver/lib/gbm`, missed the
  guest's `dri_gbm.so` and could not init a DRM render node.
  *Note (2026-10-09): guest-init now builds `/run/opengl-driver/lib/gbm` as part
  of the conventional graphics farm, so this re-pointing is belt-and-braces rather
  than required; `MESA_ENV` still sets `GBM_BACKENDS_PATH` explicitly.*
- dmabuf must still be blocked on the waypipe side (`-n`/`--no-gpu`), though the
  buffer-descriptor failure this run first recorded is fixed. The guest's
  virtio-gpu GBM path reported `DRM_FORMAT_MOD_INVALID` for its shared exports,
  which waypipe rejects outright (`unsupported modifier ffffffffffffff`), and the
  strides it reported were synthesized rather than host ones (the 44x55 hardware
  cursor exported `stride = 176`), which RADV rejects for a linear image with
  `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`; waypipe turned either
  error into a fatal `wl_display` error. The guest now publishes the host's real
  layout as `LINEAR`, so buffers arrive as dmabufs again and no import error is
  logged, but `-n` is kept because with dmabuf enabled the presenting Chromium
  GPU process still aborts (`GPU process exited unexpectedly: exit_code=6`) and
  never paints, which fails the `venus-presenting` check. Buffers therefore
  travel as `wl_shm`: rendering is accelerated, the transfer is not zero-copy.

The run also keeps the frames it scored, so a user can check the verdict by eye
instead of trusting the pixel count: `<out>/weston-screenshot.png` is the best
presenting capture and `<out>/weston-screenshot-control.png` the control's, and
both verdict branches print their paths. The presenting frame is the readable
half of the evidence: a magenta page carrying the page's own cyan
`renderer=ANGLE (... Virtio-GPU Venus ..., venus)` overlay, so the image alone
shows the frame arrived *and* names the hardware renderer. The control frame is
the bare compositor (no guest window), which is what makes the presented frame
attributable to the transport.

### Why the presenting page does not draw WebGL

The page only *reads* the WebGL renderer string; it never draws. That is not an
omission, it is a measured wall: in the presenting run (`--ozone-platform=wayland
--use-angle=vulkan`) any WebGL draw aborts the guest's GPU process, and the
canvas is lost while the rest of the page keeps presenting. Measured on this host
(pinned artifacts, 2026-09-23), with the same page, run and VM:

| page | GPU process | frame |
| --- | --- | --- |
| context + `getParameter` only (what ships) | survives | magenta page + overlay |
| visible canvas, `clear` + triangle | `exit_code=6` (SIGABRT) after the draw | magenta page + overlay, canvas region empty |
| hidden canvas, triangle + `toDataURL()` into an `<img>` | `exit_code=8704`, `Restarting GPU process due to unrecoverable error. Context was lost.` | magenta page + overlay, image never appears |

The same context is created without a crash in the headless runs (that is what
`webgl-vulkan` reads), so the wall is specific to drawing in the presenting run;
the headless pages do not draw either, and this smoke does not claim they do.

The loss shows up in the page as `WebGL: CONTEXT_LOST_WEBGL`; the GPU process is
reinitialised afterwards and the page's *later* paints still present, which is why
a crash is easy to miss from the screenshot alone. So the presenting run cannot
put GPU-drawn pixels in its own frame on this stack, and the frame's
hardware-acceleration evidence is the renderer string the page prints (readable
in `weston-screenshot.png`) plus the host-side title check, not a GPU-rendered
image. Do not "fix" `renderer-on-frame` by adding a WebGL draw to the page: it
will fail as above. Getting GPU-drawn pixels into a waypipe frame is its own
problem (candidate: publish a headless venus render into the page), not a change
to this check.

Reproducing a pinned baseline (rooted so a later `nix-collect-garbage` cannot
delete the artifacts mid-run):

```bash
nix build .#container      -o roots/container
nix build .#cang-prebuilt -o roots/cang-prebuilt
nix build .#cang-musl  -o roots/cang-musl
tools/chromium-cang-smoke/chromium-smoke.sh \
  --cang      "$PWD/roots/cang-prebuilt/bin/cang" \
  --guest-init "$PWD/roots/cang-musl/bin/cang-guest-init" \
  --container  "$PWD/roots/container" \
  --out-dir /path/on/btrfs/chromium-smoke --mem 4 --timeout 900
```

## Recreate

```bash
tools/chromium-cang-smoke/chromium-smoke.sh
```

or with overrides:

```bash
tools/chromium-cang-smoke/chromium-smoke.sh \
  --cang /path/to/cang \
  --guest-init /path/to/cang-guest-init \
  --container /nix/store/...-cang-image.tar.gz \
  --mem 4 --timeout 600
```

Resolution note: the three inputs default to `nix build --no-link` store paths,
so no `./result` symlink is read or written (the repo's `result` is mutable and
shared, and a path resolved through it can be repointed at the container archive
mid-run). `--cang` accepts a package prefix, a `.../bin/cang` binary, or a
wrapper script. `.#cang` is deliberately a raw ELF, so a `--gpu=drm` run needs
the render-server environment (`CANG_MESA_LIBDIR`, `CANG_MESA_ICD`,
`CANG_VULKAN_LOADER_LIBDIR`): for a raw ELF the smoke sources the repo's own
`.#cang-render-server-env` file (a value the caller already exported wins, and a
wrapper sets its own), so the bare `tools/chromium-cang-smoke/chromium-smoke.sh`
works; it fails up front only if that resolution produces no file.

Waypipe mode (needs weston + waypipe + python3; see *--waypipe mode* below):

```bash
nix build nixpkgs#weston nixpkgs#waypipe
nix develop --command tools/chromium-cang-smoke/chromium-smoke.sh \
  --waypipe \
  --weston      /nix/store/...-weston-15.0.1/bin/weston \
  --waypipe-bin /nix/store/...-waypipe-0.11.0/bin/waypipe \
  --cang "$PWD/roots/cang-prebuilt/bin/cang" \
  --guest-init "$PWD/roots/cang-musl/bin/cang-guest-init" \
  --container  "$PWD/roots/container" \
  --out-dir /path/on/btrfs/chromium-smoke --mem 4 --timeout 900
```

Software-renderer mode is the same invocation plus `--software-renderer` (it
needs `--waypipe`, and it drops `--gpu=drm` itself):

```bash
nix develop --command tools/chromium-cang-smoke/chromium-smoke.sh \
  --waypipe --software-renderer \
  --weston      /nix/store/...-weston-15.0.1/bin/weston \
  --waypipe-bin /nix/store/...-waypipe-0.11.0/bin/waypipe \
  --cang "$PWD/roots/cang-prebuilt/bin/cang" \
  --guest-init "$PWD/roots/cang-musl/bin/cang-guest-init" \
  --container  "$PWD/roots/container" \
  --out-dir /path/on/btrfs/chromium-smoke --mem 4 --timeout 900
```

## What it does

1. **Builds/loads** `.#container` (the flake build runs the image wrapper
   contracts incl. `browserContracts`). The OCI archive is loaded into a
   **hermetic** podman/buildah storage (a fresh `btrfs`-driver store under the
   output dir, with local `TMPDIR`), so the smoke never depends on the ambient
   `~/.config/containers/storage.conf`. The `btrfs` driver is required, not
   `vfs`: cang snapshots the Buildah-mounted rootfs, and a `vfs` graphroot is
   plain directories that `btrfs subvolume snapshot` rejects.
2. **Resolves** cang (`.#cang`) and the guest-init override
   (`.#cang-musl` → `bin/cang-guest-init`) as packaged artifacts.
3. **Isolates** config+state: `XDG_CONFIG_HOME=<out>/config`,
   `XDG_STATE_HOME=<out>/state`; a private `cang.toml` sets
   `[state].location` and `[task-rootfs].backend = "btrfs-snapshot"`. The
   smoke **fails fast** if the graphroot or state home is not on btrfs, because
   the only implemented task-rootfs backend snapshots the Buildah graphroot.
4. **Stages** `/workspace` = `<out>/workspace` containing only
   `smoke/run-guest.sh` and a **fresh** `evidence/` dir (previous evidence is
   deleted unless `--keep-evidence`). Never reuses stale artifacts.
5. **Launches** the real VM from inside the workspace via a PTY
   (`script -q -e -c`), with the verified smoke shape:
   `cang --gpu=drm --alloc hardened --mem <n> --seccomp=off --landlock=off
   -- sh /workspace/smoke/run-guest.sh`, console captured to
   `<out>/logs/cang.console`, under `timeout`.
6. **Scores** fresh evidence (every file must be non-empty **and** have
   mtime ≥ run start, so stale evidence can never pass):

   - `version.txt` — `Chromium <n>`
   - `webgl-dom.html` — post-script DOM of the probe page (holds the runtime
     `renderer=` string)
   - `webgl-renderer.txt` — the runtime `renderer=` / `vendor=` / `gl_version=`
     lines from `webgl-dom.html`; must name `Vulkan` and must not be
     `SwiftShader` (this is the vulkan/venus assertion)
   - `webgl.png` — non-empty PNG (GPU-composited screenshot)
   - `chromium-rc` — `gpu-dom=0 webgl=0 dom=0`, every chromium run rc 0
   - `gpu-dom.html` — the `chrome://gpu` dump, captured for humans but **not**
     scored: its feature-status table is shadow DOM, which `--dump-dom` does
     not serialize.

   The mtime threshold is `date +%s` (seconds), matching `stat -c %Y`; the
   renderer is read from the runtime DOM, never from the static probe HTML.
   Each in-guest chromium invocation is bounded by `timeout 150` so a
   venus/render-server stall is attributed to one run instead of hanging until
   the host-side VM timeout.

   Exit 0 only when all pass; otherwise exit 1 with the failed check and
   evidence/log paths.

## `--waypipe` mode (frozen design)

Frozen in `docs/wayfinder/waypipe-gpu-smoke/tickets/04-freeze-waypipe-mode-design.md`.

**What it proves.** That cang's `--waypipe` path carries a real Wayland client
from the guest to a host compositor, and that the client is hardware-accelerated
while doing it. Five things, scored separately so a failure says which half broke:
the transport connected; the presenting Chromium rendered on venus; the frame
arrived at the compositor; that frame carries the renderer the page printed into
it, so the screenshot names the hardware renderer by itself; and the same work
*without* `--waypipe` delivers nothing (attribution).

**Flags.**

| flag | default | meaning |
| --- | --- | --- |
| `--waypipe` | off | run the presenting run and its control, and score them |
| `--software-renderer` | off | with `--waypipe`: run without `--gpu=drm` and score the software-renderer pin (see `--software-renderer` mode) |
| `--weston <path>` | `$WESTON_BIN`, else PATH | compositor binary (`nix build nixpkgs#weston`) |
| `--waypipe-bin <path>` | `$WAYPIPE_BIN`, else PATH | waypipe binary (`nix build nixpkgs#waypipe`) |
| `--weston-renderer <gl\|pixman>` | `gl` | compositor renderer; `pixman` is an explicit opt-in (software compositing, so transport evidence only) |
| `--present-wait <secs>` | 30 | first compositor screenshot after launch; a second follows 30s later |
| `--python <path>` | `$PYTHON`, else PATH | reads the screenshot pixels (stdlib zlib; `nix develop` provides python3) |

**Stages.** (1) preflight: the existing btrfs/free-space checks plus weston,
waypipe and python3, each a hard failure naming the build command; (2)
compositor: `weston --backend=headless --renderer=gl --debug --width=640
--height=480 --socket=cang-smoke`, waited for with a liveness check; (3) waypipe
client: `waypipe -d -n --socket <out>/waypipe/waypipe.sock client` pointed at that
compositor socket, also waited for with a liveness check; (4) run A: `cang ...
--waypipe=<socket>` with the guest in `waypipe` mode while the host captures the
compositor twice; (5) run B: the identical guest work with no `--waypipe`
(control), captured the same way; (6) teardown: an EXIT trap kills both helpers.

**Evidence and predicates** — all under `<out>/workspace/evidence/`, all required
to be fresh (non-empty and mtime >= run start):

| check | evidence | predicate |
| --- | --- | --- |
| `waypipe-transport` | `host-waypipe-client.log` | holds `Connection received` and `Connected waypipe-server` |
| `venus-presenting` | `host-waypipe-client.log` | a `set_title("waypipe-venus:...")` line naming `Vulkan` and `venus`, never `SwiftShader` |
| `frame-presented` | `host-frame-early.png`, `host-frame-late.png` | either holds >= 5000 pixels of the pattern colour |
| `renderer-on-frame` | `host-frame-early.png`, `host-frame-late.png` | either holds >= 500 pixels of the cyan the page's `renderer=`/`vendor=`/`gl_version=` overlay is printed in (counted with a 16/255 per-channel tolerance) |
| `control-no-frame` | `control-frame-early.png`, `control-frame-late.png` | neither holds >= 5000 pattern pixels |
| the four headless checks | as in the default mode | unchanged |

**Why these choices.**

- **The venus claim is read off the host, not the guest.** The pattern page sets
  `document.title` to the WebGL `UNMASKED_RENDERER_WEBGL` string, and waypipe logs
  window titles verbatim, so the renderer crosses the transport into a host-side
  log. One artefact proves the title crossed *and* names the renderer. No strace:
  tracing the GPU process slowed startup enough to hide a crash loop, so tracing
  must not sit on the scored path.
- **The frame is asserted on pixels, never on the file.** Without `--debug` weston
  refuses capture and writes a plausible all-black PNG, so the check decodes the
  image (`png-colour-count.py`, stdlib `zlib` only; an optional per-channel
  tolerance absorbs antialiased glyph edges in the renderer overlay).
- **The screenshot carries the hardware-acceleration claim, not just the frame.**
  The pattern page paints a magenta background and prints
  `renderer=`/`vendor=`/`gl_version=` into an on-page overlay drawn in `#00ffff`.
  So one PNG answers both halves: magenta pixels say the guest's frame reached
  the compositor, and cyan pixels say the renderer readout really is in the
  image a human opens — `renderer-on-frame` fails if the overlay stops reaching
  the frame, so the screenshot cannot silently lose the claim. The window title
  carries the same string into the host-side log, which is where the mode's
  machine-checkable venus evidence comes from; the overlay is that string for a
  human. Nothing here proves the frame was *drawn* on the GPU: the page cannot
  draw with WebGL in this run at all, see below.
- **A control run is mandatory.** A green frame check on its own cannot show the
  frame arrived *through the transport*.
- **Preflights check liveness, not just sockets.** `--out-dir` is reused between
  runs, and a leftover socket file both makes a new listener fail with
  `EADDRINUSE` and satisfies a `-S` test, so both helpers are verified alive.
- **Guest-side mode travels by file.** Environment variables do not reach the
  guest (cang passes only PATH plus its allowlist), so the runner stages
  `smoke/run-mode` and the guest reads it.
- **The presenting run gets the guest to itself** (`PRESENT_DWELL`, 90s) before
  the headless checks start: running them concurrently starved the presenting
  renderer, whose window set its title but never put pixels on the wire inside
  the capture window.

**What it deliberately does not do.** dmabuf zero-copy (blocked, see above);
input events; weston on real DRM/KMS; transports other than vsock.

## `--software-renderer` mode

`--waypipe --software-renderer` scores the other renderer the image carries: the
software stack (mesa's lavapipe, `llvmpipe`) that guest-init pins for the waypipe
transport, rather than the venus GPU path.

It must run **without `--gpu=drm`**: that is the only configuration in which cang
exports the software-renderer environment, because
`guest_init::components::waypipe.rs::export_env` applies it only when the DRM GPU
mode is off, and only along the waypipe path. So the mode drops `--gpu=drm` and
keeps `--mem <n> --alloc hardened --seccomp=off --landlock=off` (the allocator
matters: Chromium's `partition_alloc` crashes with the default mimalloc), plus
everything else the waypipe mode does - the compositor, the client, the frame
capture and the no-`--waypipe` control run.

The pin under test is `VK_ICD_FILENAMES=<software renderer>/…/lvp_icd.x86_64.json`
with `VK_DRIVER_FILES` unset. The loader gives `VK_DRIVER_FILES` precedence over
the `VK_ICD_FILENAMES` a client sets for itself, so a guest that pinned the ICD
that way would lose this software renderer *and* ANGLE's SwiftShader display in an
ordinary run.

| check | evidence | predicate |
| --- | --- | --- |
| `software-env` | `renderer-env-waypipe.txt` | the *waypipe server's* environment (read from its `/proc/<pid>/environ`) holds `LIBGL_ALWAYS_SOFTWARE=1`, `VK_ICD_FILENAMES=<software renderer lvp icd>` and no `VK_DRIVER_FILES`; the pid must be present |
| `software-presenting` | `host-waypipe-client.log` | a `set_title("waypipe-software:...")` line naming `Vulkan` and `llvmpipe`, never `SwiftShader` |
| `waypipe-transport`, `frame-presented`, `renderer-on-frame`, `control-no-frame`, `version`, `chromium-rc`, `webgl-png` | as in `--waypipe` mode | unchanged |

Why the pin is read off the waypipe server and not off the guest entrypoint: the
software-renderer environment is exported to the waypipe server and its command
child (that is the design contract), and the *presenting* run is the process in
that tree. The guest entrypoint environment is deliberately its own curated set,
so a headless run started from it has no ICD to reach and reports `no-webgl`;
that probe is reported as `INFO` here and the renderer claim is scored by
`software-presenting`, whose title *is* the presenting page's WebGL renderer
string (ANGLE/Vulkan on `llvmpipe`).

Two wrinkles shape the guest flags:

- A software Vulkan device is on Chromium's GPU blocklist, so WebGL refuses it
  (`ContextResult::kFatalFailure: WebGL1/2 blocklisted`) and the page reports
  `no-webgl` until the guest passes `--ignore-gpu-blocklist`. That flag is part of
  this mode's `ANGLE_FLAGS`. Without it the only WebGL a software guest gets is
  Chromium's own bundled SwiftShader, which is *not* the image's software renderer
  and would let a weaker check pass for the wrong reason.
- The presenting page's title label comes from the mode
  (`?label=waypipe-software`), so a software run cannot be scored against a venus
  title.

Because it never asks for a DRM render node, this is also the one mode that does
not need the amdgpu-backed `/dev/dri/renderD*` prerequisite.

## Output layout

```text
<out>/
  config/cang/cang.toml   hermetic cang config
  state/                    cang state root
  workspace/                guest /workspace (bind)
    smoke/run-guest.sh      the in-guest workload
    evidence/               the scored artifacts
  logs/cang.console        full VM console (run A)
  logs/cang-control.console full VM console (control run, --waypipe only)
  logs/weston.log           compositor log (--waypipe only)
  logs/waypipe-client.log   host waypipe client log (--waypipe only)
  waypipe/run/              private XDG_RUNTIME_DIR for the compositor
  waypipe/shot/             weston-screenshooter working dir
  waypipe/waypipe.sock      the socket cang is given (--waypipe only)
  weston-screenshot.png     best presenting frame (magenta page + the page's cyan
                            renderer= overlay), for a human to look at
  weston-screenshot-control.png  control frame (bare compositor), same guest work without --waypipe
  logs/*                    mirrored chromium logs live in evidence/
```

## Prerequisites

- `/dev/kvm` accessible to the user.
- A `/dev/dri/renderD*` node accessible to the user, backed by an amdgpu host
  GPU exposed through DRM native context: the render-server wrapper pins mesa's
  `radeon_icd`, and cang runs `virgl_render_server` with venus.
- `podman`, `buildah`, `script` (util-linux), `nix` on PATH (the `nix develop`
  shell provides them; the README live-run section lists the same set).
- **btrfs output filesystem**: both the hermetic container graphroot and the
  cang state home live under `--out-dir`, and `btrfs-snapshot` is the only
  implemented task-rootfs backend. On non-btrfs the smoke exits 2 up front.
- `--waypipe` additionally needs **weston** and **waypipe**
  (`nix build nixpkgs#weston nixpkgs#waypipe`; both resolve in the current pin)
  and **python3** for the PNG pixel check (`nix develop` provides it). Each is a
  hard failure with the build command in the message, never a silent skip.
- **Disk space**: the cang image (with the in-image Chromium) decompresses to
  ~7.7 GiB. The smoke keeps a 12 GiB free-space preflight on the output
  filesystem and fails fast with a clear message rather than mid-load ENOSPC.
  Each run loads the image into a fresh hermetic store under `--out-dir`, and a
  finished run cannot be removed with a plain `rm -rf` (rootless podman owns the
  layer subvolumes through its subuid mapping, so the delete fails with
  `Permission denied` on files such as `.../home/dev/.terminfo/*`). Clean it
  inside that mapping instead: `podman unshare chmod -R u+w <out-dir> &&
  podman unshare rm -rf <out-dir>`.

## Triage

- **All PASS** — fresh evidence in `<out>/workspace/evidence/` proves Chromium
  launched with the GPU path, WebGL is hardware-backed (non-SwiftShader), and
  a GPU-composited PNG rendered.
- **FAIL webgl-vulkan** — `webgl-renderer.txt` holds `renderer=no-webgl`
  (Chromium could not create any WebGL context), named `SwiftShader`, omitted
  `Vulkan`, or is empty (venus stall). Read `<out>/logs/cang.console`,
  `<out>/workspace/evidence/gpu-diag.txt`, `chromium-webgl.log`,
  `chromium-webgl-dom.log`, and `webgl-renderer.txt`. The current baseline is
  exactly this case: the Chromium GPU process aborts (`exit_code=6`) and
  `renderer=no-webgl`. The runner's job is reproducibility and honest
  attribution, not to fix Chromium; do not weaken the assertions to force
  green.
- **VM timeout (124)** — `<out>/logs/cang.console` ends without the guest
  evidence; check host prerequisites (`/dev/kvm`, state-home backend) first.
- **Stale/missing evidence** — the freshness rule caught a reused artifact;
  re-run without `--keep-evidence`.
- **FAIL waypipe-transport** — the guest never dialled the host client. Check
  `<out>/logs/waypipe-client.log` (a stale socket shows up as `EADDRINUSE`),
  then `cang.console` for cang's own preflight messages (`waypipe socket does
  not exist`, `waypipe transport is not a Unix socket`).
- **FAIL venus-presenting** — the transport worked but the presenting Chromium
  did not report a venus renderer. Read `presenting-waypipe.log` and
  `presenting-waypipe-state.txt`: a missing `GBM_BACKENDS_PATH`, an added
  `--enable-features=Vulkan`, or a GPU process crash loop are the usual causes.
- **FAIL frame-presented** — the window never reached the compositor. Open
  `<out>/weston-screenshot.png` against `<out>/weston-screenshot-control.png`
  (the same frames the check scored, in `host-frame-*.png` / `control-frame-*.png`
  form in the evidence dir); raise `--present-wait` if the guest was simply slow,
  and check `presenting-waypipe-state.txt` for whether the browser was alive at
  the end of its dwell.
- **FAIL renderer-on-frame** — the frame arrived but the page's renderer overlay
  did not (magenta present, cyan missing). Open `<out>/weston-screenshot.png`:
  if the overlay is missing the page never painted it, and
  `presenting-waypipe.log` says why (a GPU process crash takes the page's later
  paints with it). `presenting-waypipe-state.txt` reports whether the browser
  was alive at the end of its dwell.
- **FAIL control-no-frame** — the pattern appeared without `--waypipe`, so the
  frame check proves nothing; look for a leftover window on the compositor.

## Notes

- The default launch flags mirror the running chromium GPU investigation
  (`--seccomp=off --landlock=off` avoids host-policy SIGSYS interference while
  reproducing the GPU evidence; re-enabling host policy is a separate
  hardening task). `--alloc hardened` is required: mimalloc is incompatible
  with Chromium partition_alloc.
- `--no-default-flags` drops the default set except `--mem`, for manual
  experimentation.