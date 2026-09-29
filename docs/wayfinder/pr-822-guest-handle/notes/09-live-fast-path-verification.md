# 09 - live fast-path verification (2026-09-29)

Ticket `tickets/09-live-fast-path-verification.md`. **The fast path is live end to
end**: a guest `wl_shm` client through `wl-cross-domain-proxy` takes
`CREATE_GUEST_HANDLE` (the pool becomes a udmabuf, the kernel imports it as a
guest-handle blob, no copy ioctl anywhere) and the run completes, with a
measurable A/B against the copy path.

Getting there took one fork fix (ticket 12) and exposed two permission gates,
both recorded below with the harness.

## Harness

- cang: the tree build; firmware: `libkrunfw-v5.6.2-cang.1-local` (ticket 05) via
  `CANG_LIBKRUNFW_LIBRARY`; guest-init: `--guest-init <cang-musl>/bin/cang-guest-init`
  from the tree (the image's own guest-init predates the udmabuf fix below).
- image: `nix build path:$PWD#container`, loaded into a hermetic btrfs graphroot.
  **A stale image silently has no fast path**: the proxy is an in-image store
  path, and one earlier archive predated upstream PR #24, so it never calls
  `import_memfd_via_udmabuf` at all. Check
  `readlink /proc/<proxy-pid>/exe` plus `grep -ac 'udmabuf fast path'` on that
  binary (1 in a current image).
- host compositor: `weston --backend=headless --socket=wl-guest`, with
  `XDG_RUNTIME_DIR`/`WAYLAND_DISPLAY` in cang's environment (the render server
  child inherits them), because the cross-domain path needs somewhere to land.
- launch shape: `cang --mem 4 --gpu=drm --wayland [--zero-copy-shm] --alloc
  hardened --seccomp=off --landlock=off -- sh /workspace/ab2.sh`, where `ab2.sh`
  prints the guest surface, attaches `strace -f -e trace=openat,ioctl,fcntl` to
  the proxy and runs
  `tools/wl-shm-bench/wl-shm-bench.py --seconds 6 --width 1920 --height 1080
  --rate 120`.
- the VM worker's own log (libkrun's `log` records, the fork's warnings) is at
  `<task-state-dir>/helper.stderr.log` when the run uses `--preserve-debug`; on a
  successful run nothing is replayed to the console.

## Evidence chain (fast-path run)

Guest: `/dev/udmabuf` is `crw-rw-rw-` and opens as the task user;
`DRM_IOCTL_VIRTGPU_GETPARAM` answers **`CREATE_GUEST_HANDLE` (10) = 1**;
`/sys/bus/virtio/devices/virtio0/features` is the bitstring
`1111101100000000000000000000000010000000000000000000000000000000`, i.e. bits
0-4 plus **6 `CREATE_GUEST_HANDLE`**, **7 `BLOB_CTX_ID_FIX`** and `VERSION_1`.

Proxy (strace), in order:

```
openat("/dev/dri/renderD128", O_RDWR|O_CLOEXEC) = 10
ioctl(10, DRM_IOCTL_VIRTGPU_GETPARAM, ...) = 0        # the feature probe
fcntl(12, F_GET_SEALS) = 0                            # the wl_shm pool memfd
fcntl(12, F_ADD_SEALS, F_SEAL_SHRINK) = 0
ioctl(11, UDMABUF_CREATE, {memfd=12, flags=0, offset=0, size=8294400}) = 13
ioctl(10, DRM_IOCTL_PRIME_FD_TO_HANDLE, ...) = 0      # the guest-handle import
ioctl(10, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, ...) = 0
```

Counts over the whole run: `UDMABUF_CREATE`=1, `PRIME_FD_TO_HANDLE`=1,
**`PRIME_HANDLE_TO_FD`=0** (the copy path's signature) for the flag-on run, and
exactly the mirror image for the flag-off run (`UDMABUF_CREATE`=0,
`PRIME_FD_TO_HANDLE`=0, `PRIME_HANDLE_TO_FD`=1). The pool size in the ioctl is
the client's 1920x1080x4 pool byte for byte.

### Trap: grepping strace for the raw ioctl number

`strace` decodes `UDMABUF_CREATE` symbolically, so a `grep '0x75, 0x42'` for
`_IOW('u', 0x42, struct udmabuf_create)` reads 0 even when the call is right
there. Grep for the symbolic name.

## The measured A/B

Same image, guest-init, firmware, client, pool and rate; only the flag differs:

| run | param 10 | warm-up | frames | fps | client cpu | guest busy cpu | guest cpu/frame |
|---|---|---|---|---|---|---|---|
| copy (no flag) | 0 | ok | 720 | 120.00 | 0.100 s | 2.800 s (46.7%) | 3888.9 us |
| fast (`--zero-copy-shm`) | 1 | ok | 720 | 120.00 | 0.090 s | **2.650 s (44.2%)** | **3680.6 us** |

So the fast path is measurably cheaper at a fixed rate: **-5.4% guest busy CPU**,
i.e. the copy handler's `memcpy(stride*height)` per commit is gone while the rest
of the proxy's per-commit work (the cross-domain submit that reaches the host
compositor) is unchanged. The delta is modest by construction - at 1920x1080 and
120 fps the removed copy is only ~1 GB/s - which is exactly why the ticket asked
for a measurement instead of an assumption.

