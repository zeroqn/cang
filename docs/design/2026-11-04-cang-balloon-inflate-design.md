# cang balloon inflate design

## Summary

cang's host-pressure reclamation (`docs/images-and-storage.md`, "Guest memory and
zram swap") can only ask the guest to release **clean page cache**. When host
pressure comes from guest **anonymous** memory - compiled artifacts, a running
daemon, a compiler working set - nothing in cang can take it back, because
libkrun's virtio-balloon implements only its free-page-reporting queue and a
guest that is not freeing pages reports nothing.

This note covers the second mechanism: **balloon inflate**. The host raises the
balloon's target page count, the guest's `virtio_balloon` driver hands pages
back out of its own free-page pool, and the VMM discards them on the host, which
is the only path that returns guest anonymous memory.

Decision recorded here: **deferred, with a two-phase plan.** The measured cache
case already recovers most of an idle guest's residency (3.8 GiB of a 4.2 GiB
residency, see "Why"), and inflate is not just a bigger version of that - it is
an asynchronous, guest-cooperative, sticky control whose half-implemented state
hangs the guest. Phase 1 (fork: inflate queues, control handle, mapping-aware
discard, `DEFLATE_ON_OOM`) is independently provable with a manual target and is
the prerequisite for any policy work. Phase 2 (cang: pressure-driven two-sided
controller with a cap) only pays off once Phase 1 exists and a real workload is
shown to be anon-dominated.

## Goals

- Return guest anonymous memory to the host under host memory pressure, without
  killing or restarting the guest.
- Keep the existing cache-reclaim path as the first responder, since it is
  synchronous and free of guest-side risk.
- Never leave the guest starved: a target set by pressure must be released when
  pressure is gone, capped as a fraction of guest RAM, and defensively bounded
  by the guest's own OOM path.

## Non-goals

- Elastic `--mem`: the guest still sees a fixed memory size. Inflate makes the
  host reclaim resident pages; it does not give them back without a deflate.
- Guest swap policy: zram swap stays the guest's own answer to guest pressure.
- Replacing page reporting. The two queues are independent and coexist.
- Any per-memory-region or per-cgroup control on the host side.

## Why

Measured on the shipped cache path (2026-11-04, `--mem 6`, guest holding 3.8 GiB
of page cache from `/nix` reads, host driven into genuine swap-out at
23,394 pages/s): the guest released 3,804,940 KiB, guest `Cached` fell
3.81 GB -> 32 MB, and the VM worker's RSS fell 4,290,224 -> 569,340 KiB
(-3,633 MiB) as host `MemFree` rose in lockstep. So the cache half is real and
cheap.

What that path cannot touch is the rest of the residency: the same guest's
`AnonPages` (2.0 GiB in an earlier arm that allocated, untouched by
`drop_caches`). A workload that compiles, renders or serves from anonymous
memory keeps that memory on the host while the host swaps, and guest zram does
not help the host - it re-compresses inside the guest's own RAM.

## Alternatives considered

### Host `madvise(MADV_PAGEOUT)` on guest RAM

No guest cooperation, but it only works if the host has real swap to page into:
this host's only swap is zram, so paging guest RAM out mostly re-compresses it in
host RAM - a bounded win with a CPU cost, a KVM fault-in stall on next touch,
and it fights the `MADV_HUGEPAGE` advice the builder applies. Not a substitute
for reclaiming the memory.

### Guest-side `drop_caches` (shipped)

`ReclaimMemory` over the exec channel; synchronous, no guest-side risk, reclaims
clean page cache only. Kept as the first responder and the fallback when the
fork has no inflate support.

### Balloon inflate (this note)

The only mechanism that reaches anonymous memory. Costs a permanent fork
divergence and a guest-side asynchronous control loop.

## Architecture

### The device

The fork's balloon is closer to inflate than its stubs suggest. All five queues
exist at size 256 and are epoll-registered at `activate`, and the reporting
queue is fully implemented:

