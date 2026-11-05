# zg Vulkan Cang Live Smoke

A live smoke for **`zg` on the guest GPU**: that the image's `zg` indexes a
workspace with its GGUF embedding model on the guest's Vulkan device — mesa's
venus through virtio-gpu — and not silently on the CPU.

The failure this exists for is quiet. When the venus memory window is too small
for a model's weight buffers, `zg` logs
`ggml_vulkan: Device memory allocation of size … failed`, warns
`llama.cpp vulkan GPU backend is unavailable, falling back to CPU`, and then
indexes with the GGUF model on the CPU — minutes per workspace instead of tens of
seconds, at an exit status of 0. Nothing about the run looks broken without
reading those lines or timing it (`docs/graphics-audio.md`, "The venus memory
window").

The corpus is a fixed-size slice of **this repository's own Rust sources**, and
the run is scored on device, fallback, retrieval and throughput — so the number it
prints is `zg`'s GPU embedding performance on the codebase itself, comparable from
one commit to the next.

## Recreate

```bash
# cang's only implemented task-rootfs backend is btrfs-snapshot, so the output
# (hermetic podman graphroot + cang state) has to be on a btrfs filesystem
# (/home/dev/disp/disk on this host; no loop image needed):
nix develop --command tools/zg-vulkan-cang-smoke/zg-vulkan-smoke.sh \
  --out-dir /home/dev/disp/disk/zg-vulkan-cang
```

Everything is resolved from the flake (`.#cang`, `.#cang-musl`'s guest-init,
`.#container`, `.#virglrenderer`), so no arguments are needed on a host that has
the prerequisites. To reuse an image that is already loaded (the load is the slow
part), pass its reference and its store:

```bash
CONTAINERS_STORAGE_CONF=/path/to/store/storage.conf \
  nix develop --command tools/zg-vulkan-cang-smoke/zg-vulkan-smoke.sh \
  --container localhost/cang:latest \
  --out-dir /home/dev/disp/disk/zg-vulkan-cang
```

## What it does

1. **Preflight** resolves every input and refuses to start on anything missing:
   `/dev/kvm`, a `/dev/dri/renderD*` node, a btrfs `--out-dir`, 12 GiB free
   there — each a hard failure naming the `nix build` command, never a silent
   skip. It also resolves `.#virglrenderer` and checks that cang's
   `libexec/cang-helpers/virgl_render_server` symlink resolves inside it, so a
   cang built against a different virglrenderer fails before anything boots. For
   a raw-ELF `.#cang` it sources `.#cang-render-server-env` (the same rule the
   other GPU smokes use; `.#cang-prebuilt`'s wrapper sets those itself).
2. **One hermetic store**: a fresh btrfs-driver podman storage under `--out-dir`
   (the ambient `~/.config/containers/storage.conf` is never read), the image
   loaded once, and an isolated `<out-dir>/config/cang/cang.toml` pinning
   `[state].location` and `backend = "btrfs-snapshot"`.
3. **A corpus cut from this repository**: `--corpus-bytes` bytes (default 64 KiB)
   of the working tree's Rust sources under `--corpus-path` (default
   `crates/cang/src`), each file contributing at most an eighth of the budget so
   the slice is a spread of files rather than the first big one. The code under
   test and the text it embeds are therefore the same codebase, and the fixed
   budget is what keeps the throughput figure comparable from one commit to the
   next: the sources change, the amount of them does not.
4. **One `--gpu=drm` VM** running `smoke/run-guest-zg-vulkan.sh`, with an
   attribution sampler reading `/proc/<pid>/maps` while it is up:
   `cang --mem 4 --gpu=drm --alloc hardened --seccomp=off --landlock=off
   --guest-init <musl> -- sh /workspace/smoke/run-guest-zg-vulkan.sh`.
5. **Scores** the evidence and prints `VERDICT: PASS|FAIL`, exiting 0 only on
   PASS.

The guest has no environment channel (cang passes PATH plus its own allowlist),
so the model, device and index timeout reach it as the staged file
`/workspace/smoke/zg-params`, and every client — `zg`, `vulkaninfo` — is the
**image's** (agent layer). A host store path cannot run in the guest: its
`/nix/store` is the image's closure.

## The guest arm

| step | command shape | why |
| --- | --- | --- |
| device probe | `vulkaninfo --summary` | the guest's own answer to "is there a Vulkan device", the same probe guest-init runs to pick the GPU model |
| model check | `find ~/.zvec-grep/models -name '*.gguf'` | the GGUF has to be the one guest-init linked out of the image: the guest has no network, so a `zg` that decides to download 610 MiB fails here instead of after minutes |
| index | `zg --index /workspace/corpus --embedding local/qwen3-embedding-0.6b --device vulkan`, bounded by `--index-timeout` | the run itself; `RUST_LOG=zg_engine=debug` makes the backend choice a log line |
| status | `zg --status /workspace/corpus --check-ready` | the index is ready, at 100% coverage, on the GPU model |
| query | `zg "<identifiers unique to one staged file>"` | the end-to-end path: the query is embedded through the same model, and the file those identifiers belong to has to come back in the top 3 |

**Checks.**

| check | evidence | predicate |
| --- | --- | --- |
| `virglrenderer-helper` | preflight | the cang render-server helper resolves under `.#virglrenderer` |
| `zg-vm-exit` | `<out>/logs/cang-zg.console` | the guest ran to completion |
| `zg-guest-steps` | `evidence/zg-rc.txt` | `vulkaninfo`, `index`, `status` and `query` all recorded `=0` (a missing line fails: the guest died before writing it) |
| `zg-vulkan-device` | `evidence/zg-vulkaninfo.txt` | a `deviceName` that is not `llvmpipe`/`lavapipe`/`swiftshader`/`softpipe` |
| `zg-ggml-device` | `evidence/zg-index.log` | a `ggml_vulkan: <n> = <device> (<driver>)` line naming a non-software device |
| `zg-backend-vulkan` | `evidence/zg-index.log` | `GPU embedding backend selected` with `backend=Vulkan` and `gpu=Vulkan` |
| `zg-no-cpu-fallback` | `evidence/zg-index.log` | none of `falling back to CPU`, `vulkan GPU backend is unavailable`, `VK_ERROR_OUT_OF_DEVICE_MEMORY`, `Device memory allocation of size`, `Failed ggml_vk_create_buffer`, `Failed to allocate pinned memory` |
| `zg-model-local` | `evidence/zg-model.txt` | the GGUF resolves into `/nix/store` (the image's model, not a download) |
| `zg-no-download` | `evidence/zg-index.log` | no `Downloading model` line |
| `zg-index-ready` | `evidence/zg-status.txt` | 100% coverage with `local/qwen3-embedding-0.6b` |
| `zg-query-hits` | `evidence/zg-query.txt` | the file whose unique identifiers formed the query appears in the top 3 hits |
| `zg-throughput` | `evidence/zg-index.log` | the corpus embeds at no less than `--min-tokens-per-second` (default 100) |
| `virglrenderer-loaded` | `<out>/logs/attribution.txt` | every sampled `libvirglrenderer.so.1` mapping is under `.#virglrenderer` |

**Why both the device line and the throughput floor.** The device lines catch the
fallback the moment it happens; the floor catches a run that took the GPU path and
still did the work an order of magnitude too slowly (or a zg whose fallback
warning moved).

Throughput is reported in embedding tokens per second, the unit
`docs/graphics-audio.md` uses. Nothing in `zg`'s output counts tokens, so the count
is derived from the corpus's byte count: this repository's Rust sources tokenize at
4.10 chars per Qwen3 token (measured with the Qwen3 BPE tokenizer over a 64 KiB
slice of `crates/cang/src`), which the runner records as `CHARS_PER_TOKEN_100=410`.
For the default corpus that is 65 536 bytes / 1 972 lines / ~15 984 tokens over 19
files.

Measured 2026-11-05 on this host (RX 7600M XT / RADV, 28-core Zen4), same corpus
and same `zg` 0.3.0 binary in both GPU arms:

| arm | duration | throughput |
| --- | --- | --- |
| host RADV, `--device vulkan` (warm) | 21.5 s, 21.7 s | ~740 t/s |
| `cang --gpu=drm` guest, venus (3 runs) | 46 s, 47 s, 45 s | ~344 t/s |

The guest pays roughly 2x the host RADV wall clock: every `vkMapMemory` of the
model's weight buffers is served by the renderer over the venus window, and a fresh
process recompiles llama.cpp's 155 SPIR-V pipelines through venus, which also has no
on-disk cache to reuse. The host row is a reference for what the same binary does on
the same corpus without the guest GPU path - it is not a second arm of the smoke,
which runs on Vulkan only.

Both GPU figures are whole-run numbers, so they are floors for this corpus size: a
process pays model load plus pipeline compilation regardless of how much text it
embeds. Raise `--corpus-bytes` to approach the marginal rate.

**Why the query is scored at all.** An index that is built but unusable — the
model reference stored differently from the one the query resolves — still exits
0 and still writes an index. `zg-query-hits` is the only check that reads the
result of the embeddings rather than their side effects.

## Prerequisites

- `/dev/kvm` and a `/dev/dri/renderD*` node accessible to the user.
- `podman`, `buildah`, `script` (util-linux) and `nix` on PATH (the dev shell
  provides them).
- **A btrfs `--out-dir`.** `btrfs-snapshot` is the only implemented task-rootfs
  backend and it snapshots the Buildah graphroot; `fuse-overlay` is not
  implemented. On a non-btrfs output the smoke exits 2 up front. On this host
  `/home/dev/disp/disk` is btrfs and already mounted.
- **Disk space**: the image unpacks to ~7.7 GiB per hermetic store; the smoke
  keeps a 12 GiB free-space preflight on the output filesystem.
- A finished run cannot be removed with a plain `rm -rf` (rootless podman owns
  the layer subvolumes through its subuid mapping): clean it inside that mapping
  with `podman unshare chmod -R u+w <out-dir> && podman unshare rm -rf <out-dir>`.

## Triage

- **`zg-no-cpu-fallback` fails with `Device memory allocation of size … failed`**:
  the venus window is too small for the model. It is 4 GiB by default and
  `--gpu=drm:shm=SIZE` overrides it per run; the model's weight buffers are
  ~633 MiB, but the window bounds the *cumulative* live mappings
  (`docs/graphics-audio.md`).
- **`zg-ggml-device` fails with `ggml_vulkan: No devices found`**: the guest's
  Vulkan stack never reached venus. Read `evidence/zg-diag.txt` (the `VK_*`
  environment, the ICD manifests) and `evidence/zg-vulkaninfo.txt`; a guest
  started without `--gpu=drm` has no venus ICD at all.
- **`zg-model-local` fails**: the image's `~/.zvec-grep/models` link is dangling —
  a collected GC root, so the image build has to be rooted (`nix build
  .#container -o <path outside the store>`) and kept.
- **`zg-throughput` fails while every device line passes**: the run used venus but
  spent the time somewhere else — check the index log for a long
  shader-compilation phase, and whether `--min-tokens-per-second` is simply too
  aggressive for a cold host. `--corpus-bytes` sets the amount of text, so a much
  smaller corpus raises the share of the fixed startup cost.
- **`zg-query-hits` fails**: the corpus changed and no identifier is unique to the
  first staged file any more, or the index was built with a different model than
  the query resolved. `evidence/zg-query.txt` has the query and its hits,
  `evidence/zg-query-text.txt` the exact text.
- **A `zg` that stops and looks hung**: it is not. Under the managed PTY a guest
  process that reads stdin stops on `SIGTTOU`; every `zg` invocation here takes
  stdin from `/dev/null`.
