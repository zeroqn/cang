---
label: wayfinder:map
title: PR 822 zero-copy guest-handle fast path into the fork
---

## Destination

`cang` runs libkrun **PR 822**'s zero-copy `CREATE_GUEST_HANDLE` fast path end to
end: `deps/libkrun` (the `zeroqn/libkrun` fork, branch `cang`, ABI-2 base) carries
the VMM half against a **rev-pinned `magma-gpu/rutabaga_gfx` git dependency**
and is compiled into cang by cang's own rustc (it is not a released artifact -
the submodule pointer is the pin, and cang's vendored-crate hashes move with
it); `deps/libkrunfw` carries `CONFIG_UDMABUF=y` plus the virtio-gpu guest-side
patch and is **published as a new release and pinned per system in
`nix/pins.nix`** (built and boot-tested locally before any tag); and a cang guest
is **shown actually taking the fast path** - evidence, not "it builds".

## Notes

- Domain: `deps/libkrun` (`zeroqn/libkrun`, branch `cang`),
  `crates/cang-libkrun` (the wrapper that links the fork's **Rust API** by path,
  `libkrun = { path = "../../deps/libkrun/src/libkrun" }`),
  `crates/cang/src/runtime/vm/libkrun/launcher.rs` (`GpuMode`, `configure_gpu`),
  `deps/libkrunfw` (`zeroqn/libkrunfw`, branch `cang`),
  `deps/wl-cross-domain-proxy` (the guest userland half, already merged as
  upstream PR #24 at `cc64c65`), `nix/pins.nix`, `nix/pkgs/cang-rust.nix`
  (`cargoDeps` - vendors libkrun's crates because cang compiles them),
  `nix/pkgs/libkrun-source.nix` (`libkrunCargoDeps`, the musl `krunInitBinary`),
  `nix/pkgs/libkrunfw.nix`, `scripts/update-libkrunfw.sh`, and the cang live-VM
  recipe.
- Skills: `cang-create-guest-handle-fast-path-prereqs` (the three gates),
  `cang-libkrun-family-fork-rebase-onto-release` (libkrunfw configs, patch
  ordering, `MakefileLto` seeds, the unwrapped-CC trap, per-system repin),
  `cang-fork-versioned-release-tags-and-pins` (permanent tags, the pin gate),
  `cang-libkrun-v1-to-v2-api-port-mapping` (the fork is ABI 2),
  `cang-local-validation-gates`, `cang-state-and-storage-root` and
  `nix/pins.nix` CONFIG_VALUES for the live-VM recipe.
- Tracker: **local markdown**, this directory. `gh` is unauthenticated in this
  session; bob pushes branches and tags.
- Prior map: [`../libkrun-main-rebase/MAP.md`](../libkrun-main-rebase/MAP.md).
  PR 822 was deferred there (its ticket 03) *conditional on this effort's
  prerequisites*, which its **Out of scope** records. Evidence:
  [`../libkrun-main-rebase/notes/02-pr-822-inertness.md`](../libkrun-main-rebase/notes/02-pr-822-inertness.md)
  and eight `02-raw-*` files beside it. Do not re-litigate the deferral.
- **Execution is in scope** (bob, 2026-09-28): this map ends with releases
  published, pinned and the fast path verified - not with a plan.
- Destination decisions (bob, 2026-09-28): **end-to-end live fast path**;
  **the libkrunfw release is in scope**, but the kernel is **built and tested
  locally first** and bob tags only after that, because the `-lto` asset takes
  hours; **rutabaga_gfx is a rev-pinned git dependency**, not a submodule; bob
  pushes the `zeroqn/libkrun` and `zeroqn/libkrunfw` tags.
- Starting facts (verified 2026-09-28):
  - **cang links libkrun's Rust API** (`038ef6f`): `crates/cang-libkrun` depends
    on `deps/libkrun/src/libkrun` by path, so **there is no libkrun prebuilt pin**
    (`libkrunRelease`, `nix/pkgs/libkrun.nix` and `scripts/update-libkrun.sh` are
    gone) and no fork release is on cang's critical path - the submodule pointer
    is the pin. Only `libkrunfwRelease` is pinned. **Both** vendored-crate hashes
    move with a libkrun change: `nix/pkgs/cang-rust.nix` `cargoDeps` and
    `nix/pkgs/libkrun-source.nix` `libkrunCargoDeps`.
  - PR 822 is **still open, draft**, head `3819ce5` (2026-08-27), base `0d75eb4b`
    (pre-ABI-rewrite), `mergeable=false`; unchanged since August.
  - `magma-gpu/rutabaga_gfx` main = `ec60ee11` (2026-09-25). PR #81 (the
    guest-blob-handle half) **merged 2026-09-02**, but routed through the
    **context** (`ctx_id > 0` -> `context_create_blob`), whereas 822 routes it
    through the **component** (`ctx_id == 0`). Main still lacks
    `RUTABAGA_BLOB_FLAG_CREATE_GUEST_HANDLE` and component-level handle
    retention. Latest crates.io release is **0.1.85 (2026-08-06)** - main is
    unreleased.
  - The git-dep route is mechanically clean: upstream libkrun already carries a
    git dependency (`ffier`), rutabaga's `third_party/mesa3d` is a plain tree
    (not a submodule), and `fetchCargoVendor` vendors git deps.
  - Pinned kernel (`libkrunfw v5.6.2-cang.1`, `linux-6.12.109` +
    `v6.12.109-hardened1`) had **none** of the guest side: no `VIRTGPU_PARAM` 10,
    use mask `0x7`, `virtgpu_gem_prime_import()` = plain
    `drm_gem_prime_import()`, `CONFIG_UDMABUF is not set` in all six configs.
    (Superseded 2026-09-29: `deps/libkrunfw` `3fdbb59` carries it; the *pinned
    release* is still the old asset until ticket 06 re-pins.)
  - The Linux side is **in flux**: the PRIME-import change landed
    (`df4dc947c46b`) and was **reverted 2026-09-15** (it broke vrend); the
    re-send is gated on `VIRTIO_GPU_F_CREATE_GUEST_HANDLE`, and a virtio-comment
    series (v2, 2026-09-03) proposes bits **6 (CREATE_GUEST_HANDLE)** and
    **7 (BLOB_CTX_ID_FIX)**. cang's kernel already claims bit **5** for
    `VIRTIO_GPU_F_FENCE_PASSING` (`patches/0018`).
  - `deps/libkrun` HEAD was `3d7af2c2` (`v2.0.0-cang.3`, `cang-main-rebase`);
    `deps/libkrunfw` HEAD `9616ca0`; `upstream/main` `a980e779` (2026-09-25);
    cang pin `v0.9.1`. (HEADs move - see *Status* below.)
- Harness note: `rlm.spawn` children **do** have tools in this session (the
  prior map's research tickets were resolved that way).

## Status (2026-09-29)

**The destination's first half is done: the port and the kernel are landed and
the fast path is verified live.** Ticket 06 (publish the libkrunfw release and
re-pin per system) is the only open ticket, and it needs bob's push/tag.

- `deps/libkrun` (branch `cang`) tip **`63f3737f`**: `a1a772a0` carries the port
  on the `ctx_id` route, `5d9cb075` the lock + regenerated bindings, `63f3737f`
  the udmabuf list coalescing fix ticket 09 needed. cang's submodule pointer
  `6166b8c` is the pin; both `fetchCargoVendor` hashes are current
  (`cang-rust.nix` `sha256-/tsacxWjGgl9uCRNkaO9pF893yn0gXIMhqORxnekYfw=`,
  `libkrun-source.nix` `sha256-5Snz7O5nbcg0qVgLPhSzFdGUk5+pqy+Iavt0mE3FLaQ=`).
- `deps/libkrunfw` (branch `cang`) tip **`3fdbb59`**: patches `0037-0039` +
  `CONFIG_UDMABUF=y` in all six configs.
- Verified live (tickets 05 and 09, notes 05/09): the guest boots on the locally
  built firmware, `/dev/udmabuf` + `VIRTGPU_PARAM` 10 + bits 6/7 gate exactly on
  `--zero-copy-shm`, a `wl_shm` client through the proxy takes
  `UDMABUF_CREATE`+`PRIME_FD_TO_HANDLE` and no copy ioctl, and the measured A/B is
  -5.4% guest CPU (2.800 s -> 2.650 s busy over 6 s at 1920x1080x120). The
  Chromium GPU smoke passes with hardware venus on the same tree.
- Mechanics worth not rediscovering: the udmabuf driver's `list_limit` (1024
  runs) and `size_limit_mb` (64 MiB) both surface as a bare `EINVAL`, and the
  guest names one run per 4 KiB page; the host's `/dev/udmabuf` must be openable
  from cang's keep-id user namespace (0666, like /dev/kvm) and the guest's must be
  openable by the task user (guest-init does it now, `57fc4ec`).
- Closed: 01, 02, 03, 04, 05, 07, 09, 10, 11, 12 (08 cancelled). Open: **06**.

## Decisions so far

<!-- the index: one line per closed ticket, enough to judge relevance, then zoom the link -->

- [Backport the PRIME-import prerequisite onto 6.12.109, or move the libkrunfw kernel base to 6.14+?](tickets/11-kernel-base.md):
  **backport** - carry Vivek's v5 series and Val's six commits as `patches/0037...`
  plus `CONFIG_UDMABUF=y`; a base move would not even remove the unlock work
  (every stable line 6.14-6.18 still carries the reverted gate), so it stays a
  separate future effort.
- [Decide the design, the gate and the feature-bit map the fork carries](tickets/04-carried-design.md):
  the **`ctx_id` route** (bit 7) with 822's three rutabaga hunks **dropped**; bits
  frozen at 5 = FENCE_PASSING (ours), 6 = CREATE_GUEST_HANDLE, 7 =
  BLOB_CTX_ID_FIX; the gate is cang's **`--zero-copy-shm`** companion flag
  (default off, one bool driving both the RAM backing and feature advertisement,
  balloon inert on fast-path runs by design); unavailable means **warn and take
  the copy path**; the fork **logs** a handle-dropping mis-routed ctx instead of
  failing it; evidence is the proxy mode line + an observed `udmabuf_create` +
  negotiated bits + a measured A/B `wl_shm` delta with the Chromium smoke as
  backstop.
- [Does the udmabuf fast path need all guest RAM file-backed, or only the GPU shm window?](tickets/10-udmabuf-ram-backing.md):
  all of it - the blob's pages are the imported dma-buf's sg list, i.e. ordinary
  RAM, which the shm window (a device BAR above `ram_last_addr`) can never hold;
  narrowing fails *silently*, so the answer is one opt-in gate that withholds
  the feature bit and keeps anonymous RAM together, default off until ticket 09.
- [What does the ported VMM half still need from rutabaga_gfx?](tickets/01-rutabaga-delta.md):
  nothing beyond a rev-pinned git dep, **if** the fork adopts the `ctx_id` /
  `BLOB_CTX_ID_FIX` route magma-gpu main already implements - 822's three
  rutabaga hunks are then dropped, not carried; the alternative is a second fork
  repo. Two `fetchCargoVendor` hashes move (`libkrun-source.nix` and
  `cang-rust.nix`).
- [Which Linux guest-side patch set gives 6.12.109 CREATE_GUEST_HANDLE?](tickets/02-kernel-patch-set.md):
  no single series - backport Vivek's v5 PRIME-import base (first in v6.14),
  port Val's un-posted `guest-handle` branch (`3e6a365d2ac9`), author the
  conditional PRIME-import unlock the revert left behind, set `CONFIG_UDMABUF=y`
  in all six configs; keep 5 = FENCE_PASSING and add 6/7; the proxy needs no
  change.
- [What does PR 822's payload look like on the current ABI-2 fork tip?](tickets/03-port-matrix-on-abi2.md):
  not a cherry-pick - five of ten apply, the four rutabaga commits have no file
  to patch, `6c51645c` conflicts in `virtio_gpu.rs`'s imports, `4ef22a14`'s
  condition is dead on ABI 2 (`gpu_shm_size.is_some()` is the right one), the
  constant stays 6, and the new file-backed RAM costs the balloon's host-memory
  reclaim (ticket 10).

## Not yet specified

- **Whether the 6.12.109 backport is retired once upstream's series lands.**
  Ticket 11 may choose to bump the base instead of backporting; if it backports,
  the six carried patches (plus our gate) are dropped when `guest-handle` is
  posted and merged. Sharpens with ticket 11's answer and Val's v3.
- **The latent bit-5 collision on a future libkrunfw rebase.** Upstream mainline
  assigned `VIRTIO_GPU_F_BLOB_ALIGNMENT = 5` while cang's fork kernel uses bit 5
  for `VIRTIO_GPU_F_FENCE_PASSING` (`patches/0018`). Today that only forces
  re-authored hunks (ticket 02); if `deps/libkrunfw` ever rebases past v6.13 the
  two must be reconciled deliberately, and the fork's 0018 may have to move.
  Sharpens when a libkrunfw kernel bump happens.
- **When the libkrunfw base moves past 6.13.** Ticket 11 deliberately backported
  instead of bumping, because a bump does not remove the unlock work; but a base
  move will happen at some point, and it has to reconcile cang's
  `VIRTIO_GPU_F_FENCE_PASSING = 5` with upstream's `BLOB_ALIGNMENT = 5`,
  regenerate the LTO/KVM seeds and re-verify the 36 patches. Recorded, not
  scheduled.
- **Retiring the carried kernel patches.** When Val's series is posted to lkml and
  merged, the six commits (and our gate, if it goes upstream) are dropped on the
  next libkrunfw rebase. Sharpens per upstream posting.
- **Where the A/B `wl_shm` client lives.** Ticket 09 needs a small guest-side
  `wl_shm` client that no image layer or `tools/` directory has today;
  `tools/` (like `virgl-guest-probe`) or a new layer. Sharpens inside ticket 09.
- **Architecture coverage for the first libkrunfw release.** The kernel patch
  touches virtio-gpu and udmabuf, which are per-arch configs and CI jobs;
  x86_64 (`kvm-lto`) is the cang default but aarch64/riscv64 ship too. Sharpens
  once ticket 02 says how portable the patch is.
- **How the fast path is made observable.** Whether the evidence is proxy logs,
  a udmabuf/`udmabuf_create` counter, a tracepoint, a `DebugFS` count, or just
  "venus renderer + no copy path", and where that instrumentation hooks in.
  Sharpens inside ticket 04 and is executed by ticket 09.
- **Whether cang itself changes.** The fast path may be transparent to the CLI
  (image userland + kernel only) or may need a knob/env
  (`CANG_*`) to advertise the feature. Sharpens after ticket 04's design.
- **Retiring the git dependency.** When magma-gpu cuts a release (> 0.1.85) the
  fork should return to a registry dependency; how the Cargo.lock/fetchCargoVendor
  hash churn is handled. Sharpens once the pins move.
- **What the fork keeps when upstream lands 822.** 822 is a moving draft; if
  libkrun merges it (or its re-send), the fork's carried commits are dropped.
  Sharpens per upstream PR.

## Out of scope

- **Publishing/pinning a libkrun prebuilt release.** Obsolete since cang links
  the fork's Rust API (`038ef6f`): there is no `libkrunRelease` pin, no
  `nix/pkgs/libkrun.nix`, and no `scripts/update-libkrun.sh`. Ticket 08 was
  closed as cancelled; the submodule pointer plus cang's two `fetchCargoVendor`
  hashes are the whole pin.
- **Upstreaming the Linux guest-side patches** (the virtio-comment series and the
  drm/virtio re-send). We port them into `deps/libkrunfw`; upstreaming them is
  Val's effort, not this map's.
- **Upstreaming PR 822 itself** (asking libkrun to merge, or pushing the
  component-routing design upstream). The fork carries it; when upstream merges
  something, this map drops its copy and re-pins.
- **The dGPU-passthrough use of `CREATE_GUEST_HANDLE`.** Only the
  udmabuf/wl_shm zero-copy path is in scope; passing through a physical GPU over
  cross-domain is a different workload.
- **PR 822's `230f2c55` renumbering** (`VIRTIO_GPU_F_CREATE_GUEST_HANDLE` 6 -> 5).
  It collides with cang's `VIRTIO_GPU_F_FENCE_PASSING = 5`; any carry excludes or
  adapts it, and the constant stays 6.
- **A cang-side v1/v2 compatibility story for other consumers of the fork.**
  The fork is ABI 2 only (prior map, ticket 09).