- queue indices `IFQ 0 / DFQ 1 / STQ 2 / PHQ 3 / FRQ 4`:
  `deps/libkrun/src/devices/src/virtio/balloon/device.rs:15-24`;
- `VirtioBalloonConfig { num_pages, actual, free_page_report_cmd_id, poison_val }`
  (`device.rs:33-44`), read raw by `read_config` (`device.rs:207-219`), but
  `num_pages` is never written and every config write is dropped with a warning
  (`device.rs:221-227`) - and the guest driver *does* write `actual` back;
- inflate/deflate/page-hint handlers are inert: `error!("balloon: unsupported
  inflate queue event")` then drain the eventfd (`event_handler.rs:17-43`,
  page hint at `:60`), so the used ring never advances;
- reporting queue done: `device.rs:122-175` with `madvise(MADV_DONTNEED)` in
  `linux.rs:24-27`.

Inflate therefore needs, in the fork:

1. **Target/actual accounting.** A `num_pages` field the host can write,
   `actual` accepted from the guest's config write, both cleared by `reset()`.
2. **A real inflate handler.** Firecracker's shape is the reference: on an IFQ
   event, pop each descriptor, validate it (not write-only, `len % 4 == 0`,
   at most 256 PFNs per descriptor), `add_used`, compact adjacent PFNs into
   ranges, then discard each range. Deflate is almost a no-op on the host - the
   pages are the guest's again - and only the descriptor has to be acked and the
   used ring signalled.
3. **A config-change notification.** Writing `num_pages` is useless without
   poking the guest: `InterruptTransport::signal_config_change`
   (`deps/libkrun/src/devices/src/virtio/device.rs:68-80`), already wired through
   the mmio/pci transports (`mmio.rs:567`, `pci.rs:1450`) and currently called by
   nothing in the balloon.
4. **Mapping-aware discard.** `linux.rs:26` madvises `MADV_DONTNEED`
   unconditionally. That is right for anonymous RAM and wrong for
   `MAP_SHARED` memfd RAM, where it drops the mapping's RSS while the pages stay
   resident in shmem. Guest RAM is memfd-backed when `use_vhost_user ||
   use_gpu_udmabuf` (`vmm/builder.rs:2155`), i.e. the udmabuf zero-copy path, so
   that case needs `MADV_REMOVE` or `fallocate(PUNCH_HOLE)`. Firecracker's
   `discard_range` and QEMU's `ram_block_discard_shared_range` both branch on the
   mapping type. This is a latent bug in the shipped reporting path too (the
   README says the balloon "has nothing to reclaim" on the zero-copy path, which
   is true in effect but for the wrong reason).
5. **`VIRTIO_BALLOON_F_DEFLATE_ON_OOM`.** `AVAIL_FEATURES`
   (`device.rs:27-30`) advertises `STATS_VQ`, `FREE_PAGE_HINT` and `REPORTING`
   only. Without deflate-on-OOM (bit 2) an over-inflated balloon turns guest
   pressure into an OOM kill instead of a self-deflate; the Linux driver honours
   it by giving back 256 pages (1 MiB) per OOM notification.
6. **A host-callable control handle.** `BalloonDevice { inner: Arc<Mutex<Balloon>> }`
   is `pub(crate)` and `attach()` consumes it into the transport registry with no
   lookup back (`api/device_builders.rs:1426-1441`, `:1465-1476`). So the fork
   must hand out something callable *before* `attach`. Cheapest shape: a
   `control()` returning a clone of the `Arc` plus a device-owned `EventFd`
   (the console's `sigwinch_evt` is the existing precedent: `console/device.rs:73`,
   registered in `interest_list()`, handler in `console/event_handler.rs`).
   cang builds libkrun as a path dependency **without** the `ffi` feature, so the
   C ABI and `make gen-libkrun-bindings` are optional; the Rust API is not.

Three guest-side properties constrain the design:

