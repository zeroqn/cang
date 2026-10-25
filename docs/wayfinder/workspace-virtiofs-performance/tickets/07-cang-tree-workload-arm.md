---
label: wayfinder:task
title: Add the cang-tree workload arm (--workload cang) to the benchmark
status: open
blocked_by: [11]
claimed_by: pi (2026-10-25)
---

## Question

Not a decision - the harness work bob's suggestion turned into. Extend
`tools/rust-compile-bench` with a second workload so the same three arms can
measure the repo itself:

1. **Tree.** A copy of this repo excluding `target/`, `.git`, `.cang` and any
   nested `target/` (119 MB, 1356 files, `deps/libkrun` + `deps/libkrunfw`
   included as plain files, exactly as `cargo build` sees them in-tree).
   `--workload cang|synthetic`, default unchanged (synthetic), so the recorded
   baseline keeps its meaning; `summary.tsv` gains a workload column.
2. **Build.** `cargo build --offline -p cang` with the same pinned toolchain and
   the same `RUSTC_WRAPPER=`/`CARGO_INCREMENTAL=0`/`CARGO_BUILD_JOBS` settings
   as the synthetic arm. Verified on the host: 84 s, offline, no network.
3. **The guest half.** The image ships rustc/cargo/clang/gcc/pkg-config/mold but
   **not** the headers `libkrun`'s build scripts need, so the guest build must
   resolve them from the `/nix` mount the guest already has:
   `PKG_CONFIG_PATH` for `virglrenderer` and `mesa-libgbm`, `LIBCLANG_PATH`,
   `KRUN_INIT_BINARY_PATH`, and the matching `-L` for the linker. Resolve them
   by glob at run time (not by hard-coding store hashes), fail loudly with the
   missing piece named rather than falling back to a different build, and record
   the resolved paths in the arm's log so a number is reproducible.
4. **Validate** one guest run of each of the two guest arms before recording
   anything, then record the cang-workload baseline in the README next to the
   synthetic one, with the same fields (per-run seconds, the two ratios, the
   toolchain line, the resolved nix paths).

Blocked in practice by
[Why does a cold cargo build hit ENOENT on freshly created target dirs in
/workspace?](08-virtiofs-enoent-on-new-dirs.md) for the virtiofs arm - a build
that cannot complete measures nothing.

## Resolution

(to follow)

### Blocker found by running it (2026-10-25/26, pi)

The arm is implemented (`--workload cang`) but **cannot build inside a cang
guest yet**: the guest's `/nix` (676 store paths, the image closure) ships clang,
libclang, gcc, rustc, cargo, pkg-config, mesa-libgbm and libdrm, but **not
`virglrenderer`** - and `deps/rutabaga_gfx/build.rs:16-19` probes
`virglrenderer.pc`. `cang-libkrun` depends on libkrun which depends on
rutabaga_gfx, so the interesting part of the workspace cannot be compiled without
it. (The env script also asked for `krun-init`, which no build needs - it is a
test assertion only; that demand was dropped.)

Two ways out, both a decision for
[ticket 11](11-image-ships-cang-build-inputs.md): ship the missing build inputs in
the image, or restrict the in-guest workload to crates the image can already
build. The arm's host number is recorded either way (55.9 s, offline).

Also recorded here because it cost an hour: a guest payload whose failure prints
nothing (its stdout is not forwarded) looks exactly like a VM that never launched.
The harness now runs every arm through one inner script that redirects *all*
output to `bench-run-<tag>.log` in the shared workspace and writes a
`BENCH_FAILED` result on exit, which is how the missing `virglrenderer.pc` was
found (`cang-workload-env: cannot build the cang tree, missing:
virglrenderer.pc`).
