# Virglrenderer Cang Live Smoke

A live smoke for **the virglrenderer cang ships** - the build in
`nix/lib/virglrenderer-patched.nix`, published as `.#virglrenderer` and linked
into the cang packages (`nix/lib/systems.nix`). It proves that build does real
work in both halves of the vrend path and in the GPU path underneath them, and
that it - not some other copy in the closure - is the one doing it.

Three things are scored, so a failure says which one broke:

| arm | proves |
| --- | --- |
| 1 | Chromium renders on venus (ANGLE on Vulkan, never SwiftShader) and its frame reaches a host compositor over cang's `--waypipe` path |
| 2a | VA-API **decode**: a software-encoded H.264 clip decodes through `/dev/dri/renderD128` in a `--gpu=drm` guest |
| 2b | VA-API **encode**: a hardware-encoded stream is a real bitstream (a software decoder reads it back frame-exactly) and a hardware encode -> hardware decode round trip is correct |
| both | every `libvirglrenderer.so.1` mapped during the run resolves into `.#virglrenderer`, and the render-server helper cang execs comes from it |

Arm 1 is `tools/chromium-cang-smoke --waypipe`, delegated to unchanged: its
frozen design, scoring and fresh-evidence rules are reused rather than
reimplemented. Arm 2 is new here, because nothing under `tools/` covered VA-API.

## Recreate

```bash
# cang's only implemented task-rootfs backend is btrfs-snapshot, so the output
# (hermetic podman graphroot + cang state) has to be on a btrfs filesystem:
sudo mount -o loop /home/dev/cang/btrfs.img /home/dev/cang/disk

nix develop --command tools/virglrenderer-cang-smoke/virglrenderer-smoke.sh \
  --out-dir /home/dev/cang/disk/virglrenderer-smoke
```

Everything is resolved from the flake (cang, cang-musl's guest-init,
`.#container`, `.#virglrenderer`) or nixpkgs (weston,
python3), so no arguments are needed on a host that has the prerequisites. With
overrides, and VA-API only (no weston/waypipe):

```bash
nix develop --command tools/virglrenderer-cang-smoke/virglrenderer-smoke.sh \
  --cang "$PWD/roots/cang-prebuilt/bin/cang" \
  --guest-init "$PWD/roots/cang-musl/bin/cang-guest-init" \
  --container  "$PWD/roots/container" \
  --out-dir /home/dev/cang/disk/virglrenderer-smoke --mem 4 --timeout 900 \
  --frames 60
```

`--skip-chromium` runs only arm 2, which is the fast loop while working on the
VA-API side (one VM boot instead of two, and no compositor).

## What it does

