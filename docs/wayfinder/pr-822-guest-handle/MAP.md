---
label: wayfinder:map
title: PR 822 zero-copy guest-handle fast path into the fork
---

## Destination

`cang` runs libkrun **PR 822**'s zero-copy `CREATE_GUEST_HANDLE` fast path end to
end: `deps/libkrun` (the `zeroqn/libkrun` fork, branch `cang`, ABI-2 base) carries
the VMM half against a **rev-pinned `magma-gpu/rutabaga_gfx` git dependency**,
`deps/libkrunfw` carries `CONFIG_UDMABUF=y` plus the virtio-gpu guest-side patch
(**built and boot-tested locally before any tag**), both forks are published as
new releases and pinned in `nix/pins.nix`, and a cang guest is **shown actually
taking the fast path** - evidence, not "it builds".

## Notes

- Domain: `deps/libkrun` (`zeroqn/libkrun`, branch `cang`), `deps/libkrunfw`
  (`zeroqn/libkrunfw`, branch `cang`), `deps/wl-cross-domain-proxy` (the guest
  userland half, already merged as upstream PR #24 at `cc64c65`), `nix/pins.nix`,
  `nix/pkgs/libkrun-source.nix` (`fetchCargoVendor` hash - moves with the
  submodule pointer), `nix/pkgs/libkrunfw.nix`, `scripts/update-libkrunfw.sh`,
  the forks' release workflows, and the cang live-VM recipe.
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
    `v6.12.109-hardened1`) has **none** of the guest side: no `VIRTGPU_PARAM` 10,
    use mask `0x7`, `virtgpu_gem_prime_import()` = plain
    `drm_gem_prime_import()`, `CONFIG_UDMABUF is not set` in all six configs.
  - The Linux side is **in flux**: the PRIME-import change landed
    (`df4dc947c46b`) and was **reverted 2026-09-15** (it broke vrend); the
    re-send is gated on `VIRTIO_GPU_F_CREATE_GUEST_HANDLE`, and a virtio-comment
    series (v2, 2026-09-03) proposes bits **6 (CREATE_GUEST_HANDLE)** and
    **7 (BLOB_CTX_ID_FIX)**. cang's kernel already claims bit **5** for
    `VIRTIO_GPU_F_FENCE_PASSING` (`patches/0018`).
  - `deps/libkrun` HEAD `3d7af2c2` (`v2.0.0-cang.3`, branch `cang-main-rebase`);
    `deps/libkrunfw` HEAD `9616ca0`; `upstream/main` `a980e779` (2026-09-25);
    cang pin `v0.9.1`.
- Harness note: `rlm.spawn` children **do** have tools in this session (the
  prior map's research tickets were resolved that way).

## Decisions so far

<!-- the index: one line per closed ticket, enough to judge relevance, then zoom the link -->

- [What does PR 822's payload look like on the current ABI-2 fork tip?](tickets/03-port-matrix-on-abi2.md):
  not a cherry-pick - five of ten apply, the four rutabaga commits have no file
  to patch, `6c51645c` conflicts in `virtio_gpu.rs`'s imports, `4ef22a14`'s
  condition is dead on ABI 2 (`gpu_shm_size.is_some()` is the right one), the
  constant stays 6, and the new file-backed RAM costs the balloon's host-memory
  reclaim (ticket 10).

## Not yet specified

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
