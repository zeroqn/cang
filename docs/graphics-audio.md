# Graphics and audio

Display and audio passthrough for cang tasks. Host prerequisites for
`--gpu=drm`, `--wayland`, and `--waypipe` are listed in the
[README](../README.md#prerequisites).

## Pulse TCP audio

```ini
# ~/.config/pipewire/pipewire-pulse.conf.d/cang-tcp.conf
pulse.properties = {
    server.address = [
        "unix:native"
        "tcp:127.0.0.1:4713"
    ]
}
```

Restart the host `pipewire-pulse` user service after adding the drop-in. Use
`--pulse=tcp:localhost:PORT` or `--pulse=tcp:127.0.0.1:PORT` to create a
private per-task bridge to the selected host IPv4-loopback listener. The guest
receives `PULSE_SERVER=unix:/run/user/<uid>/cang-pulse`; each guest Pulse
connection crosses a dedicated libkrun vsock channel and connects only to the
configured host `127.0.0.1:PORT`. Cang does not enable passt host-loopback
mapping or expose other host-loopback ports. Task launch succeeds if the host
listener is unavailable, and a later guest connection can succeed after the
host service starts or restarts.

Other literal IPv4 and bracketed IPv6 endpoints remain direct guest TCP
endpoints and are exported canonically as `PULSE_SERVER=tcp:IP:PORT`. Cang
does not start host or guest `pipewire`/`pipewire-pulse`, proxy native PipeWire
`pipewire-0`, forward a Pulse cookie, or test the connection before launch.
`cang exec` inherits the endpoint selected when the task was launched and
cannot change it.

Host `pipewire-pulse` configuration owns authorization. A permitted Pulse
client may gain playback, capture, stream inspection, or server-control access,
so configure the listener's access policy for the trust granted to the task.
For combined software-only Waypipe playback, mpv 0.41.0 also needs
`--gpu-sw=yes`, for example:

```bash
./result/bin/cang \
  --waypipe=/tmp/cang-waypipe.sock \
  --pulse=tcp:localhost:4713 \
  -- mpv --gpu-sw=yes '/workspace/'*.mp4
```

## Remote Waypipe

```bash
# Workstation: connect Waypipe to the local compositor.
waypipe --socket "$XDG_RUNTIME_DIR/cang-waypipe.sock" client

# Workstation: keep an authenticated reverse Unix-socket forward open.
ssh -R /tmp/cang-waypipe.sock:"$XDG_RUNTIME_DIR/cang-waypipe.sock" cang-host

# cang host: launch a Waypipe-capable task with the initial target.
./result/bin/cang \
  --workspace=/home/dev/foo \
  --waypipe=/tmp/cang-waypipe.sock \
  -- gui-application

# Reuse the running Waypipe server for another GUI command.
./result/bin/cang --waypipe exec TASK -- another-gui-application

# Replace the target, restart the guest Waypipe server, then run a command.
./result/bin/cang --waypipe=/tmp/other-waypipe.sock exec TASK -- gui-application
```

Both ends of that transport should come from one build. The guest's `waypipe` -
the server guest-init execs - is the image's own (zeroqn/headless's prebuilt
asset where one is pinned for the system), and `nix build .#waypipe-bin` is that
same package for the workstation, so the client and the server speak one
protocol version.

cang validates that the selected workspace is an absolute directory and each
valued socket path is absolute and already exists as a Unix socket. The guest
command is optional; when omitted, cang starts the normal interactive fish
login shell. Valueless `--waypipe` launches the task capability without an
active target. A valueless Waypipe exec reuses the running server and display.
A valued Waypipe exec serially changes the target, terminates and reaps the
running server, starts a fresh server on the stable display name, waits for
readiness, and only then starts the command. This is replacement, not protocol
reconnection: existing GUI applications connected to the old server lose their
Wayland connection and normally exit. If `--workspace` is omitted, cang uses
the current working directory. cang does not start SSH or the workstation
Waypipe client and does not create, unlink, or clean up the forwarded socket.
Without `--gpu=drm`, the mode passes `--no-gpu` to Waypipe. The cang guest
image provides Mesa software rendering for this path: OpenGL/EGL applications
use llvmpipe and Vulkan applications use lavapipe on the guest CPU. When
`--gpu=drm` is also selected, guest-init omits Waypipe's `--no-gpu`, does not
force the software-renderer environment, and preserves the DRM-scoped Mesa
OpenGL/EGL and Vulkan discovery paths, including `GBM_BACKENDS_PATH`
(`/usr/lib/cang-mesa-runtime/lib/gbm`). It also builds the NixOS-conventional `/run/opengl-driver`
symlink farm (libva's `lib/dri`, libgbm's `lib/gbm`, glvnd's `egl_vendor.d`, the Vulkan
`icd.d`) out of the image's runtime directories, so a nixpkgs client that consults those
compiled-in defaults resolves without a cang env var; the env vars above stay the
authoritative override. The farm's `lib/dri` entry is the patched VA driver, never mesa's
own. The frame transfer is then dma-buf
rather than shared memory: a client that allocates GBM buffers presents them
without a copy, and `ffmpeg`/`mpv` played through the display reach the guest's
hardware VA-API path. For a media client, `mpv --hwdec=vaapi --vo=gpu
--gpu-api=opengl` reports `Using hardware decoding (vaapi)` and a
`vaapi[yuv420p]` video output; the GL video output is required because mpv's
default Vulkan output (and `--use-angle=vulkan` in Chromium) hits venus dma-buf
format-modifier failures. `--waypipe` remains mutually exclusive with
`--wayland`.

