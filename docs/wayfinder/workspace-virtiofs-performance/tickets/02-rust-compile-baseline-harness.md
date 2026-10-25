---
label: wayfinder:task
title: Build the three-arm rust-compile baseline (host, guest-virtiofs, guest-local-disk) and record it
status: in_progress
blocked_by: []
claimed_by: pi (2026-10-25)
---

## Question

Not a decision - the work that unblocks every decision on this map: without a
number, "faster" is unmeasurable. Build the harness, run it, record the result.

1. **Where it lives.** A tool under `tools/` in the style of the existing smokes
   (`tools/chromium-cang-smoke`, `scripts/cang-pty-benchmark.sh`): a script plus
   a README that carries the recorded baseline. It drives the real host CLI
   (`nix build .#cang`, then the built `cang -- cargo build ...` from inside the
   workload tree, since cang mounts the CWD as `/workspace`), one outer timeout
   per arm, one log per arm, and it must never run a VM arm in the caller's
   shell (delegate each arm; rule #194).
2. **The workload.** A pinned, **network-free** small crate tree that still
   exercises the rustc IO shape (a generated crate graph with many small files
   and a dependency chain is fine; it must be byte-identical between arms and
   builds must not need the network - vendor or generate everything). It must be
   big enough to take minutes, not seconds, and small enough that three arms fit
   a session. Record the tree's provenance and size in the README.
3. **The three arms.** (a) host-native, building the same tree in a host temp
   dir; (b) guest over virtiofs, building in the mounted `/workspace`; (c) guest
   with sources **and** `CARGO_TARGET_DIR` copied to a guest-local disk, so the
   only variable left is VM CPU. Report wall clock per arm, `cargo`'s own phase
   times, and the ratio (b)/(c) - the virtiofs penalty - per run, with the run
   repeated enough times to show the spread.
4. **Toolchain control.** Both guest arms must use the same rustc and the same
   sccache state policy (warm vs cold stated explicitly), and link with the same
   linker (the image ships mold). State which build profile (debug? release?)
   and which `codegen-units`/`incremental` settings are used - they change the
   IO mix a lot.
5. **Record.** First cold numbers for all three arms, the exact commands, the
   image and `cang` commit, and the host/kernel line. Where the number should
   live: the tool README plus a line in `docs/diagnostics.md` next to the PTY
   benchmark.

Leaving the harness in place is the point: every later ticket re-runs it. If the
guest-local-disk control cannot be built (no writable guest-local disk with
enough space), say so and use a tmpfs control, noting the memory cost.

## Resolution

**Harness built and running** - `tools/rust-compile-bench/` (`gen-workload.sh`,
`cang-workload-env.sh`, `run.sh`, README with the recorded baseline).
First numbers and the two cang behaviours the harness has to work around are in
the README and in [ticket 09](09-cang-exit-status-lies.md); the attribution arm
changed from "guest container disk" to "guest `/dev/shm`" because a plain cang
guest attaches no containers disk (`/dev/vdb` absent, `/home/dev/.local/share/
containers` not writable) - recorded in the README.

Not closed yet: spread across repeated runs, and the `--workload cang` arm.

### First recorded numbers (2026-10-25/26, clean windows, one VM at a time)

Synthetic workload, 2401 crates / 16801 `.rs` files, cold `target/`, rustc
1.95.0, `CARGO_BUILD_JOBS=26`, sccache and incremental off:

| arm | build | vs host | vs guest-local |
|---|---|---|---|
| host | 104.5-106.4 s (several runs) | 1.00x | - |
| guest-local (`/dev/shm`, no FUSE) | 232.0 s | 2.2x | 1.00x |
| guest-virtiofs (`/workspace`) | 298.9 s | 2.9x | **1.29x** |

So the virtio-fs share costs about **29%** on top of the guest's own CPU/IO floor,
while the VM itself costs 2.2x the host. That reframes the map: most of the guest's
slowness is VM CPU, not FUSE - and a `/dev/shm` control *understates* the FUSE
penalty (RAM is faster than any disk), so 1.29x is a floor for the penalty, not a
ceiling.