1. **Preflight** resolves every input and refuses to start on anything missing:
   `/dev/kvm`, a `/dev/dri/renderD*` node, a btrfs `--out-dir`, 12 GiB free
   there, and weston/waypipe/python3 for arm 1 - each a hard failure naming the
   `nix build` command, never a silent skip. It also
   resolves `.#virglrenderer` and checks the cang package's
   `libexec/cang-helpers/virgl_render_server` symlink resolves inside it, so a
   cang built against a different virglrenderer fails before anything boots.
   For a raw-ELF `.#cang` it sources `.#cang-render-server-env` (the same rule
   the Chromium smoke uses; `.#cang-prebuilt`'s wrapper sets those itself).
2. **One hermetic store**: a fresh btrfs-driver podman storage under `--out-dir`
   (the ambient `~/.config/containers/storage.conf` is never read), the image
   loaded once and shared by both arms, and an isolated
   `<out-dir>/config/cang/cang.toml` pinning `[state].location` and
   `backend = "btrfs-snapshot"`.
3. **Arm 1** runs the Chromium smoke with `--waypipe` and the already-loaded
   image reference; its exit status is arm 1's result and its own evidence and
   verdict are kept under `<out-dir>/chromium/`.
4. **Arm 2** stages `smoke/run-guest-vaapi.sh` plus the run's geometry into
   `/workspace/smoke` (the guest has no environment channel: cang passes PATH
   plus its allowlist, so every guest input is a staged file), starts an
   attribution sampler, and boots one `--gpu=drm` VM. The client is the image's
   own `ffmpeg`/`vainfo`, resolved from PATH inside the guest:
   `cang --mem <n> --gpu=drm --alloc hardened --seccomp=off --landlock=off
   --guest-init <musl> -- sh /workspace/smoke/run-guest-vaapi.sh`.
5. **Scores** the evidence and prints `VERDICT: PASS|FAIL`, exiting 0 only on
   PASS.

## Arm 2: the VA-API chain

All of it is 640x360, 60 frames, qp 26, `-bf 1`, from one lavfi `testsrc2`
source, and every step is bounded by `timeout` in the guest:

| step | command shape | why |
| --- | --- | --- |
| source | `testsrc2` -> raw nv12 | one input shared by every comparison |
| reference | `-c:v libx264 -qp 26 -bf 1` | the decode arm needs an input cang did not encode, or the two halves confound each other |
| decode (hw, frames) | `-hwaccel vaapi -hwaccel_output_format vaapi ... -f null -` | the decoder does the work for every frame; the count is scored |
| decode (hw, content) | `-hwaccel vaapi ... -vf format=nv12` | the same decoder's frames copied out as nv12 to compare |
| decode (sw) | plain `-i reference.h264` | control: the clip is decodable at all |
| encode (hw) | `-vaapi_device ... -vf format=nv12,hwupload -c:v h264_vaapi -qp 26 -bf 1` | cang's vrend encode path |
| encoded -> sw decode | plain `-i encoded-vaapi.h264` | the hardware stream has to be a bitstream something else accepts |
| round trip (frames) | `-hwaccel vaapi -hwaccel_output_format vaapi` on the *hardware* stream, `-f null -` | the encode's own stream decodes in hardware, frame for frame |
| round trip (content) | `-hwaccel vaapi` on the *hardware* stream, `-vf format=nv12` | and its frames match the source |
| VA probe | `vainfo --display drm --device /dev/dri/renderD128` | H.264 `VLD` (decode) and `EncSlice` (encode) must be advertised |

**Checks.**

| check | evidence | predicate |
| --- | --- | --- |
| `virglrenderer-helper` | preflight | the cang render-server helper resolves under `.#virglrenderer` |
| `chromium-waypipe` | arm 1 | the delegated Chromium smoke exits 0 with its own `VERDICT: PASS` |
| `vaapi-vm-exit` | `<out>/logs/cang-vaapi.console` | the guest ran to completion |
| `vaapi-guest-driver` | `evidence/vaapi-driver.txt` | the image's `/usr/lib/cang-va-runtime` driver resolves to a real store file |
| `vaapi-profiles` | `evidence/vaapi-vainfo.txt` | `VAProfileH264` + `VAEntrypointVLD` + `VAEntrypointEncSlice` |
| `vaapi-steps` | `evidence/vaapi-rc.txt` | every step recorded `name=0` (a missing line fails: the guest died before writing it) |
| `vaapi-frames` | `evidence/vaapi-frames.txt` | both hwframe decodes report exactly `frames` decoded frames |
| `vaapi-frame-bytes` | `evidence/vaapi-sizes.txt` | every nv12 buffer is exactly `frames * 640 * 360 * 3 / 2` bytes and both bitstreams are non-empty |
| `vaapi-first-gop` | `evidence/vaapi-psnr-decode_hw_sw-perframe.log` | the hardware decode equals the *software* decode of the same clip, for each of the first 30 frames, on every plane (`mse <= 1`) |
| `vaapi-psnr-decode` | `evidence/vaapi-psnr-decode.txt` | hardware decode vs source over the whole clip: `y >= 30`, `u/v >= 25` |
| `vaapi-psnr-encode` | `evidence/vaapi-psnr-encode.txt` | software decode of the hardware stream vs source |
| `vaapi-psnr-roundtrip` | `evidence/vaapi-psnr-roundtrip.txt` | hardware encode -> hardware decode vs source |
| `virglrenderer-loaded` | `<out>/logs/vaapi-attribution.txt` | every sampled `libvirglrenderer.so.1` mapping is under `.#virglrenderer` |

**Both defects this arm was built for are fixed and scored here.** The decode copy
used to take the picture out of the DMA-BUF the decoded surface exports, which cost
two failures: a three-plane guest buffer filled from a two-plane (NV12) export, so
the guest's Cb was the interleaved plane's first channel and its Cr was never written
(bit-exact luma and Cb, a zeroed Cr); and, because that export is cached per buffer
while the memory behind it is the driver's to reuse, a decoded output that stopped
advancing once the surface pool started recycling, so that after the first GOP
boundary the guest read the same stale frame for ever. Both are gone now that vrend
reads the surface itself and packs each guest plane from it
(`nix/pkgs/patches/virglrenderer-decode-surface-readback.patch`), which is what
`vaapi-first-gop` and the whole-clip checks measure.

**Why per-plane PSNR and a byte count, not "it exited 0".** The 2026-10 chroma
bug produced streams whose luma was fine and whose chroma was wrong; luma-only or
size-only evidence would have passed it. The `psnr` filter reports y/u/v
separately and all three are scored, with the measured values printed so a human
sees the margin (a working stack measures ~44-46 dB luma and ~42-44 dB chroma;
the floors sit well below that so the smoke reports a broken path rather than
encoder tuning).

**Known defect the smoke currently reports (red).** The guest's hardware decode
returns a **corrupted Cr (V) plane**: comparing the decoded frames to the source
per frame gives bit-exact luma and Cb (`mse_y: 0.00 mse_u: 0.00`, PSNR `inf`) and
a garbage V plane (`mse_v ~22540`, `psnr_v 4.6`) for the early frames, decaying to
`psnr y 22 / u 19 / v 4.6` by frame 60 as the damaged planes feed the decoder's
references. The software decode of the same clip scores `y 46.6 / u 44.2 /
v 44.4`, so the clip, the comparison and the plane layout are sound and the loss
is in the guest's VA surface read-back. That is the plane-index/geometry class
`docs/wayfinder/guest-vaapi-video/tickets/05-chroma-planes-wrong.md` named for
the *encode* side (`EGL_DMA_BUF_PLANE0_*` used for every plane,
`width / (i + 1)`), now on the decode side.
`vaapi-psnr-decode` / `vaapi-psnr-roundtrip` fail on it, and
`evidence/vaapi-psnr-*-perframe.log` has the per-frame shape. Measured
2026-10-13 in the shipped image (ffmpeg 8.1.2, guest mesa 26.1.8 VA driver,
`--gpu=drm`), 640x360 x 60 frames, qp 26, `-bf 1`.

**Known limitation: the explicit `hwdownload` form crashes (unscored).**
`-hwaccel_output_format vaapi -vf hwdownload,format=nv12` - the usual way to pull
a VAAPI surface back to memory - does not work in a cang guest: the driver cannot
derive an image from a decoded surface, so `hwdownload` alone fails with
`EINVAL` (ffmpeg exits 234) and adding `format=nv12` segfaults (rc 139, all
variants: with and without B-frames, all-intra, and on the hardware-encoded
stream). The decoder itself is fine (`-hwaccel_output_format vaapi ... -f null -`
reports every frame) and the same decoder's implicit copy-out
(`-hwaccel vaapi ... -vf format=nv12`) is correct, which is why the smoke scores
those two and records the `hwdownload` result as unscored `INFO` in
`logs/`+`evidence/vaapi-hwdownload.txt`. Measured 2026-10-13 in the shipped image
(ffmpeg 8.1.2, mesa 26.1.8 guest VA driver, `--gpu=drm`). If the explicit form
starts working (or starts failing differently), the `INFO` line changes.

**Attribution is sampled, not assumed.** While arm 2's VM is up, every
`/proc/<pid>/maps` containing `libvirglrenderer` is sampled every 2s into
`logs/vaapi-attribution.txt` (pid, comm, library path). The check fails if the
sample is empty or if any sampled path is outside `.#virglrenderer` - so a cang
linked against an unpatched virglrenderer fails even when the encode and decode
happen to succeed.

## Prerequisites

- `/dev/kvm` and a `/dev/dri/renderD*` node accessible to the user.
- `podman`, `buildah`, `script` (util-linux) and `nix` on PATH (the dev shell
  provides them).
- **A btrfs `--out-dir`.** `btrfs-snapshot` is the only implemented task-rootfs
  backend and it snapshots the Buildah graphroot; `fuse-overlay` is not
  implemented. On a non-btrfs output the smoke exits 2 up front. On this host
  that means mounting the loop image first (`sudo mount -o loop
  /home/dev/cang/btrfs.img /home/dev/cang/disk`).
- **Disk space**: the image unpacks to ~7.7 GiB per hermetic store; the smoke
  keeps a 12 GiB free-space preflight on the output filesystem.
- Arm 1 needs weston and waypipe (resolved with `nix build nixpkgs#weston
  nixpkgs#waypipe`, or `--weston`/`--waypipe-bin`) and python3.
- Arm 2 runs the image's own `ffmpeg` and `vainfo` (agent layer:
  `pkgs.ffmpeg-headless`, `pkgs.libva-utils`). They cannot be host tools: the
  guest's `/nix/store` is the **image's closure** (a few hundred paths, not the
  host's tens of thousands), so a host store path does not exist in the guest at
  all. If the run reports `failed to run command '<host path>'`, the image is
  older than this smoke's assumption - rebuild it.
- A finished run cannot be removed with a plain `rm -rf` (rootless podman owns
  the layer subvolumes through its subuid mapping): clean it inside that mapping
  with `podman unshare chmod -R u+w <out-dir> && podman unshare rm -rf <out-dir>`.

## Triage

- **`--skip-chromium` first.** If only one arm fails, the other's evidence tells
  you whether the GPU path was alive at all.
- **Arm 2 hangs at `frame= 0`**: read `evidence/vaapi-guest-driver.txt`. An empty
  or unresolved driver means the image's `/usr/lib/cang-va-runtime` layer is a
  dangling store symlink (a collected GC root), so libva fell through to the
  unpatched mesa runtime - the encode then stalls identically for every cang
  build. Root the image build (`nix build .#container -o <path outside the
  store>`) and keep the symlink.
- **`vaapi-profiles` fails with `VAProfileNone` only**: the request never reached
  the renderer (vrend's video path was not enabled), not a guest kernel problem.
- **`vaapi-psnr-*` fails on `u`/`v` only**: the chroma-plane class of bug.
- **`INFO hwdownload` shows a non-zero rc**: expected - see "Known limitation"
  above. A `0` there would mean the guest's VA driver learned to derive images.
- **A guest `ffmpeg` that stops and looks hung**: it is not. Under the managed PTY
  a guest ffmpeg stops on `SIGTTOU` unless `-nostdin` is passed, which is why
  every step passes it.
- **`-bf 0` is never used here.** On a virtio-gpu host, `-bf 0` with a decoded
  input hangs the host driver (see
  `docs/wayfinder/guest-vaapi-video/notes/encode-measurement-hazards.md`); the
  arms use `-bf 1` and a lavfi source.
- **Stray processes**: `timeout` sends SIGTERM only, and a stalled guest ffmpeg
  ignores it. Kill leftovers by exact name between runs (`pkill -9 -x ffmpeg`,
  `pkill -9 -x cang`), from a script file - an inline `pkill -f` whose own
  command line contains the pattern kills the shell running it.
- **Attribution has no sample**: the VM died before the worker mapped
  libvirglrenderer - read `<out>/logs/cang-vaapi.console` first.

## Output layout

```
<out-dir>/
  logs/
    cang-vaapi.console        arm 2's VM console
    vaapi-attribution.txt     sampled libvirglrenderer mappings (pid, comm, path)
    chromium-smoke.log        arm 1's full output
    podman-load.log
  vaapi/workspace/            arm 2's guest /workspace
    smoke/                    the guest script + its staged inputs
    evidence/                 every scored file (see the checks table)
  chromium/                   arm 1's own smoke tree (logs + workspace/evidence)
  container-storage/          the hermetic podman store
  config/, state/             isolated cang config and state
```

## Relationship to `tools/chromium-cang-smoke`

The Chromium smoke stays the authority on the browser/waypipe path and is run
unchanged as arm 1; its README documents that design, its `--waypipe` scoring and
its own prerequisites. This smoke adds the VA-API arms, the virglrenderer
attribution, and the composition. Both need a btrfs output filesystem and both
boot a real `--gpu=drm` cang microVM.
