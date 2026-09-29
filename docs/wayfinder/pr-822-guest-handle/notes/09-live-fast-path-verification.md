# 09 - live fast-path verification: progress (2026-09-29)

Ticket `tickets/09-live-fast-path-verification.md`. **The guest side of the
fast path engages; the run then stalls in the guest proxy, so the measured A/B
is not yet possible.** Evidence, the two permission gates found on the way, and
exactly what could not be shown are below.

## Harness

- cang: the tree build `/nix/store/5ybxzdcwqwj4x0py7s9yv4dfmxmix5dv-cang-0.9.1`
  (plus `731fc0d`'s README/guest-init changes, which are guest-side only).
- firmware: the locally built `libkrunfw-v5.6.2-cang.1-local` (ticket 05), via
  `CANG_LIBKRUNFW_LIBRARY`.
- image: `nix build path:$PWD#container` (`/nix/store/cvi3s4jv3x954sdzkqyavgyxh0xxfkgk-cang.tar.gz`),
  loaded into a hermetic btrfs graphroot on the loop image.
  **A stale image silently has no fast path**: the proxy is an in-image store
  path, and the first runs here used an older archive whose
  `wl-cross-domain-proxy` predates upstream PR #24 - it never calls
  `import_memfd_via_udmabuf` at all. Always check the message:
  `readlink /proc/<proxy-pid>/exe` plus
  `grep -ac 'udmabuf fast path' <that binary>` (1 in a current image).
- guest-init: `--guest-init <cang-musl>/bin/cang-guest-init` from the tree (the
  image's own guest-init predates the udmabuf fix below).
- host compositor: `weston --backend=headless --socket=wl-guest`, with
  `XDG_RUNTIME_DIR`/`WAYLAND_DISPLAY` in cang's environment, which the render
  server child inherits - the cross-domain path needs somewhere to land.
- launch shape: `cang --mem 4 --gpu=drm --wayland [--zero-copy-shm]
  --alloc hardened --seccomp=off --landlock=off -- sh /workspace/ab2.sh`, where
  `ab2.sh` prints the guest surface, attaches `strace -f` to the proxy
  (`-e trace=openat,ioctl,fcntl`), runs
  `tools/wl-shm-bench/wl-shm-bench.py --seconds 6 --width 1920 --height 1080
  --rate 120`, and prints the strace summary.

## Evidence chain (fast-path run)

Guest, before the run: `/dev/udmabuf` is `crw-rw-rw- root root`; opening it as
the task user succeeds; `DRM_IOCTL_VIRTGPU_GETPARAM` on `/dev/dri/renderD128`
answers `3D_FEATURES=1`, `RESOURCE_BLOB=1`, **`CREATE_GUEST_HANDLE` (10) = 1**;
`/sys/bus/virtio/devices/virtio0/features` is the bitstring
`1111101100000000000000000000000010000000000000000000000000000000`, i.e. bits
0-4 (`VIRGL`, `EDID`, `RESOURCE_BLOB`, `HOST_VISIBLE`, `FENCE_PASSING`) plus
**6 `CREATE_GUEST_HANDLE`** and **7 `BLOB_CTX_ID_FIX`** plus `VERSION_1` (bit 32).

Proxy (the same run, strace), in order:

```
openat("/dev/dri/renderD128", O_RDWR|O_CLOEXEC) = 10
ioctl(10, DRM_IOCTL_VIRTGPU_GETPARAM, ...) = 0        # the feature probe
fcntl(12, F_GET_SEALS) = 0                            # the wl_shm pool memfd
fcntl(12, F_ADD_SEALS, F_SEAL_SHRINK) = 0
ioctl(11, UDMABUF_CREATE, {memfd=12, flags=0, offset=0, size=8294400}) = 13
ioctl(10, DRM_IOCTL_PRIME_FD_TO_HANDLE, ...) = 0      # the kernel's guest-handle import
ioctl(10, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, ...) = 0
```

`size=8294400` is exactly the client's pool (1920x1080x4), the `PRIME_FD_TO_HANDLE`
is the PRIME-import path that stamps `BLOB_FLAG_CREATE_GUEST_HANDLE`, and there is
**no `PRIME_HANDLE_TO_FD` and no `create_sharable_blob`** - the copy path's
signature - anywhere in the run. So the guest half of PR 822 is live: the pool
becomes a udmabuf, the kernel imports it as a guest-handle blob.

Control run, same harness without `--zero-copy-shm`: param 10 = 0, feature bits
`[0,1,2,3,4,32]`, the proxy opens `/dev/udmabuf` but makes **no `UDMABUF_CREATE`**
and no seal `fcntl`, and the pool goes through `PRIME_HANDLE_TO_FD` (the copy
path). The gate is exactly the host's advertisement, as designed.

### Trap: grepping the strace for the raw ioctl number

`strace` decodes `UDMABUF_CREATE` symbolically, so a `grep '0x75, 0x42'` for
`_IOW('u', 0x42, struct udmabuf_create)` reads 0 even when the call is right
there. Grep for `UDMABUF_CREATE`.

## The blocker: the proxy stalls after the import

With `--zero-copy-shm` the guest proxy never answers the client's first
`wl_display.sync`, and stays in a `DRM_IOCTL_VIRTGPU_EXECBUFFER` retry loop (the
same pointer, thousands of calls per second) for as long as the client waits:

```
warmup=no_sync_reply (no wl_display.sync reply within 5.0s)
```

The benchmark's commits are asynchronous, so the loop still runs three ways, and
the A/B (same client, same rate, only the flag differs) comes out:

| run | frames | fps | client cpu | guest busy cpu | warm-up |
|---|---|---|---|---|---|
| copy (no flag) | 720 | 120.00 | 0.09 s | 2.94 s (49.0%) | roundtrip ok |
| fast (`--zero-copy-shm`) | 720 | 120.00 | 0.05 s | 3.68 s (61.3%) | **no sync reply** |

The fast-path run is *slower* because the proxy is spinning, not because the path
is expensive: it is stuck, so the numbers cannot be read as a fast-path cost.
Nothing here says the pool copy was cheaper or dearer - only that the stack does
not reach a steady state with the feature on.

Not yet diagnosed, and the split between the layers is the next step: the guest
proxy (upstream PR #24 code, unchanged here) after `import_memfd_via_udmabuf`,
the fork's blob arm for a `CREATE_GUEST_HANDLE` blob on the cross-domain context
(`a1a772a0` - the mis-route warning would have been visible at the `warn` floor
and was not), or the host compositor's handling of the imported dma-buf. A
worker-side libkrun log capture would separate them first: the supervisor does
not forward the VM worker's stderr to the console on a successful run.

## Two permission gates found on the way

1. **Host.** `--zero-copy-shm` needs the host's `/dev/udmabuf` openable from
   inside cang's keep-id user namespace, where the probe runs. This host had it
   `crw-rw---- root kvm`, whose gid is unmapped there, and the invoking user is
   not in `kvm` either - `/dev/kvm` works because it is `crw-rw-rw-`. With
   `chmod 0666 /dev/udmabuf` the bit is advertised and param 10 answers 1.
2. **Guest.** devtmpfs creates the guest's node `crw------- root root`, and the
   proxy runs as the task user: measured `Permission denied`, after which the
   proxy silently took the copy path for every pool. `57fc4ec` has guest-init
   prepare the node like the DRM render node when `--gpu=drm`/`--wayland` is on.

## Regression backstop: the Chromium GPU smoke PASSES

`tools/chromium-cang-smoke/chromium-smoke.sh` on the current tree (container
rebuilt from it, guest-init override from `.#cang-musl`, image loaded into a
fresh btrfs graphroot), default `--gpu=drm` shape, no fast path:

```
PASS  version (Chromium version)
PASS  chromium-rc (the WebGL and probe-DOM runs exit 0)
PASS  webgl-vulkan (ANGLE/Vulkan renderer, not SwiftShader)
PASS  webgl-png (non-empty PNG screenshot)
VERDICT: PASS - evidence: .../workspace/evidence (fresh, 14 files)
```

renderer: `ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT
(RADV NAVI33)) (0x00007480)), venus)`. So the fork port did not disturb the
copy path or the venus renderer.

## What could not be shown here

- **A completed fast-path run**, and therefore the measured delta. The guest
  side engages (above) and the proxy then stalls; that stall is the specific
  gate still blocking the destination.
- **Chromium on the fast path**: it needs the same proxy, so it is behind the
  stall.