- **Inflate is asynchronous.** The host only writes `num_pages` and raises a
  config interrupt. The driver's `update_balloon_size_func` runs on
  `system_freezable_wq` and fills at most 256 PFNs (1 MiB) per round trip,
  re-queueing itself, so a hundreds-of-MiB target is tens of ms to seconds.
- **It degrades exactly under pressure.** When the guest cannot allocate pages
  the driver sleeps 200 ms ("Out of puff") and retries, and the spec gives no
  progress guarantee. A balloon is worst at the moment it is wanted.
- **It is all-or-nothing per popped chain.** `tell_host()` blocks in
  `virtqueue_get_buf()` holding `balloon_lock`, so every popped IFQ/DFQ
  descriptor must be `add_used` and signalled. A stub is inert; a partial
  implementation wedges the guest.

### Reaching the device from the policy

The pressure policy runs in the helper. The balloon is created and run two
processes below it - helper -> `libkrun-vm-worker-enter` -> the sandboxed VM
child that calls `krun_start_enter` - and the child's Landlock rules make
sampling `/proc/pressure/memory` there impossible outside `--gpu=drm`. The
request therefore crosses a socketpair, forwarded as an inherited fd exactly
like passt's (`crates/cang/src/runtime/session/supervisor/vm_child.rs:88,113,130`),
with a tiny reader thread in the child that calls `control().set_target_pages()`.
That fd hop, not the device code, is the largest structural item on cang's side.

### Policy

Keep the existing trigger and make the response two-sided:

- Cache first: `psi_full >= 1 %` or swap-out `>= 64 pages/s` -> `ReclaimMemory`
  (synchronous, today's behaviour; `CANG_MEMORY_RECLAIM_PSI_FULL` /
  `CANG_MEMORY_RECLAIM_SWAP_OUT` still move the thresholds).
- Anon follow-up: if host pressure persists while the cache release bought
  little, raise the balloon target in 256 MiB steps, one step per sample, up to a
  cap of 50 % of `--mem`.
- Deflate on relief: when `psi_full < 0.1 %` and swap-out `< 8 pages/s` for 60 s,
  lower the target back (to zero, or to a small residual that the next pressure
  event re-uses).
- Off switches: `CANG_MEMORY_RECLAIM=0` disables the watcher entirely;
  `CANG_MEMORY_RECLAIM_BALLOON=0` keeps cache-only reclaim, which is also the
  automatic behaviour on a guest whose image predates inflate support.

## File-by-file changes

Fork (`deps/libkrun`, ~220 non-test LOC over 7 files, ~200 test LOC):

| file | change | est. LOC |
| --- | --- | --- |
| `src/devices/src/virtio/balloon/device.rs` | target/actual fields, `set_target_pages`, `write_config` accepting `actual`, `reset()`, `DEFLATE_ON_OOM` in `AVAIL_FEATURES` | ~120 |
| `src/devices/src/virtio/balloon/event_handler.rs` | real `handle_ifq_event` / `handle_dfq_event`, control eventfd in `interest_list()` | ~20 |
| `src/devices/src/virtio/balloon/linux.rs` | discard by mapping type (`MADV_DONTNEED` vs `MADV_REMOVE`) | ~25 |
| `src/devices/src/virtio/balloon/macos.rs`, `windows.rs` | keep compiling; inflate stays Linux-only | ~10 |
| `src/libkrun/src/api/device_builders.rs` | `BalloonDevice::control()` handle taken before `attach` | ~35 |
| `src/libkrun/src/api/mod.rs` | export the handle type | ~3 |
| `src/devices/src/virtio/device.rs` | `DeviceState::signal_config_change` shim if the device cannot reach the transport | ~8 |

cang (~340 LOC over 11 files):