## Chromium in the guest

The image ships `cang-chromium` (`tools/cang-chromium/`, embedded by
`nix/image/layers.nix`), which starts Chromium with that environment and the flags
the guest path needs, in one of two modes:

```bash
cang --gpu=drm            -- cang-chromium headless https://example.com
cang --gpu=drm --waypipe  -- cang-chromium gui      https://example.com
```

`headless` needs no display; `gui` opens a window on the guest's Wayland display,
which in a cang guest is the one `--waypipe` provides (presented on the host's own
compositor). `cang-chromium --check` reports the render node, the Vulkan/EGL/VA
driver paths, the guest driver's VA-API profiles and what Chromium actually got,
and exits non-zero unless WebGL renders on the GPU and the driver advertises H.264
decode. A guest started without `--gpu=drm` has none of that — no DRM render node,
no venus ICD, no VA driver — so the launcher explains that and exits instead of
starting a software-rendered browser. `tools/cang-chromium/README.md` has the
environment table and the per-flag rationale (`--disable-vulkan-surface`, why
`gui` must not pass `--enable-features=Vulkan`, and so on).

## Embedding models on the guest GPU (`zg`)

The image's `zg` (zvec-grep's Rust build) carries llama.cpp's **Vulkan** backend,
so its GGUF embedding models can run on the guest's GPU instead of the CPU. This
is not a marginal optimisation: measured on the pinned revision with the image's
own binary, indexing 24 k tokens of Rust source

| model | device | 6 k tokens | 24 k tokens | marginal |
|---|---|---|---|---|
| `local/embeddinggemma-300m` | CPU (28-core Zen4) | 92 s (66 t/s) | 345 s (70 t/s) | 72 t/s |
| `local/embeddinggemma-300m` | Vulkan (RDNA3, RADV) | 18 s (334 t/s) | 26 s (949 t/s) | 2 454 t/s |
| `local/qwen3-embedding-0.6b` | CPU | 200 s (30 t/s) | — | ~30 t/s |
| `local/qwen3-embedding-0.6b` | Vulkan | 15 s (393 t/s) | 30 s (821 t/s) | 1 288 t/s |

Per indexed token that is ~34× the CPU; whole-run wall clock is 5–13× because a
fresh process pays ~10–15 s of Vulkan startup (ggml keeps no pipeline cache on
disk, so every process recompiles its SPIR-V shaders). Short single-query
searches are therefore at parity — the GPU wins on bulk indexing, which is what a
first `zg --index` over a workspace is.

### The venus memory window

A venus device owns no memory to hand out: the renderer serves every
`vkMapMemory` of host-visible device memory by mapping the host driver's exported
dma-buf over a slice of one guest shared-memory window
(`virtio_gpu::resource_map_blob`), so that window — not the 8 GiB device-local
heap the ICD advertises — bounds the **cumulative** size of live mappings. cang
sizes it at 4 GiB (`GPU_SHM_SIZE_BYTES` in
`crates/cang/src/runtime/vm/libkrun/launcher.rs`), and `--gpu=drm:shm=SIZE` (MiB
bare, or `K`/`M`/`G` suffixes) overrides it per run — for A/B measurement, or to
raise the ceiling for a workload with more weight buffers. Sizes below 1 GiB are
allowed but logged: they are the regime where a Vulkan backend falls back to the
CPU.

| window | `qwen3-embedding-0.6b`, 15 files / 8.8 k lines |
|---|---|
| 256 MiB (before) | mapping the model's 633 MiB of weight buffers fails with `VK_ERROR_OUT_OF_DEVICE_MEMORY` (live mappings cap out at ~192 MiB) → CPU fallback |
| 4 GiB (now) | model stays on venus, 1 m 30 s |

