---
label: wayfinder:map
title: Fast /workspace for rustc inside a cang guest (virtio-fs tuning + baseline)
---

## Destination

`cang` has a **committed, reproducible rust-compile benchmark** (a pinned small
crate tree plus the repo itself as an optional workload; arms: host-native,
guest-over-virtiofs, guest-local) and `/workspace` over virtiofs is **measurably
faster for that workload** through changes that benchmark attributes - carried
through the fork's tag/pin and cang's build, so the next tuning change is judged
against recorded numbers instead of a feeling.

Since 2026-10-26 the intended shape of that win is **bob's**: an **opt-in cache
scoped to `/workspace` only** (never `/nix`, never the cargo registry), which
prints a warning on launch that host-side access to the mounted workspace can
corrupt what the guest sees; and, if the host filesystem permits it, **per-inode
DAX** rather than the mount-wide window ticket 05 dropped.

"Reached" means: the harness exists and its recorded baseline is in the docs; the
cache flag (and per-inode DAX, if possible) ship on fork tags pinned into cang;
and the before/after numbers come from live guests, not from a compile or a CI
green.

## Notes

- **Where this effort lives.** In the repo, on `main`: the map, its tickets, its
  notes and `tools/rust-compile-bench` all landed there on 2026-10-27, and the
  worktree the effort used before that (`/home/dev/cang/cang-vfio`, branch
  `wayfinder/workspace-virtiofs-perf`) was removed once the branch merged. That
  worktree existed because the primary checkout was shared with the concurrent
  **THP** effort; when a benchmark needs an isolated tree again, make a fresh
  worktree, land through `main`, and delete the worktree with the run.

