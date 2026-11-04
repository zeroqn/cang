# 45 - Why a compile-test iteration re-issues ~69k lookups, and the one lever that removes a third of them

**Measured 2026-11-04** on `main` (`d1b8fbf`-era tree), guest 7.2.10-hardened1,
toolchain rustc/cargo 1.99.1 (the image's, matching the devshell), 26 vCPUs,
`--mem 10`, one boot per arm. Workload: `tools/rust-compile-bench`'s synthetic
tree (2401 crates, 16801 `.rs`), `cargo build --offline`, `CARGO_INCREMENTAL=0`,
`RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=26`, warm `target/` — i.e. the *iteration*
shape of a compile test, not a cold build.

Repo state: the fork edits used for the experiments (multi-queue, a temporary
lookup trace) were reverted; the shipped tree is unchanged by this note. Arms ran
from a cargo-built binary at `CANG_LIBKRUNFW_LIBRARY=…/libkrunfw.so.5`, RPATH set
to virglrenderer's store lib.

## 1. The iteration is the fs, not the compiler

| arm | no-op rebuild | one-leaf edit + rebuild |
|---|---|---|
| host (ext4, no VM) | **1.45 / 1.48 / 1.44 s** | 2.51 / 2.51 s |
| guest, tree on `/dev/shm` (tmpfs, same VM) | **2.12 / 0.87 / 1.27 s** | 6.85 s |
| guest, tree on `/workspace` (virtio-fs) | **10.9 / 10.6 / 10.7 s** (11.3-11.8 s in later arms) | 13.0 / 12.5 s |

A no-op rebuild compiles nothing: cargo only decides that 2401 crates are fresh.
On tmpfs that costs ~1.3 s, on the share ~11 s. **~87 % of a repetition is the
share**, and the whole VM penalty (2.2x on real compilation) is absent here —
tmpfs matches the host.

## 2. What one iteration asks the server for

`CANG_FS_STATS=1` over four iterations (one boot): **370 042 requests**, of which
`LOOKUP 293 771 (79.4 %)`, `READ 26 068 (7.0 %)`, `GETXATTR 19 781 (5.4 %)`,
`WRITE 15 568 (4.2 %)`, `SETATTR 9 713 (2.6 %)`, `GETATTR 4 434 (1.2 %)`. That is
~92k requests per iteration and ~7k requests/s — the same ceiling the metadata
storm hits (note 44), with no server thread saturated.

`OPEN` is 3 requests in total (zero-message open) and `FLUSH` is absent
(`no-flush`), so the request classes left are the ones no shipped lever removes.

## 3. Are those lookups repeats, or a fresh walk?

A temporary trace in `PassthroughFs::lookup` counted distinct `(parent, name)`
pairs and the most frequent names:

- distinct pairs **plateau at ~69 213** and then never grow, while the total
  keeps climbing to 320k with **78 % repeats** — i.e. every iteration re-asks for
  the same ~69k names.
- top names per 4 iterations: `src` 19 223, `lib.rs` 19 207, `bin` 9 801,
  `main.rs` 9 612, `examples` 9 612, `Cargo.toml` 9 608, `README.md` 9 604,
  `README.txt` 9 604 (then `build.rs`, `tests`, `benches`, `README`).

That mix is **cargo's per-crate target auto-discovery**: for each of 2401 crates
it probes `src/lib.rs`, `src/main.rs`, `src/bin/`, `examples/`, `benches/`,
`tests/`, `build.rs`, `README*` — and most of those candidates *do not exist*
(~9.6k missing-name probes per iteration, answered by the negative entry and so
not counted as `-ENOENT` in the histogram, which reports 100 % positive).

## 4. Why they are not cached

- A `find` walk of the same tree is **2.39 s cold, 0.67 s warm** 8 s later — so
  the dentry cache does survive across processes, and 300 s `entry_timeout`
  changes nothing about that walk (0.65 s).
- A cargo iteration lasts 11-15 s, longer than the 5 s `entry_valid`, so each
  iteration starts with an expired walk and re-issues all ~69k names.
- Raising `entry_timeout` does **not** remove them, it converts them:
  5 s → 300 s took `LOOKUP` from 79 % to 27 % while `GETATTR` rose to **51.5 %**
  (173 235 requests), net zero — the same exchange note 35 recorded for 5 s → 60 s.
- Raising `attr_timeout` is actively harmful (60 s on both: 20.7 s and 14.1 s
  per no-op): cargo decides freshness from mtimes, so stale attributes make it
  redo work.

## 5. The one lever that pays: keep the *missing* names longer

Arms on the identical tree, steady-state no-op iterations:

| setting | no-op | vs base |
|---|---|---|
| base (`entry` 5 s, negative 250 ms) | 11.8 s | — |
| `entry` 60 s | 11.2 s | -5 % |
| `entry` 300 s | 12.0 s | 0 |
| **negative entry 30 s** | **7.8-8.2 s** | **-33 %** |
| negative entry 300 s | 8.1-8.9 s | -30 % |
| negative 30 s + `entry` 300 s | 7.3-7.6 s | -38 % |
| (for reference) target/ on tmpfs, sources on the share | 7.0-7.2 s | -40 % |

The negative-entry lifetime ships as `--fs=negative-entry:<ms>` /
`CANG_FS_NEGATIVE_ENTRY_TIMEOUT`, so **`cang --fs=negative-entry:30000` is the
whole change**: it removes the ~9.6k missing-candidate probes per iteration,
which is a third of the iteration, and unlike the `GETATTR` exchange it removes
requests instead of renaming them.

Cost, unchanged in kind from the 250 ms default: a name the *host* creates stays
invisible to the guest for that long. For a compile-test loop that is the right
trade; for an editor-driven workspace it is not, which is why the 250 ms default
stays.

## 6. What is left, and what was ruled out today

After the negative-entry lever the iteration is ~7.5 s against tmpfs's ~1.3 s.
The remainder is one round trip per *existing* path component per iteration at
the ~7k requests/s ceiling, and every host-side explanation of that ceiling was
tested and failed:

- **worker pool 4 → 16**: flat (mean 6.3 k vs 5.8 k creates/s, note 44 §4);
- **request-queue count 1 → 4** (implemented in the fork: `num_request_queues`
  from the config word, `1 + n` queues, one dispatcher; verified in the guest —
  requests landed as `#0=11 #1=276 #2=1065 #3=306 #4=112`): **flat** on both
  shapes, storm 4.6-8.2 k with 4 queues vs 5.2-9.3 k with 1, iterations 11.9 s
  vs 11.3 s. A 26-process storm cannot exceed the same ~7k ops/s with either
  count, and the dispatcher stays at ~40 % of a core, so the handoff is not the
  serial resource. Code reverted (ticket 21);
- **notifications**: already batched (~43 replies per host→guest interrupt);
- **connection credit**: already maxed (libkrun's INIT reply sets
  `max_background = u16::MAX`, note 48 §2) *and* structurally irrelevant - it gates
  async allocations only, while every request in this workload is a synchronous
  `fuse_simple_request`;
- **longer metadata lifetimes**: exchange, or worse (section 4).

That leaves the guest-side FUSE path itself (~55 µs of vCPU time per request,
three VM exits per round trip — note 22) as the cost, which no host-side change
reaches.

## Reproducing

Scripts and raw arms under `/home/dev/cang-bench/`: `guest-virtiofs/noop.sh`
(iteration shape), `ctl-noop.sh` (tmpfs control), `walk.sh` (find twice),
`ab-queues.sh` (queue-count A/B), `tgt.sh` (`CARGO_TARGET_DIR` on tmpfs), with
results `ab-*.txt` and cang logs `*.log`. The fork's temporary lookup trace is
reverted; its shape is described in section 3.
