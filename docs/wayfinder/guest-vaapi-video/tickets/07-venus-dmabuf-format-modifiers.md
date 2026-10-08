---
label: wayfinder:research
title: Venus dma-buf format-modifier imports block Vulkan-presenting clients
status: open
blocked_by: []
claimed_by:
---

## Question

Vulkan clients in the guest fail on dma-buf format-modifier imports, so anything
that presents through venus aborts instead of drawing. Who owns the mismatch -
waypipe, venus, or the modifier the host's driver hands out - and can a guest
Vulkan client present through the Waypipe display at all?

Evidence (2026-10-02, `--gpu=drm --waypipe` guest):

- `mpv`'s **default** video output (`--vo=gpu`, `--gpu-api=auto` => Vulkan)
  aborts with `rc=134` in both hwdec arms, with the render server reporting
  `vkr: failed to query resource props: invalid res_id 15`,
  `vkGetMemoryResourcePropertiesMESA resulted in CS error` and
  `ring_submit_cmd: vn_dispatch_command failed` - before any decode happens;
  `--gpu-api=opengl` presents normally;
- the same wall was recorded earlier for Chromium (`vk_helpers.cpp initExternal`
  and waypipe's dmabuf import failing with
  `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`, the host compositor's
  AMD modifier leaking into the guest), and the chromium smoke's
  `tools/chromium-cang-smoke` blocks dmabuf on the waypipe side to avoid it;
- with `GBM_BACKENDS_PATH` fixed the guest now *does* allocate GBM buffers and
  Waypipe carries them as dma-bufs, so the transport half works; what fails is
  venus's import/query of those buffers.

Scoping questions for the first pass: which modifier the guest's driver
advertises for GBM/venus allocations and which one the host compositor hands out;
whether the failure is in venus's `vkGetMemoryResourcePropertiesMESA` path or in
the format-modifier list negotiation; and whether `waypipe`'s dmabuf path or a
guest-side compositor rendering with virgl (no venus) is the smaller fix.

## Acceptance

A Vulkan-presenting client in a `--gpu=drm --waypipe` guest that reaches its
first frame through the host compositor (mpv with its default video output, or
Chromium with `--use-angle=vulkan`), with the render server logging no
`invalid res_id`/CS error.

## First pass (2026-10-08, static analysis; no live repro - this box has no host compositor)

**Q1 - which modifiers?** The guest advertises the **host renderer's modifier list verbatim**:
`vn_wsi.c`'s `vn_wsi_init` sets `wsi_device.supports_modifiers` from the host's
`EXT_image_drm_format_modifier`, and `vn_physical_device.c`'s `vn_GetPhysicalDeviceFormatProperties2`
→ `vn_sanitize_format_properties` only masks YCbCr feature flags - it never intersects the list with
what *guest-allocatable* resources can back. So the guest tells its clients it supports tiled
modifiers (AMD GFX11 families, DCC variants) while the guest's GBM/virgl shared resources are
**LINEAR only** - `virglrenderer`'s pipe-resource layout reports LINEAR for them (cang's
`virglrenderer-gbm-layout-linear-modifier.patch`), and guest-init sets
`GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm`.

**Q2 - which layer fails?** The mpv abort is the **resource-properties query path**, not
modifier-list negotiation: guest `vn_GetMemoryFdPropertiesKHR` → `vn_get_memory_dma_buf_properties` →
`vn_renderer_bo_create_from_data`, with the modifier arriving as `0xffffffffffffffff`
(`DRM_FORMAT_MOD_INVALID`) - which is the same value `tools/chromium-cang-smoke/README.md:198-207`
records for the chromium flavour, and that README documents the chromium side as *fixed* by making the
guest publish the host's real layout as LINEAR. `can_import_image` passes before the properties query
in the mpv case, so the list itself is not what rejects.

**Q3 - smaller fix?** Waypipe's dmabuf path - one guest process, and cang already controls
`GBM_BACKENDS_PATH` and the venus ICD selection - rather than a guest compositor rendering through
virgl.

**Not established:** no live reproduction (no compositor on this box), and the guest waypipe's
`DmabufDevice` being the venus ICD is inferred from guest-init's `VK_ICD_FILENAMES` pin plus waypipe's
Vulkan usage.

