---
label: wayfinder:research
title: Can a pinned databend tree be the real-world workload arm, offline and repeatably?
status: closed
blocked_by: [02]
claimed_by: pi (2026-10-25, closed before any work)
---

## Question

Bob's suggested real-world workload is a shadow clone of databend (nightly
release tag, e.g. `v1.2.954-nightly`) mounted as `/workspace` and built in the
guest. Answer what that would take, so the harness can carry it as an optional
mode:

1. **The pin.** Which tag/commit, its size on disk, and the resolved crate
   count / target count of `cargo build` at that commit (a `cargo metadata`
   crate graph, not a guess).
2. **Build requirements inside the cang image.** Does it need network at build
   time (`cargo` fetching), `protoc`, a C/C++ toolchain for `rocksdb`/
   `openssl`/`zstd`, `clang`/`lld`? Which of those the image already ships, and
   which would have to be added (that is image work with a pin, so it must be
   named before it is decided).
3. **Offline repeatability.** Whether a vendored `cargo vendor` tree is
   feasible for it (size, git dependencies, build scripts that shell out), or
   whether the arm simply requires network and is therefore not a CI arm.
4. **Cost.** Expected wall clock per arm on this host (28 CPUs) and in a guest
   of cang's default size, and how many runs the three arms need for a stable
   number. If one arm is tens of minutes, say so plainly - it decides whether
   this stays a manual, occasional arm behind a flag.
5. **Verdict.** Is databend a usable workload arm, and if so, what is the exact
   invocation the harness should record?

## Deliverable

`notes/06-databend-workload.md`: pin, requirements, cost, and the recommended
invocation (or the reason the arm is not worth it).

## Resolution (2026-10-25, bob's decision - superseded, no research done)

**Superseded before it was worked: the real-world workload is cang's own tree,
not databend.** bob asked whether our own code should be the rust-compilation
workload, and feasibility was checked on the spot: a copy of this repo (119 MB,
1356 files, `deps/` submodules included, `target/`/`.git` excluded) builds
`cang` end to end with the image's toolchain in **84 s**, fully offline
(`cargo build --offline -p cang`), including bindgen/libclang, the proc-macro
crates and the final link. That beats databend on every axis this ticket was
meant to weigh: an external pin becomes this repo's own commit, no vendoring
problem, no `protoc`/rocksdb/openssl image work, and the build is already in the
same wall-clock bracket as the synthetic arm.

The arm is added as `--workload cang` by
[Add the cang-tree workload arm](07-cang-tree-workload-arm.md), and **both**
workloads stay: the synthetic tree keeps its 16801 files (the metadata/stat
churn the single-worker virtio-fs design is most suspected of), the cang tree
supplies the heavy per-unit compile and the real crate graph.