## What blocked it, and the fix (ticket 12)

The first fast-path run reached the pool's `UDMABUF_CREATE` and the PRIME import
and then stalled: the proxy never answered the client's `wl_display.sync` and
spun on `VIRTGPU_EXECBUFFER`. The worker log, once captured, said:

```
WARN krun_devices::virtio::gpu::virtio_gpu] Failed to create udmabuf for resource 4:
  system call returned EINVAL: Invalid argument (entries=2025, bytes=8294400,
  first=Some((GuestAddress(4602589184), 4096)))
DEBUG krun_devices::virtio::gpu::worker] Some(ResourceCreateBlob) -> ErrUnspec
```

The guest names one dma-buf entry per 4 KiB page, so an 8 MiB pool arrives as
**2025 runs**; the udmabuf driver's `list_limit` is **1024** and
`UDMABUF_CREATE_LIST` rejects more with a bare `EINVAL`. The device turned that
into `ErrUnspec` - correct per ticket 04's "fail loudly" decision - but the guest
proxy had already committed to zero-copy and simply retried.

`63f3737f` merges adjacent runs in the same memfd before the ioctl (a contiguous
pool becomes one item), refuses requests still over the limit with a named error,
and prints the entry count/bytes/first address in the warning. The host kernel's
contract was measured directly on `/dev/udmabuf` for the record:

| request | result |
|---|---|
| 1 run x 8 MiB, `F_SEAL_SHRINK` | ok |
| 1 run x 8 MiB, no seals / `GROW` only / `SHRINK|GROW|WRITE` | `EINVAL` |
| 1024 runs x 4 KiB | ok |
| 2025 / 2048 runs x 4 KiB | `EINVAL` (`list_limit`) |
| 1 run x 128 MiB | `EINVAL` (per-dmabuf `size_limit_mb`, default 64 MiB) |
| list `flags=0` and `flags=UDMABUF_FLAGS_CLOEXEC` | both ok |

## Two permission gates found on the way

1. **Host.** The probe opens `/dev/udmabuf` inside cang's keep-id user namespace.
   This host had it `crw-rw---- root kvm`, whose gid is unmapped there, and the
   invoking user is not in `kvm` either - `/dev/kvm` works because it is
   `crw-rw-rw-`. With `chmod 0666 /dev/udmabuf` the bit is advertised and param
   10 answers 1.
2. **Guest.** devtmpfs creates the guest's node `crw------- root root` and the
   proxy runs as the task user: measured `Permission denied`, after which the
   proxy silently took the copy path for every pool. `57fc4ec` has guest-init
   prepare the node like the DRM render node on `--gpu=drm`/`--wayland` runs.

## Regression backstop: the Chromium GPU smoke PASSES

`tools/chromium-cang-smoke/chromium-smoke.sh` on the same tree (container rebuilt
from it, guest-init override from `.#cang-musl`, fresh btrfs graphroot), default
`--gpu=drm` shape, no fast path: `PASS version`, `PASS chromium-rc`,
`PASS webgl-vulkan`, `PASS webgl-png`, `VERDICT: PASS` (14 fresh evidence files),
renderer `ANGLE (AMD, Vulkan 1.4.334 (Virtio-GPU Venus (AMD Radeon RX 7600M XT
(RADV NAVI33)) (0x00007480)), venus)`. The fork port neither disturbed the copy
path nor the venus renderer.

## Not shown here

- A guest *application* other than the benchmark on the fast path (Chromium in a
  `--gpu=drm` run does not go through the `wl_shm` proxy path, so the smoke is a
  regression backstop rather than fast-path evidence).
- Anything about `--zero-copy-shm` beyond `wl_shm` pools: the balloon trade and
  the file-backed RAM are ticket 10's analysis, and the file-backing is only
  exercised here in that the run works with it on.