**Next concrete step (no C change, discriminating):** re-run the captured `smoke-mp2` arm with the
guest's venus WSI forced LINEAR (`MESA_ENV` addition `VN_PERF=no_tiled_wsi_image`) and the waypipe
server started with `--test-skip-vulkan`, then require the render server to log neither
`invalid res_id` nor CS error and mpv to reach its first frame. That separates the
cross-context-resource explanation from the modifier one. Also check whether the image ships
`dri_gbm.so` at all and either point `GBM_BACKENDS_PATH` at it or add it to the image's mesa runtime.
It needs a host with a compositor (not this box).

## Experiment: the modifier theory is refuted; the failure is a venus ring hang (2026-10-08)

Ran the discriminating step on this box: headless weston 15.0.1 (host) + waypipe 0.11.0 client +
`cang --gpu=drm --waypipe` guest, presenting client = **mpv v0.41.0 from the image** with `--vo=gpu`
(waylandvk = Vulkan through venus), `--hwdec=no`:

| arm | variant | client rc | playback | host frame |
| --- | --- | --- | --- | --- |
| A | baseline `--vo=gpu` | **134** | no | blank |
| B | `VN_PERF=no_tiled_wsi_image` (LINEAR WSI) | **134** | no | blank |
| C | control `--gpu-api=opengl` | 124 (killed after playing) | **yes** | real video frame |
| D | baseline + `MESA_LOG_LEVEL=debug VN_DEBUG=wsi,result` | 134 | no | blank |
| E | `VN_PERF` + the same debug | 134 | no | blank |
| S | baseline under strace | 134 | no | blank |

The env knob *is* live - arm E logs `rejecting non-linear wsi image format modifier <0x…>` where arm D
logs `rejecting multi-plane (2/3) modifier …`, i.e. the WSI modifier policy really changes - and both
then create the swapchain (`vn_wsi_create_image` x3) and die in the identical place:

```
MESA-VIRTIO: debug: stuck in ring seqno wait with iter at 4096
MESA-VIRTIO: debug: aborting on expired ring alive status at iter 4096
```

**So the abort is a venus ring hang** (the host stops advancing the ring after swapchain creation),
not a dma-buf modifier or import failure. Under strace the venus `VIRTGPU_EXECBUFFER` ioctls all
return 0, then ~5 s of quiet, then a self-raised SIGABRT (`SI_TKILL`) - no failing ioctl.

Also settled by this run:

- **`dri_gbm.so` is shipped and pointed at** (`/usr/lib/cang-mesa-runtime/lib/gbm/dri_gbm.so`,
  in-guest `GBM_BACKENDS_PATH=/usr/lib/cang-mesa-runtime/lib/gbm`), so that half of the earlier
  proposal does not apply;
- **waypipe 0.11.0 has no `--test-skip-vulkan`** (host and image), so that half cannot be run;
- the 2026-10-02 `invalid res_id 15` / CS-error sequence was **not** reproduced on the current practice
  image - the failure today is a silent ring hang. The host-side absence of `vkr:` lines is *not*
  evidence either way: the shipped render server binary has no `VIRGL_LOG_FILE`/`VIRGL_LOG_LEVEL`
  support and the messages are INFO-level.

**Redirect:** ticket 07 is no longer a format-modifier ticket. The next step is to instrument the venus
ring/CS - build cang's virglrenderer with logging (cang can, and the marker recipe is known) and/or
bisect the submit that never completes - with the GL path (arm C) as the healthy control.

## Instrumentation attempt (2026-10-08): built, but did not converge

A delegated attempt built a marker-instrumented cang virglrenderer (markers at `vkr_context_set_fatal`,
`vkr_queue_sync_submit`, ring submit/thread paths - `insert_markers.py` + `t07c_mark.h` under
`/home/dev/cang/disk/nctx/t07c/`) and ran the well-known arms (headless weston + waypipe + mpv
`--vo=gpu`). What survives:

- **no marker line was ever captured.** That is the *known* trap, not evidence: the render server's
  stderr does not reach the VM console, and it may write to its own `/dev/shm`, so markers only count
  if they go to a file in the shared workspace *and* the pid is recorded. The attempt did not reach
  that point.