- Domain: `deps/libkrun` (`zeroqn/libkrun`, branch `cang`) -
  `src/devices/src/virtio/fs/` (`device.rs`, `worker.rs`, `server.rs`,
  `filesystem.rs`, `fuse.rs`, `linux/passthrough.rs`), `src/libkrun/src/api/
  device_builders.rs` (`FsDevice`, `DeviceRequirements.shm_size`);
  `crates/cang-libkrun` (`linked.rs::fs_device_new`, `api.rs`);
  `crates/cang/src/runtime/vm/libkrun/launcher.rs` and
  `crates/cang/src/runtime/launch/` (mounts, `IMAGE_CANG_ENV_ALLOWLIST`);
  `crates/cang-guest-init` (guest env, `/workspace` probes);
  `nix/pins.nix`, `nix/pkgs/cang-rust.nix` (`cargoDeps`) and
  `nix/pkgs/libkrun-source.nix` (`krunInitCargoDeps`) - two `fetchCargoVendor`
  hashes that must be harvested with **distinct** placeholders (rule #134);
  `tools/` for the harness.
- Skills every session on this map should consult:
  `cang-workspace-io-device-attribution` (the workspace share is virtio-fs, not
  virtio-blk: a blk-side change cannot help it), `cang-libkrun-fork-maintenance`
  (fork tags `v<base>-cang.<n>`, pin gate, `cargo --locked`/vendor-hash moves),
  `cang-change-verification-gates` (nixfmt, cargo fmt/clippy, artifact
  attribution), `cang-live-guest-evidence-harness` (delegated VM arms, per-arm
  logs, fresh-log check, clean window) and `cang-prebuilt-pin-refresh` if the
  release path moves.
- Tracker: **local markdown**, this directory. `gh` is unauthenticated in these
  sessions; bob pushes branches and tags.
- **Destination decisions (bob, 2026-10-25):**
  - the map ends at *tuning + a committed baseline*, not at an analysis.
  - levers in scope: **libkrun fork virtio-fs work** and **guest caching first**.
  - workload (revised 2026-10-25 after bob's suggestion): **both** the generated
    high-file-count tree (16801 files - the metadata/stat churn) **and a build of
    this repo itself** (`--workload cang`: the real crate graph, our own code, no
    external pin, 84 s offline on the host). Databend is **retired** as
    superseded - it is an external pin whose image requirements this repo does not
    need to take on.
  - **a virtio-fs correctness bug is in scope**: the first baseline run found a
    cold `cargo build` in `/workspace` failing with `ENOENT` on directories it
    just created (the guest-local arm builds fine), and bob made fixing it a
    **prerequisite** of any tuning measurement - a build that cannot complete on
    `/workspace` measures nothing.
  - **if the measured penalty turns out small**, the map ends by *recording that*
    ("the virtio-fs cost of /workspace is within X% of a guest-local build") and
    closing, leaving remaining rustc slowness to CPU/sizing/codegen - the
    excluded levers are not reopened for it.
  - attribution arms: **host-native control** and **guest-local-disk control**.
    (Per-phase timings and VMM-side FUSE counters were not asked for - the two
    controls are what makes a win attributable.)
- Reading of those decisions: "cang-side plumbing" was *not* selected as a
  lever, so passing a new option from `cang`'s launcher into an existing fork
  API is in scope as part of a virtio-fs change, while reworking cang's mount
  layout for its own sake (splitting `/nix`, cargo and sccache onto separate
  shares) is not. The one place this reading may have to be revisited is sound:
  if a virtio-fs knob cannot apply to the **root** mount, `/workspace` would
  need its own device - see **Not yet specified**.
- **Starting facts (verified 2026-10-25, this checkout + host):**
  - cang registers exactly **one** virtiofs device: `fs_device_new(
    CANG_FS_ROOT_TAG, &config.task_rootfs, false)` in
    `launcher.rs:236-247`, and the guest mounts it as `/` (`rootfstype=virtiofs`,
    `DEFAULT_KERNEL_CMDLINE`). `/workspace`, `/nix`, `~/.cargo` and
    `~/.cache/sccache` are all **inside that single share** (the workspace is a
    bind graft into the prepared root, `prepared_root/mod.rs:113`).
  - The device offers **one** request queue: `defs::NUM_QUEUES = 2`
    (`HIPQ_INDEX` + `REQ_INDEX`), `queue_config()` returns that static array and
    `config.num_request_queues = 1`. **Correction (ticket 15): virtio-fs has no
    `VIRTIO_F_MULTIQUEUE` feature bit at all** - the queue count is purely the
    device's config word (kernel `virtio_fs.c:963-971`), so there is nothing to
    negotiate; the limit is a hard-coded constant inherited from crosvm's 2019
    device, not a withheld feature. **Addition (2026-11-04, note 44):** the guest
    does honour a larger number - `virtio_fs.c` takes `num_request_queues` from
    the config word and treats `0` as `min(ncpus, 64)`, one `fuse_iqueue` per
    queue - so raising the constant is a host-side-only change, now
    [ticket 21](tickets/21-multiqueue-request-queues.md). The fork's request
    threads are not a lever: 4 -> 16 workers measured flat (note 44, section 4).
  - One `FsWorker` thread serves that queue serially (`worker.rs::process_queue`
    -> `server.handle_message` inline), so rustc's stat/open/read/write stream
    is a single-threaded request loop.
  - The advertised FUSE_INIT set is **at parity with upstream virtiofsd, and
    slightly more aggressive**: `server.rs:897-908` advertises ASYNC_READ,
    PARALLEL_DIROPS, BIG_WRITES, AUTO_INVAL_DATA, ASYNC_DIO, HAS_IOCTL_DIR,
    ATOMIC_O_TRUNC, MAX_PAGES, SUBMOUNTS, HANDLE_KILLPRIV_V2, INIT_EXT, plus
    `ALLOW_IDMAP` and (from `passthrough.rs:899`) DO_READDIRPLUS/READDIRPLUS_AUTO,
    and `MAX_PAGES` is already at the kernel's 1 MiB ceiling. What is *not*
    negotiated: WRITEBACK_CACHE (`cfg.writeback` false, `passthrough.rs:371`),
    the zero-message-open pair (NO_OPEN_SUPPORT / NO_OPENDIR_SUPPORT),
    CACHE_SYMLINKS, and EXPLICIT_INVAL_DATA (mutually exclusive with the
    AUTO_INVAL_DATA it sets). `attr_timeout` is the virtio-fs default 5 s
    (`device.rs:88-93`). (Corrected 2026-10-25 by ticket 01: an earlier reading
    of `passthrough.rs::init` alone mistook the filesystem-specific flags for
    the whole set.)
  - A **DAX window is already plumbed but never enabled**: `FsDevice::
    set_dax_window_size()` (`device_builders.rs:664`) feeds
    `DeviceRequirements.shm_size`, `attach` calls `fs.set_shm_region(...)`
    (`:728-729`), and `linux/passthrough.rs:2045 setupmapping` really `mmap`s the
    host file into the window. `crates/cang-libkrun/src/linked.rs:317` never
    calls it, so `shm_region` is `None` for cang's share.
  - Guest kernel (pinned libkrunfw `linux-7.2.10` + `v7.2.10-hardened1`, both
    `-kvm-lto` and kvm configs) has
    `CONFIG_FUSE_FS=y`, `CONFIG_VIRTIO_FS=y`, `CONFIG_FUSE_DAX=y`,
    `CONFIG_FUSE_IO_URING=y`, `CONFIG_FUSE_PASSTHROUGH=y` - the guest side is
    not the missing piece.
  - Host: `7.2.11-xanmod1`, 28 CPUs, 24 GiB RAM. Existing compiler-cache wiring:
    `SCCACHE_DIR=/home/dev/.cache/sccache` plus `RUSTC_WRAPPER` -> bundled
    sccache, both on virtiofs.

## Numbers on the table (2026-10-25/26, **contended window** - see ticket 12)

The measurement this map was missing exists, and it reframes the effort.
Synthetic workload (2401 crates, 16801 sources), cold `target/`, rustc 1.95.0,
`CARGO_BUILD_JOBS=26`, sccache and incremental off, one VM at a time, `/dev/shm`
control:

| arm | runs (s) | vs host | vs guest-local |
|---|---|---|---|
| host | 104.2, 104.7 (+ 104.5, 105.6, 106.4 earlier) | 1.00x | - |
| guest-local (`/dev/shm`, no FUSE) | 232.0, 232.9, 235.1 (**lever-off: see below**) | 2.2x | 1.00x |
| guest-virtiofs (`/workspace`) | 298.9, 306.5, 311.4, 326.5 | 2.9x | **1.29-1.36x** |

- **The virtio-fs penalty is ~30%**, and it is a *floor*: the control is RAM.
- **Correction (2026-10-26): the FUSE worker uses ~26% of one core, not 12%.**
  The sampler summed the wrong `/proc/<tid>/stat` fields because the thread's comm
  is `fs worker` *with a space*, which shifts every field index by one;
  `utime+stime` and `schedstat` runtime now agree at 19.7 s of a 77 s window
  (25.6%). It is still not *blocked* (runqueue wait 0.06%, most samples in
  `do_epoll_wait`, ~3000 wakes/s), so little is available to overlap - but the
  ceiling for pool/io_uring work is a quarter of a core, not an eighth, and
  whether the guest's FUSE path scales with concurrency is now measured directly
  (note 20).
- **The VM penalty is 2.2x the host** - twice the FUSE penalty, and no virtio-fs
  change can touch it.
- Run-to-run spread: host ~2%, guest-local ~1%, guest-virtiofs ~4%. A shipped
  change must beat ~5% on the guest-virtiofs arm to mean anything.
- `--workload cang` (this repo, 1356 files, heavy per-unit compile): host 55.9 s
  recorded; the guest arms are blocked by
  [ticket 11](tickets/11-image-ships-cang-build-inputs.md) (the image has no
  virglrenderer, which the build needs - and mounting the host's store path does
  not help, because the guest's store is a different store).
- **Where the penalty comes from (the decisive number):** during a full `JOBS=26`
  guest-virtiofs arm, the single FUSE **`fs_worker` thread used 12% of one core**
  (a measurement since corrected to ~26%: same thread, a field-index bug in the
  sampler, see the note below)
  while the guest's own vCPUs used ~65% + ~22% each. The worker is 88% idle, so
  the cost is per-request round-trip latency, not worker serialization.
- **Caveat:** another agent session may have been running a live smoke during
  these runs (bob, 2026-10-26). The ratios are consistent across runs and the
  worker-idle finding is contention-safe in the conservative direction (a
  co-tenant inflates the worker's share), but the absolute seconds are ±;
  [ticket 12](tickets/12-clean-window-re-record.md) re-records them cleanly.


## The governor fact that reorders everything (2026-10-26, verified twice)

**`/` is itself the virtio-fs share.** `/proc/mounts` in a cang guest:
`/dev/root / virtiofs rw,relatime` and `none /workspace virtiofs`. So *every*
process launch (`rustc`, `ld`, `cc`) opens its binary over the same FUSE
connection, and `target/` was never the only thing paying for it.

That invalidates every "no-FUSE floor" this map used before zero-message open was
measured: the guest-local arm still launched 2401 rustc/ld binaries off virtiofs,
so it paid OPEN+RELEASE too (measured: **OPEN 87,871-89,374, RELEASE
87,316-88,876** with the lever off, and 337.7/313.8 s; with it on, **OPEN 3,
RELEASE 1** and 124.4/111.0 s).

Measured again, one VM at a time, `JOBS=26`, 4 interleaved runs per side, with the
host arm as a drift yardstick (the host itself measured 130.2 s this session vs
104.2 s in note 15 - the host moved, so ratios are the only stable unit):

| arm | mean (range) | /host | note |
|---|---|---|---|
| host | 130.2 s (119.5-140.9) | 1.00x | no VM, no FUSE |
| guest tmpfs, zero-message open **on** | 131.5 s (111.0-164.9) | 1.01x | true no-FUSE floor |
| guest virtiofs, **off** | 427.1 s (366.2-563.0) | 3.28x | pre-change behaviour |
| guest virtiofs, **on** | 176.0 s (162.6-185.3) | **1.35x** | shipped |

`ON/OFF = 2.42x`; requests fall 3.6 M -> 1.5 M; OPEN-class requests go from 18.9%
of every request to zero.

**What this does to the map's earlier conclusions:**
- the ~70 s "virtiofs penalty" computed against the 232 s floor was an artefact of
  mixed states; the real penalty *before* the change was 427 - 131 = **296 s**, and
  it is now 176 - 131 = **45 s**;
- the pool (ticket 04) was justified on `readdirplus` saturation - still true for a
  *first* scan of a huge directory, but repeated scans are now answered from the
  guest's directory cache (129 -> 4927 ops/s at one thread), so the pool's share of
  a real build fell further;
- DAX and every "remove a data round trip" design (notes 05/18/24) now have a much
  smaller numerator still: the remaining gap is 45 s, spread over
  LOOKUP/FLUSH/WRITE/GETXATTR, none of which is a data-path design.

- **Queue-depth decision (bob, 2026-11-04): chart the multi-queue work; the pool
  is settled shut.** After the shipped-defaults verification, bob asked what was
  left and approved two checks rather than a redesign: *who burns host CPU during
  a 26-process create storm*, and *does the pool size move the ceiling*. Both are
  in [note 44](notes/44-queue-depth-not-thread-count.md):
  - the dispatcher used **40.4 %** of one core and each of the four pool threads
    **17.4-17.6 %** during a 7 819 creates/s storm (4.03 cores host-wide, nothing
    saturated), so the wall time is latency and queueing;
  - **4 -> 16 workers changed nothing** (mean 6.3 k vs 5.8 k creates/s @26 across
    two interleaved rounds per side; the edit was reverted and the tree left
    clean), so `CANG_FS_WORKER_THREADS` is not a lever and 4 stays;
  - completion signalling is already batched (651 host->guest interrupts/s against
    ~28 k requests/s, ~43 replies per notification), and the connection's
    background-request credit is unreachable from the guest (no
    `/sys/fs/fuse/connections` entry for a virtiofs mount), so neither is a knob;
  - what *is* reachable is the **queue count**: the guest kernel reads
    `num_request_queues` from the device config word and gives each request queue
    its own `fuse_iqueue` (`0` = `min(ncpus, 64)`), while the fork hard-codes `1`.
  **Reading:** this does not reopen ticket 01's finding (there is still no
  multiqueue feature bit) - it corrects the *inference* drawn from it. The pool
  was the right first move and is finished; the next fork change is
  [ticket 21](tickets/21-multiqueue-request-queues.md), and the payoff is scoped to
  metadata-heavy workloads because a cold 2401-crate build already sits at ~1 % of
  the host.

- [Advertise N virtio-fs request queues](tickets/21-multiqueue-request-queues.md):
  **the next fork change, and the only queue-side lever left open.** Parallel
  metadata churn tops out at 7-8 k creates/s against a 25-29 k tmpfs control, with
  no host thread saturated (dispatcher 40 %, pool 17 % each), the pool size flat at
  4 vs 16 workers, completion notifications already batched (~43 replies each), and
  the connection's background-request credit unreachable from the guest. What is
  left is one serialized kick -> dispatch path per request, so the ticket
  advertises N request queues (the guest kernel already supports them from the
  config word) behind a per-launch knob, A/B'd 1 vs 4 on the storm, the scaling
  ladder and a rust-build regression arm. Numbers and dead ends:
  [note 44](notes/44-queue-depth-not-thread-count.md).

- [Advertise N virtio-fs request queues](tickets/21-multiqueue-request-queues.md):
  **implemented, measured flat, reverted.** The guest does drive several request
  queues when the device advertises them (five queues activated, requests landing
  `#1=276 #2=1065 #3=306 #4=112`), and it changes nothing: a 26-process create
  storm runs 4.6-8.2k/s with 4 queues against 5.2-9.3k/s with 1, and a compile
  iteration 11.8 s against 11.3 s. One dispatcher thread over N queues serialises
  the handoff by construction, and the dispatcher was only ~40 % of a core
  either way, so the queue is not the serial resource. The limit is one round trip
  per path component at ~7k requests/s, spent in the guest FUSE path.
  [note 45](notes/45-compile-iteration-lookup-diagnosis.md) §6.

- [Where the per-request cost sits, and what to do next](notes/46-request-cost-split-and-recommendation.md):
  **no single component owns it.** Of the ~516 us of host CPU per create, 48 % is
  the guest (vCPUs), 27 % is the FUSE server (dispatcher 10 %, pool 17 %) and 24 %
  is VMM plumbing; parallelism is 4.0x. Halving either side buys ~1.3-1.5x, both
  ~2x - so there is no 4x in per-request work, and the completion-exit count is
  already amortised (~11-43 replies per interrupt). Recommendation: accept 8.2 s
  or grill [ticket 18](tickets/18-workspace-target-on-block.md); a
  virtio-pci/MSI-X or inline-completion patch is worth at most ~1.5-2x and only if
  a fork cycle is explicitly wanted.

## Decisions so far

- [Issue #939 / the cache policy / metadata timeouts / what the remaining 45 s
  is](notes/35-no-flush-and-negative-entry-shipped.md): **the cache policy is
  already spent and the timeouts cannot help; two other request classes could,
  and both shipped.** `CachePolicy::Always` only sets `FOPEN_KEEP_CACHE` in an
  open reply, and zero-message open already leaves the guest with exactly that
  flag as its no-open default, so `always` would change nothing measurable;
  issue #939's attribute-remembering reply lives in `do_open`, which this share
  no longer reaches. `entry_timeout` 5 s -> 60 s cut lookups 2.3% and returned
  every one of them as a GETATTR (-0.2% wall), because 91.5% of lookups are
  negative and an `-ENOENT` reply is never cacheable. Answering misses with the
  protocol's negative entry (250 ms) removed **72% of the lookups**, and
  answering `FUSE_FLUSH` with `-ENOSYS` removed **21% of all requests**; together
  **171.4 s -> 116.3 s (-32.2%)**, against a 115 s host-native build, i.e. the
  share costs ~1%. Shipped on by default in the fork's `v2.0.0-cang.13` and in
  `crates/cang-libkrun/src/lib.rs`, each overridable per launch
  (`CANG_FS_NO_FLUSH=0`, `CANG_FS_NEGATIVE_ENTRY_TIMEOUT=0` or a millisecond
  value); the cost is bounded host->guest create invisibility, measured 74 ms at
  the shipped setting.

- [Zero-message open](tickets/19-zero-message-open.md): **implemented and
  measured, shipped on by default** - a full synthetic rustc arm 372.8 s ->
  140.2 s mean (2.66x, four interleaved runs per side), the `open` probe cell
  1788 -> 4788 ops/s at one thread, `readdirplus` 129 -> 4446. It deletes 17.9%
  of a build's requests (OPEN+RELEASE+OPENDIR+RELEASEDIR, counted server-side) and
  keeps the page cache across opens, which is the larger half of the win. The one
  cost - a host-side *in-place* rewrite of a file the guest has read is visible
  within `attr_timeout` (5 s) instead of at the next open - is the decision in
  [ticket 20](tickets/20-zero-message-open-coherence-contract.md). Numbers and
  the opcode histogram: [note 23](notes/23-zero-message-open-results.md).

- [Can a pinned databend tree be the real-world workload arm, offline and
  repeatably?](tickets/06-databend-workload.md): **superseded, no research done** -
  the real-world workload is cang's own tree (builds `cang` fully offline in 84 s
  on the host), and both workloads stay: the synthetic tree for metadata churn,
  the cang tree for the real graph. See
  [Add the cang-tree workload arm](07-cang-tree-workload-arm.md).
- [Would a DAX window on the root share actually help a rustc build, and what
  does enabling it cost?](tickets/05-dax-window-viability.md): **drop** - DAX is
  per-connection so it covers the whole guest root (every `/nix` binary too), turns
  every first fault into a `SETUPMAPPING` on the single worker, has no fallback
  when the window is exhausted, and its failure modes brick the boot.
- [May the guest cache /workspace, given the host tree is live while the guest
  writes it?](tickets/03-workspace-cache-coherence-contract.md): **no** - cang
  promises exact host visibility, so no write caching ever (WRITEBACK_CACHE out),
  metadata timeouts stay 5 s and ship only on later evidence; the pool and DAX are
  what is left. Regression gates named.
- [What can this guest kernel and the fork's FUSE server actually negotiate, and
  where does upstream virtiofsd differ?](tickets/01-fuse-negotiation-and-queue-facts.md):
  the free flag lever is already spent (fork is at virtiofsd parity, MAX_PAGES at
  the kernel ceiling, no FUSE_ACCESS because virtio-fs forces
  `default_permissions`); the only real levers are **WRITEBACK_CACHE** (first) and
  a **worker pool over the existing single request queue** (no multiqueue, no
  inode-map refactor needed); passthrough/io_uring are unreachable from a VMM
  server. Detail: [note](notes/01-fuse-negotiation-and-queue-facts.md).
  *Correction (2026-11-04, note 44):* "no multiqueue" was about the missing
  feature bit and stays true, but the **queue count is a host-side config word the
  guest honours**, so it is a lever after all
  ([ticket 21](tickets/21-multiqueue-request-queues.md)); and the pool, once
  shipped, measured flat from 4 to 16 threads - it was the right first move, not a
  scaling knob.

## Awaiting bob (answered 2026-10-26, except where noted)

1. **Is ~30% worth the worker pool?** Numbers: host 104 s, guest-local (/dev/shm)
   232 s, guest-virtiofs 299-311 s. Options: do the pool (ticket 04) / try
   NO_OPEN_SUPPORT first / document and stop.
2. **Should `cang -- cmd` report the command's exit status, and forward its
   stdout?** Today a guest command's status is lost (`cang -- false` exits 0) and
   its stdout is not relayed; the *shell* path already reports the code, and the
   fork already has the `0x7602` exit-code ioctl that nothing sends. Fixing it is
   ~20 lines in guest-init + two small host changes, but it changes user-visible
   semantics ([ticket 09](tickets/09-cang-exit-status-lies.md)).
3. **How to unblock the in-guest cang workload?** The image has no
   `virglrenderer`, which `deps/rutabaga_gfx/build.rs` needs. Options: add
   `pkgs.virglrenderer` to `cToolchainImagePackages` (ticket 11, recommended) /
   build only crates the image already supports / keep that workload host-only.

## Shipped (2026-10-26)

- Fork **`v2.0.0-cang.11`** (commit `d00b6d70`, assets published) carries three
  virtio-fs changes: the worker-robustness fix, the request thread pool, and
  **zero-message open**. cang pins it in the submodule and in `flake.lock`
  (`libkrun-src`), and the live smoke gate (Chromium-on-venus over `--waypipe`,
  VA-API decode, VA-API encode + round trip, attribution) passed on that commit
  before the tag was cut.
- **Result on the map's own workload**: cold 2401-crate rustc build 427.1 s ->
  **176.0 s** (2.42x, 4 interleaved runs per side, verified twice); 1.35x the host
  and 1.34x the true no-FUSE floor, where it was 3.28x. The share's cost fell from
  296 s to 45 s. Cost: 5 s close-to-open lag for in-place host rewrites, accepted
  by bob.

## Shipped (2026-10-27)

- Fork **`v2.0.0-cang.13`** (commit `e8a77159`) adds two request-removing
  switches - answer `FUSE_FLUSH` with `-ENOSYS`, and answer a miss with a
  cacheable negative entry - plus a positive/negative/error split in the opcode
  histogram, and drops the temporary cheap-lookup counters. cang turns both on by
  default (250 ms negative lifetime) with per-launch overrides, and pins the tag
  in the submodule and `flake.lock`.
- **Result on the map's own workload**: cold 2401-crate rustc build **171.4 s ->
  116.3 s (-32.2%)** against a 115 s host-native build, i.e. the share now costs
  ~1% where it cost 49% before zero-message open. Cost: a name the *host* creates
  can stay invisible to the guest for up to 250 ms (measured 74 ms). Numbers,
  dead ends and the ladder behind the lifetime: [note 35](notes/35-no-flush-and-negative-entry-shipped.md).

## Shipped (2026-11-03)

- Fork **`v2.0.0-cang.15`** (commit `eff52c5d`) advertises **`FUSE_CACHE_SYMLINKS`** behind
  `FsDevice::set_cache_symlinks` / `krun_fs_device_set_cache_symlinks` (off by default; bindings
  and `include/libkrun.h` regenerated, `CANG.md` updated). cang turns it on by default with
  per-launch overrides (`CANG_FS_CACHE_SYMLINKS`, `--fs=cache-symlinks:on|off`).
  The bit had been defined in the fork's `fuse.rs` since the crosvm import and never advertised,
  so every repeated symlink resolution cost a `READLINK` round trip.
- **Result**: `READLINK` 98-99% gone and total requests down 28-65% on the five workloads profiled
  (600 execs 11,522 -> 7,737; 1500 `/nix` reads 29,350 -> 12,798; 2000 `/workspace` reads 38,230 ->
  13,362; cache write+rename+read 22,854 -> 15,768; journal appends 11,861 -> 8,565), with `READ`,
  `LOOKUP` and `GETXATTR` identical on both sides of every pair. Cost: a host-retargeted symlink
  stays stale for `entry_timeout` (5 s), the same window zero-message open already trades.
- **`entry_timeout` was priced and does not pay**: with the new per-device setters, entry 5 s -> 60 s
  -> 300 s cuts `LOOKUP` 20-25% on `/nix` but the same requests return as `GETATTR` (333 -> 1,810),
  net 0-5%, and the arm that raises both timeouts is no better than raising one. The remaining
  `LOOKUP`s name *distinct* paths (1500 different store binaries, 2000 different files, each by a
  fresh process), so no timeout can cache them. The setters stay (a store that repeats names can use
  them), but they are not a lever and do not justify the `/nix` export on their own.
- **Two facts recorded with it**: the levers are **connection-wide, not per mount** - `/`,
  `/workspace`, `/nix` and `/home/dev/.codex` are grafts inside one root export and reach the guest
  as `FUSE_ATTR_SUBMOUNT` sub-volumes (`INIT` is 1 in every arm), so a per-path lever needs a second
  export; and the remaining **`GETXATTR` is entirely `security.capability`**, one request per
  process start (`distinct=1` in all five profiles), removable only by `xattr = false`, which would
  stop the guest honouring file capabilities. Numbers, instrumentation and the open ladder:
  [note 40](notes/40-cache-symlinks-shipped.md).

## Decided (2026-11-04): no `/nix` export, the host overlay already is the two layers

The store's two-layer requirement - reuse the workspace's nix cache, still pick up the
packages a *new* cang image brings - is met by the shipped host overlay: `lowerdir` is the
new image's snapshot, the workspace upper is kept across image moves and cloned into the
new generation (`seed_from_previous`), and the old store DB is parked for the guest to
merge. Cost is a one-time reflink clone (0 bytes, ~10 s) when the image changes.

Exporting the layers separately and merging them **in the guest** was implemented and
measured, and it is blocked by the guest kernel, not by cang: overlayfs refuses a FUSE
mount as upper (`overlayfs: upper fs does not support tmpfile`, `... does not support
RENAME_WHITEOUT`, `failed to set xattr on upper`), while the same option string over a
tmpfs upper mounts fine in the same guest. Reopening it means `O_TMPFILE` + whiteout
support in the fork's passthrough, or the server-side union. See
[note 43](notes/43-nix-export-guest-overlay-blocked.md); the two research notes
([41](notes/41-nix-export-host-plan.md), [42](notes/42-nix-export-guest-and-fork-facts.md))
hold the code-level plan if that work is ever scheduled.

## Withdrawn / superseded

- **The worker pool** (ticket 04): readdirplus is the one class the
  single `fs worker` really serialises (70% of a core at *one* guest thread, 95% at
  64, scan p50 21.8 ms -> 1048 ms). Bounded pool behind the single queue,
  virtiofsd's shape; A/B on note 20's probe and a full arm.
- **Zero-message open** (ticket 19, next): `FUSE_NO_OPEN_SUPPORT` removes
  `OPEN`+`RELEASE` per file and `OPENDIR`+`RELEASEDIR` per directory - the op class
  the pool cannot help, and the one rustc hammers. Needs a per-inode fd cache so
  `READ`/`WRITE` can arrive with `handle == 0`.

## Not yet specified

- **Whether the ~7k requests/s ceiling can be raised at all.** Every host-side
  explanation has been measured out of it - pool size, request-queue count,
  notification batching, the connection's background credit (note 44 §4-6,
  [ticket 21](tickets/21-multiqueue-request-queues.md)) - and the cost split says
  the guest owns 48 % and the FUSE server only 27 % of it
  ([note 46](notes/46-request-cost-split-and-recommendation.md)). So nothing on
  this axis crosses 2x, and it graduates only if a fork cycle is explicitly
  wanted (virtio-pci/MSI-X, inline completions) or a way is found to issue one
  round trip for several path components.
- **Whether 8.2 s is the accepted cost of a compile-test iteration on a live host
  tree** (against 1.45 s on the host and 1.3-2.1 s on guest tmpfs). If yes, this
  effort's metadata thread is done; if no, the next artifact is a decision on
  [ticket 18](tickets/18-workspace-target-on-block.md), not a FUSE change.
- **Whether `--fs=negative-entry:30000` should be the documented setting for
  compile-test loops, or the default.** It buys 33 % of an iteration on the
  2401-crate tree (11.8 s -> 7.9 s, [note 45](notes/45-compile-iteration-lookup-diagnosis.md) §5)
  at the cost of a name the *host* creates staying invisible for 30 s instead of
  250 ms. Shipping it as the default would weaken ticket 03's coherence contract;
  shipping it as documentation needs one line in `docs/usage.md` and a decision
  from bob.
- **Whether the remaining gap should be attacked at all.** The share now costs
  ~1 % of a cold 2401-crate build (116.3 s vs 115 s host) while the VM as a whole
  costs 1.35x the host - so anything left here serves metadata-heavy workloads
  (`git checkout`/`status`, package installs, target-dir churn), not compiles. If
  bob wants *those* fixed rather than explained, the candidates are outside this
  map's scope: `/workspace` on a block device, a share split so `target/` can be
  cached and sources cannot.
- **Whether the VM penalty itself is worth attacking** (2.2x the host, i.e. the
  larger half): CPU model, `vdso`/`virtio` interrupt affinity, vCPU count vs
  `JOBS`, transparent hugepages (the concurrent THP effort), sccache inside the
  guest. Nothing here is virtio-fs, so it is a candidate for a *new* map rather
  than this one.


- **Whether the pool is even reachable under exact visibility.** `NO_OPEN_SUPPORT`
  and handle reuse interact with "a host `git status` agrees with the guest":
  the pool itself is a scheduling change, but the follow-on round-trip savings
  may all sit on the wrong side of ticket 03's rule. Bothered only after ticket
  02's numbers.
- **How far the ENOENT fix reaches.** If the mechanism turns out to be
  guest-side caching of negative dentries or host-side changes the server never
  announces, the fix is not just "make cargo build" - it is a coherence rule for
  every `/workspace` user, and what it implies for the exact-visibility promise
  (and for a possible `FUSE_NOTIFY_INVAL_ENTRY` path in the fork) graduates from
  ticket 08.
- **What a `/workspace`-only win is worth** while `/nix`, the cargo registry and
  sccache stay on the same share and the same worker. If ticket 02 shows the
  penalty is spread across those paths, a workspace-only change may measure flat
  - which decides whether the excluded share split has to be revisited.
- **How much of the penalty sccache already hides.** A warm-cache incremental
  build barely touches the workspace; if ticket 02's baseline shows only
  cold/full builds hurt, the "rust development experience" complaint is really
  about first builds and dependency compiles, which changes what to optimize.

### Corrected facts from ticket 08

- cang attaches **all** its virtio devices through the MMIO manager
  (`launcher.rs:221-222`, `:657-658`); the PCI-only reset paths never run for the
  share. Note 01's PCI remark is wrong.
- A full ring returns `-ENOSPC` (`virtio_ring.c:718`), which the guest defers
  (`virtio_fs.c:1523-1532`) and never logs: queue saturation cannot produce the
  `-5` line, so "the single queue is saturated" is not observable that way.
- `cang` returns exit 0 about a second after boot while the guest is still
  working, and a guest that powers off without reporting a status becomes exit 0
  (`vmm/mod.rs:502-525`). Nothing in the benchmark may rely on cang's exit status
  or on forwarded stdout; the result-file protocol exists for that reason.

## Out of scope

- **`/workspace` as a virtio-blk filesystem image** (ext4/btrfs/erofs plus a
  sync policy to the host tree). A real option with parallel reads already
  enabled on the block path, but bob did not select it as a lever on 2026-10-25;
  it returns only if this destination is redrawn.
- **Moving hot IO off virtiofs as a shipped change** (target dir, sccache or the
  cargo registry on a guest-local disk). Bob chose "tuning + baseline" over
  "fast builds any way", so guest-local disk appears **only** as a benchmark
  control, never as a product default.
- **Reworking cang's mount layout for its own sake** (one share per grafted
  path, `/nix` on a separate device). Not selected as a lever; see the reading
  under Notes for the narrow exception that stays allowed.