The same corpus on the CPU, same model, took 44 m 28 s — the window is worth ~30×.
The failure is not loud: `zg` logs `ggml_vulkan: Device memory allocation of size
… failed` and `falling back to CPU`, then indexes with the GGUF model on the CPU
(~30 t/s instead of ~800 t/s), so the symptom is a workspace that indexes in
minutes instead of the tens of seconds the table above promises. The window costs guest *address space*, not guest
RAM — `MemTotal` and what the balloon can reclaim are unchanged, and a blob
mapping replaces the window's pages with the dma-buf's.

There is no way around the window from the guest side: a venus device reports no
exportable buffer handle types at all (`compatibleHandleTypes = 0` for
`OPAQUE_FD`, `DMA_BUF` and `HOST_ALLOC`, measured in a `--gpu=drm` guest), so a
client cannot ask for memory the renderer would map back into it. Growing the
window is the lever; `VK_EXT_external_memory_host` would need the renderer to
import guest pointers, which is a renderer-side project of its own.

The renderer side does steer the client away from that path. Since the pinned
virglrenderer revision `50282235`, venus **spoofs memory types** when the host
driver's first type is both `DEVICE_LOCAL` and `HOST_VISIBLE` (the UMA case an
APU presents): type 0 becomes `DEVICE_LOCAL`-only and type 1 loses
`DEVICE_LOCAL`, with every guest index shifted by one on the way to the host
driver. A client that allocates for an optimal-tiling image with "the first
type" therefore stops asking for an exportable allocation it would then map
through the window. `VKR_DEBUG=no_mem_type_spoof` turns it back off, and
`VKR_HEAP_MEMORY_PERCENT=<1-100>` shrinks the `memoryHeaps` a guest is shown
(the same revision) if a guest should be kept away from the host's heap limit.

`tools/zg-vulkan-cang-smoke/` is the live check for all of it: it boots a
`--gpu=drm` guest, indexes a fixed corpus with `zg` on venus and the qwen3 GGUF
model, and fails on `falling back to CPU`, an out-of-device-memory allocation, or
a run that embedded the corpus at under 100 tokens/s - plus, host-side, that the
`libvirglrenderer.so.1` doing the work is the one `.#virglrenderer` installed. It
indexes a fixed 64 KiB slice of this repository's own sources, so its throughput is
comparable from one commit to the next: ~344 t/s whole-run on venus against ~740 t/s
for the same binary on the host's RADV, both measured 2026-11-05 and recorded in
`tools/zg-vulkan-cang-smoke/README.md`.

Which model and device a guest uses is decided at boot by `cang-guest-init`
(`crates/cang-guest-init/src/guest_init/components/zg.rs`):

| guest | `ZVEC_GREP_EMBEDDING` | `ZVEC_GREP_DEVICE` |
|---|---|---|
| `cang --gpu=drm` and `vulkaninfo --summary` reports a hardware device | `local/qwen3-embedding-0.6b` | `auto` |
| anything else | `local/potion-code-16m-v2` | `cpu` |

The `vulkaninfo` probe is what keeps the GPU default honest. A `--gpu=drm` guest is
not proof of a usable Vulkan device — a venus device can be present in `/dev/dri`
and still enumerate zero physical devices — and the GGUF model on the CPU is far
slower than the Model2Vec model it would replace, so a guest whose probe fails
(including one that only sees llvmpipe/lavapipe) keeps the CPU model. The probe is
bounded (10 s) and its failure is silent: `--gpu=drm` alone does not change what
`zg` uses.

An explicit `--embedding`/`--device` still wins, and `auto` falls back to the CPU
when no GPU device is usable, so the GPU default cannot lose the device entirely.
One consequence worth knowing: an index stores the model it was built with, so a
workspace first indexed in a GPU guest and then opened in a CPU guest (or the
reverse) reports `model references differ` and wants `zg --index --rebuild`.

The models themselves are pre-installed, not downloaded at first use: the image
ships `local/potion-code-16m-v2` (32 MB Model2Vec) and
`local/qwen3-embedding-0.6b` (610 MiB GGUF, `nix/pkgs/zg-models.nix`), pinned by
the digests from zg's own catalog. guest-init links them into
`~/.zvec-grep/models`, which stays a writable directory — anything else the user
indexes with still downloads there.

Inside a `--gpu=drm` guest the ICD is mesa's **venus**, not a host driver: the
device appears through virtio-gpu, and it reports `KHR_coopmat` without
`coopmat2`, so the GPU path is the portable one. Changing the model also means
changing the index (see above) — pick a model per workspace, or a model per
guest shape, but not both.