| file | change |
| --- | --- |
| `crates/cang-libkrun/src/{api,linked}.rs` | bind `balloon_device_control` / `set_target_pages` on `LibkrunApi` and the arena impl (`api.rs:110-113`, `linked.rs:538-540`) |
| `crates/cang/src/runtime/vm/libkrun/launcher.rs` | take the control handle in `configure_balloon` (`:852-858`) before the device is added (`:888`) |
| `crates/cang/src/runtime/session/supervisor/{entry,vm_child}.rs` | socketpair fd forwarded alongside `passt_fd`; child-side reader thread calling the handle |
| `crates/cang/src/runtime/session/memory_reclaim.rs` | two-sided policy state machine, cap, hysteresis, second off switch |
| `crates/cang/src/runtime/vm/libkrun/tests.rs` | fake `LibkrunApi` records intents rather than applying them (the existing seam) |
| `docs/images-and-storage.md`, `docs/internals.md`, `README.md`, `deps/libkrun/CANG.md` | document the mechanism, knobs and the "fixed `--mem`" premise that inflate partly retires |

## Known hazards

- **Guest hang** from a half-implemented queue: every popped descriptor must be
  acked (`tell_host()` blocks under `balloon_lock`). Phase 1 must therefore land
  the handler and its tests together, never in pieces.
- **Sticky memory**: an inflated balloon never gives pages back on its own.
  Without the deflate arm and the 50 % cap, a host pressure spike permanently
  shrinks the guest.
- **THP/EPT shredding**: pages handed back are 4 KiB PFNs, so `MADV_DONTNEED`
  splits the 2 MiB pages and EPT entries that the builder's `MADV_HUGEPAGE`
  advice created; a later refault re-merges them (khugepaged) or does not.
- **Double counting**: page reporting (`FRQ`) and inflate (`IFQ`) act on the same
  RAM mapping. Inflated pages are the guest's free pages, so they should already
  be reported; expect the reporting queue to do less work while inflated, not the
  totals to add.
- **Memfd RAM**: without the mapping-aware discard, inflate inflates but the host
  gets nothing back on the zero-copy path.
- **Fork divergence and pin cascade** for every future rebase: fork tag
  `v2.0.0-cang.<n>`, `deps/libkrun` pointer, `libkrun-src` flake input,
  `Cargo.lock`, `cargoDeps`, `krunInitCargoDeps`, then the three live smokes in
  `docs/maintenance.md`.

## Validation

Phase 1 (fork only, manual target): unit tests for descriptor validation, PFN
compaction, `actual` accounting, `reset()`, and the mapping-type branch; then a
live arm that sets a target from a debug path and watches guest `AnonPages` and
the VM worker's RSS.

Phase 2 (policy), reusing the arms the cache path was verified with
(`/tmp/mempatrol-live` harness, one boot per arm, per-pid RSS sampler):

1. anon-heavy guest (touch 3 GiB anonymous, no cache read) under real host
   swap-out: inflate fires, worker RSS and guest `AnonPages` both fall;
2. relief: pressure removed, the target returns to zero and the guest's memory
   comes back;
3. cap: the target never exceeds 50 % of `--mem`, guest stays alive while
   allocating at the cap (`DEFLATE_ON_OOM` proven, not assumed);
4. opt-out: `CANG_MEMORY_RECLAIM=0` and `CANG_MEMORY_RECLAIM_BALLOON=0` leave
   guest and worker memory exactly where the workload left them;
5. regression: the cache arm from today's verification still fires with
   `freed_kib > 0`, and the three `docs/maintenance.md` smokes pass.

## Open questions

- Is host pressure on a real cang workload anon-dominated at all, or is the page
  cache - which the shipped path already reclaims - the whole story? A single
  measurement of a compile/render workload's guest `AnonPages` vs `Cached` under
  host pressure would settle whether Phase 2 is worth building.
- Should the deflate arm also run when the guest itself reports memory pressure
  (a guest-visible counter through the stats queue), instead of only on host
  relief?
- Does `--zero-copy-shm` want its own answer (`MADV_REMOVE` discard, or simply
  document that balloon reclaim is unavailable there), given the udmabuf path
  already trades shared RAM for copy avoidance?