- a render-server liveness watcher (`rs-watch.log`) recorded, for the whole hang,
  `state=S wchan=do_sys_poll` together with `exit_code=17`. Whether that field means the render server
  had exited (making the guest's ring wait a *symptom* of the host having vanished) or is the watcher
  misreading `/proc/<pid>/stat` was **not** resolved.
- the marked build was reverted; the tree is clean and no VM/compositor leftovers remain.

**Now the decisive and cheap instrument** (much smaller than a vkr-marker build): cang takes the
render-server binary from `CANG_VIRGL_RENDER_SERVER` (crates/cang/src/runtime/host_tools.rs:50), so a
tiny wrapper that execs the real `virgl_render_server`, forwards its argv, and records
`wait`'s status/signal plus its stderr into the shared workspace answers the *first* question - does the
host venus process die, and with what - which decides whether this ticket is "the host venus context
faults" or "the host venus thread never advances a specific submit".

## The render server does NOT die - so the wrapper is unnecessary (2026-10-08)

The liveness ambiguity is resolved from the existing watcher output, without another run. A process that
has exited and is waiting to be reaped is in state **`Z`**; the watcher recorded the render server as
`state=S wchan=do_sys_poll` for the whole hang, i.e. **alive and idle in its event loop**. Therefore the
`exit_code=17` field the watcher printed for it was a misread of `/proc/<pid>/stat` (that field is only
meaningful once a process is dead), not evidence of an exit - and the wrapper planned as the next
instrument is not needed.

The picture is therefore: the guest submits (every `EXECBUFFER` returns 0), the host's venus process is
alive and polling, and yet the guest's ring wait expires. That is *not* "the host venus context
faulted"; it is either

1. a command that reached the host but was never dispatched to the ring thread (context/resource
   mismatch - the shape the 2026-10-02 `invalid res_id 15` evidence had), or
2. a ring thread blocked on a fence/syncobj the client side never signals.

Discriminating between those needs host-side visibility inside the venus ring thread, which means a
marker build whose output lands in the *shared workspace* (the earlier attempt's markers never fired,
which the notes explain: the render server's stderr does not reach the VM console and its own
`/dev/shm` is not the host's). That is the next instrument - and it is real work, not a wrapper.

## The host venus ring thread is blocked in a futex while the guest waits for its seqno (2026-10-08)

Re-ran the presenting arm (headless weston + waypipe + `cang --gpu=drm --waypipe` + `mpv --vo=gpu`,
clip 1080p) while sampling every thread of the host render server at 0.5 s intervals. Guest:

```
MESA-VIRTIO: debug: vn_GetPhysicalDeviceImageFormatProperties2: VK_ERROR_FORMAT_NOT_SUPPORTED   (many)
MESA-VIRTIO: debug: rejecting multi-plane (2)/(3) modifier … for wsi image with format 64
MESA-VIRTIO: debug: vn_wsi_create_image: legacy_scanout=0, prime_blit=0      (x3)
MESA-VIRTIO: debug: stuck in ring seqno wait with iter at 4096
MESA-VIRTIO: debug: aborting on expired ring alive status at iter 4096
```

(mpv then aborts, `rc=134`, ~9 s in.)

Host render server, same window:

| thread | wait |
| --- | --- |
| `virgl_render_se` (main) | `do_sys_poll` |
| **`vkr-ring-1`** | **`__futex_wait`** |
| **`vkr-ringmon-1`** | **`__futex_wait`** |
| `vkr-queue-1` | `__futex_wait` |
| `virgl_r:disk$0` | `__futex_wait` |
| vCPUs | `kvm_vcpu_block` |

Nothing anywhere is in a DRM `ioctl`/syncobj wait.

Interpretation: the guest has *written* a command and is polling for the ring seqno to advance, while
the host's **ring thread and its monitor are parked in futex waits** - i.e. the guest-to-host wake for
that command was never delivered (or the ring thread is waiting on a lock owned by another parked
thread). Because no thread sits in a DRM call, this is **not** a GPU fence that never signals; it is the
ring's own notification path. That is a much sharper statement of the defect than "the host stopped
advancing the ring", and it fits the earlier 2026-10-02 `invalid res_id 15` evidence only loosely.

Next: read the ring protocol itself - venus's `src/venus/vkr_ring.c` (how the ring thread waits and how
a submit wakes it, including the shared-memory futex and the ring's alive/seqno fields) against the
guest side (mesa's `vn_ring` in `src/virtio/vulkan/`), and check whether cang's guest-proxy /
zero-copy-shm path (`--gpu=drm`, the PR-822 work) is in that notification chain. A lost wake there would
explain both the guest's seqno wait and the host's parked ring thread.

### Which hop stalls (2026-10-08, from the same run)

Two rings are involved, and the thread names locate them:

1. the **guest's** venus ring against the VM's virtio-gpu (in guest RAM): the guest's message is about
   this one, and it is serviced by the **VM worker** (libkrun/rutabaga) - the same process that also
   runs vrend's GL threads (`cang:gl0`, `cang:gdrv0`, `cang:traceq0`, all `__futex_wait` during the
   hang);
2. the **render server's** own ring (`vkr-ring-1`, `vkr-ringmon-1`, `vkr-queue-1`, all `__futex_wait`,
   with the server's main thread in `do_sys_poll`) - the external venus context cang spawns with
   `virgl_render_server --socket-fd=…`.

Since the guest complains about ring 1 and ring 1 is serviced in the VM worker, the stalled hop is
**the VM worker's virtio-gpu/venus path**, and the parked render server is a consequence (no work
arrives there). The sharpest next instrument is therefore in libkrun/rutabaga inside the VM worker -
count the virtqueue kicks and the venus ring seqno updates it performs (`virglrenderer`'s
`vkr_ring_submit_virtqueue_seqno` has the host-side counterpart) - not in venus's C code, and not in the
WSI modifier path that this ticket started from.

Environment fact worth carrying into that work (found earlier this session): the render server's
`/dev/shm` is **not** the host's while the VM worker's *is*, so any ring/wake object that is expected to
be shared between those two processes must not rely on `/dev/shm` agreeing.

## ROOT CAUSE (2026-10-08): the render worker is SIGSYS-killed for calling `rename`

Traced end to end with the VM worker under `--seccomp=audit` (plus cang's own VM-worker strace
hook, so ptrace is permitted) and the render server traced from birth under a permissive policy:

- **hop 1 works**: the guest's venus commands arrive at the VM worker - context create
  (`CREATE_CONTEXT ctx=1 name="vo"`) and 95 × 264-byte `SUBMIT_CMD` messages on the render-server
  socketpair, all delivered;
- **hop 2 works**: the sends succeed and are drained;
- **hop 3 breaks**: the per-context render worker process (`virgl-N-gpu_renderer`, the process that
  owns that context's venus ring and its `vkr-ringmon` alive-bit) **disappears** mid-run; the VM
  worker's later teardown of the context gets `EPIPE`; and the guest then reports exactly
  `stuck in ring seqno wait with iter at 4096` / `aborting on expired ring alive status` and aborts.

Diffing the syscalls the render server and its workers actually used against the packaged 107-entry
allowlist yields **one** used-but-not-allowed syscall: **`rename`**. The workers' disk-cache threads
call it to publish Mesa shader-cache entries under `MESA_SHADER_CACHE_DIR=/dev/shm/mesa-cache` (which
cang's runner sets itself): 12 `rename("<cache>/…tmp", "<cache>/…")` calls, all from the ctx-1/ctx-2
workers. With `mismatch_action: "trap"` that is an immediate SIGSYS death - of the process that owns
the guest's ring, ~5 s after the client starts presenting.

**Minimal-fix proof:** the packaged policy with nothing changed but `{"syscall": "rename"}` added -
mpv plays to its probe timeout (`rc=124`, 40 s), both workers stay alive, no ring wait, no abort.
The existing guard test (`render_server_seccomp_policy_allows_venus_driver_syscalls`, which already
covers `fallocate`/`flock`/`mkdir`/`sched_setscheduler`/`setpriority` for exactly this failure class)
was missing `rename` too.

**Fix applied in-tree:** `crates/cang/assets/seccomp/render-server.json` now allows `rename` next to
`renameat2`, and the guard test's list includes `"rename"`.

**Acceptance:** a Vulkan-presenting client in a `--gpu=drm --waypipe` guest reaches its first frame -
satisfied by the minimal-fix arm (mpv presented for the whole 40 s probe window, with both render
workers alive), and to be re-confirmed on the committed build.
