---
label: wayfinder:research
title: virtio-fs cache policy in cang - the same bit zero-message open already sets, what a --fs=cache-policy flag would cost, and the A/B that could measure it
status: open
blocked_by: []
claimed_by: pi child (2026-10-27, answering libkrun#939 for cang)
---

# 32 - Fs cache policy: plumbing plan and A/B recipe

Read first: [13](13-cache-semantics-and-scoping.md) (the modes, and why they are
connection-wide) and [23](23-zero-message-open-results.md) (the zero-message-open
lever, its instrument, and its one semantic cost). Read-only session: no builds,
no VMs, no tracked-file edits.

Question from the parent (upstream `libkrun/libkrun#939`): `CachePolicy::Auto`
makes every guest `open()` drop that file's guest page cache, which hurts shared
libs and mapped files. **Is cang's zero-message-open win the same mechanism, and
would switching the default to `always` add anything?**

## Verdict

1. **Same mechanism, same single boolean, and zero-message open already sets it.**
   `always` and zero-message open both end at `ff->open_flags & FOPEN_KEEP_CACHE`,
   tested once in the guest kernel at open: `fs/fuse/file.c:296-297` (v7.2)
   `else if (!(ff->open_flags & FOPEN_KEEP_CACHE)) invalidate_inode_pages2(...)`.
   With an `OPEN` the flag comes from the server's reply (`file.c:165`); with
   zero-message open there is no `OPEN`, and the kernel's own default at
   `file.c:155-156` - comment "Default for no-open" - is
   `FOPEN_KEEP_CACHE | (isdir ? FOPEN_CACHE_DIR : 0)`. **`always` is the same bit
   by a different road.**
2. **On cang's shipped path (`CANG_FS_ZERO_MESSAGE_OPEN = true`, `crates/cang-libkrun/src/lib.rs:68`)
   `always` adds nothing measurable.** The fork answers every `OPEN`/`OPENDIR` with
   `-ENOSYS` (`passthrough.rs:1389-1397`, `:1286-1291`), so the only `cache_policy`
   consumer left is the `CREATE` reply (`passthrough.rs:1472-1480`): under `always`
   a created file gets `FOPEN_KEEP_CACHE`, under `auto` it gets `0`. The kernel's
   create path takes the reply flags verbatim with no keep-cache default
   (`fs/fuse/dir.c:889`) and on `0` invalidates the new inode's page cache
   (`dir.c:915-916`) - an empty page cache, no FUSE request, no behaviour change.
   Predicted wire effect of flipping the policy with ZMO on: **zero requests.**
3. **It is only observable with `--fs=zero-message-open:off`, and there it is the
   worse half of a lever cang already ships.** Restoring page retention that way
   keeps paying the round trips ZMO deletes - `OPEN` 7.9-8.3% + `RELEASE`
   8.5-9.0% of a build's requests (note 23 §2, §8.6) - to buy back exactly the
   `READ` collapse ZMO gets for free (`READ` 35.8% -> 3.3%).
4. **cang has no way to name the policy today** - no CLI, no env var, no fork
   setter (`FsDevice`'s five knobs, `device_builders.rs:634-717`). The smallest
   thing that makes an A/B possible is **env-only** (`CANG_FS_CACHE_POLICY`,
   mirroring `CANG_FS_STATS`, which likewise has no CLI flag and no config field):
   ~55 lines across 6 files, 15 of them in the fork, and **no** new struct field.
5. **The flag the question asks about is ~55 lines plus the config model.** The
   natural spelling is not `--fs-cache-policy` but a second `--fs` key
   (`--fs=cache-policy:auto|always|never`), because `--fs` already exists, already
   rejects unknown keys, and already has a last-wins selector
   (`crates/cang/src/cli/mod.rs:281-287`, `:669-716`). Adding a struct field is
   what costs: `zero_message_open` - the identical `Option<bool>` - appears at
   **79 sites in 17 files**, 30 of them in `config/tests.rs` alone.
6. **The policy is per device, so per connection: one setting covers `/`, `/nix`
   and `/workspace`,** and it cannot break the boot (it changes reply flags, not
   mount options). There is no stale-`/nix` risk that zero-message open has not
   already introduced: `/nix/store` paths are content-addressed and the guest
   already keeps their pages today.

## 1. The mechanism, precisely (why the two levers cannot be added together)

| | ZMO on (cang today) | ZMO off + `Auto` | ZMO off + `Always` |
|---|---|---|---|
| does the guest send `FUSE_OPEN`? | no (`file.c:139`, `:157`) | yes | yes |
| who sets `FOPEN_KEEP_CACHE` | kernel default `file.c:156` | nobody -> `0` | server reply `file.c:165` |
| `invalidate_inode_pages2` at open (`file.c:296-297`) | no | **yes, every open** | no |
| kernel default for `OPENDIR` (`CACHE_DIR`) | `file.c:156` | none | server reply (`passthrough.rs:893-909`) |
| `CREATE` reply flags | `auto`: `0` / `always`: `KEEP_CACHE` (`passthrough.rs:1472-1480`), then `dir.c:915-916` on `0` | `0` | `KEEP_CACHE` |
| what it costs | nothing extra | close-to-open (today's `--fs=zero-message-open:off`) | `OPEN`+`RELEASE` round trips |

The fork's `CachePolicy` is `Never | Auto | Always` and is consumed in exactly
two places: `do_open` (`passthrough.rs:893-909`, unreachable when ZMO is on) and `create`
(`:1472-1480` under ZMO, `:1491-1496` without). Its `FromStr` already exists
(`passthrough.rs:353-363`), and `Default` is `Auto` (`:335-345`, `:466`), so the
"policy" is applied through per-open reply flags only - no `FUSE_INIT` bit, no
mount option (note 13 §1.1).

## 2. What `--fs=cache-policy:auto|always|never` would take in cang

### 2.1 The existing chain, with `zero_message_open` as the template

| layer | location |
|---|---|
| clap arg `--fs KEY:VALUE` + its long_help | `crates/cang/src/cli/mod.rs:279-288` |
| `FsOption` enum + strict parser (unknown key/value is an error) | `cli/mod.rs:669-700` |
| last-wins selector | `cli/mod.rs:704-716` |
| `RuntimeOptions` field | `cli/mod.rs:658`, filled at `:615` |
| `LaunchOptions` (the plan's input) | `runtime/launch/plan.rs:46`, filled at `:145` |
| cross-option validation (`bail!`) | `plan.rs:118-123` (`--zero-copy-shm requires --gpu=drm`) |
| `LaunchSpec` / `LaunchConfig` | `config/model.rs:409` / `:487` |
| spec -> config mapping | `config/mod.rs:135` |
| persisted VM-spec codec (serialize / allowlist / parse / ctor) | `config/codec.rs:159-164`, `:399`, `:457`, `:501` |
| session -> worker handoff | `runtime/session/mod.rs:318` |
| launcher resolve (CLI > env > default) + warn on bad env | `runtime/vm/libkrun/launcher.rs:76-105` |
| the actual device call | `launcher.rs:336-345` (right after `fs_device_set_worker_threads`) |
| trait + implementation | `crates/cang-libkrun/src/api.rs:53`, `linked.rs:351-361` |
| fake + exact call-order assertion | `runtime/vm/libkrun/tests.rs:42-82`, `:277-284`, `:676-681` |
| CLI test pattern | `cli/mod.rs:2162-2181` |
| struct-literal fixtures | 79 `zero_message_open` mentions in 17 files; 30 in `config/tests.rs` |

### 2.2 Fork side (a setter, no parse work, no C API on cang's path)

- `CachePolicy` and its `FromStr` already exist (`passthrough.rs:335-363`).
- `Fs` has `set_worker_threads` / `set_zero_message_open` / `set_opcode_stats`
  that mutate `self.passthrough_cfg` before activation
  (`devices/src/virtio/fs/device.rs:168-202`); the config is cloned into the
  worker at `device.rs:287`, so the setter must run before `attach`.
- `FsDevice` exposes `new`/`new_read_only`/`new_null`/`set_overlay`/
  `set_dax_window_size`/`set_worker_threads`/`set_zero_message_open`/
  `set_opcode_stats` (`libkrun/src/api/device_builders.rs:634-717`) and **no cache
  setter**: add one in the shape of `:703-705`, plus `Fs::set_cache_policy` in the
  shape of `device.rs:188-191` = ~10 lines.
- To let `cang-libkrun` name the type, one re-export beside the existing
  `DiskFormat`/`SyncMode` line (`libkrun/src/api/mod.rs:56-58`); without it the
  setter would have to take a `&str` and parse inside the fork.
- The C header is generated from the Rust annotations
  (`docs/bindings-generation.md:88`, `make gen-libkrun-bindings`;
  `include/libkrun.h`, `bindings/libkrun-via-cdylib-weak/ffier-krun.json`). cang
  links the Rust API by path and builds **without** the `ffi` feature
  (`crates/cang-libkrun/Cargo.toml`, `crates/cang-libkrun/src/lib.rs:1-12`), so the
  C ABI is not involved in cang's build - but the fork's committed header must be
  regenerated anyway, exactly as the ZMO change did (note 23 §1).
- A fork change means a new `v2.0.0-cang.<n>` tag, the submodule pointer, and the
  `libkrun-src` flake input/`flake.lock` together (project memory #141, #190); no
  `cargoDeps`/`krunInitCargoDeps` hash moves because no dependency is added.

### 2.3 Where validation belongs

1. **Value**: `parse_fs_option` (`cli/mod.rs:676-700`) - the file's own rule is
   "an unknown key or value is an error rather than a silently ignored flag", and
   the error text lists the supported keys (`:697-699`).
2. **Combination**: `LaunchPlan::new` beside `plan.rs:118-123`. This is the honest
   place for "`cache-policy:always` is inert while zero-message-open is on" - as a
   `tracing::warn!` (the launcher already warns for a bad env value,
   `launcher.rs:96-101`), or as a hard `bail!` if we would rather not ship a flag
   that can do nothing.
3. **Policy string -> enum** belongs in the fork (`CachePolicy::from_str`) or in
   `cang-libkrun`, not in clap, so the CLI never sees a type it cannot name.

### 2.4 The smallest thing that makes the A/B possible (recommended first step)

Env-only, modelled on `CANG_FS_STATS` (`launcher.rs:108-113`, which reads no
config field at all):

| file | change | lines |
|---|---|---|
| fork `devices/.../fs/device.rs` | `Fs::set_cache_policy(CachePolicy)` -> `passthrough_cfg.cache_policy` | ~5 |
| fork `libkrun/src/api/device_builders.rs` + `api/mod.rs` | `FsDevice::set_cache_policy` + re-export, regenerate bindings | ~8 |
| `crates/cang-libkrun/src/{api.rs,linked.rs}` | one trait method + one `Slot::FsDevice` arm (shape of `:351-361`) | ~20 |
| `crates/cang/src/runtime/vm/libkrun/launcher.rs` | `CANG_FS_CACHE_POLICY=auto|always|never` resolver, call after `:345` | ~15 |
| `crates/cang/src/runtime/vm/libkrun/tests.rs` | `Call::FsDeviceSetCachePolicy`, fake arm, one line in the exact call-order vec | ~6 |

**No** CLI, **no** config field, **no** codec, **no** fixture churn: one binary,
two launches. The flag variant is this plus the ~20 edits in §2.1 and the fixture
sweep, and it is only worth doing if the measurement says the knob earns a
user-facing surface - which, per §0/§4.1, it does not while ZMO is on.

## 3. Does anything already override the policy? (question 2)

No. Verified by grep over the whole fork and `crates/cang-libkrun`:

- the fork's fs subsystem reads **no** environment variable at all; the only
  `env::var_os` under `src/libkrun` is `KRUN_ENOMEM_WORKAROUND`
  (`vmm/linux/vstate.rs:1084`);
- `crates/cang-libkrun` reads exactly one env var, `CANG_LIBKRUNFW_LIBRARY`
  (`firmware.rs:40`);
- the launcher reads `CANG_FS_ZERO_MESSAGE_OPEN` (`launcher.rs:79`) and
  `CANG_FS_STATS` (`:109`).

So the only cache-semantics knob that exists today is
`CANG_FS_ZERO_MESSAGE_OPEN` / `--fs=zero-message-open:on|off` - which is also the
knob that decides whether the question is observable at all.

## 4. The A/B recipe

### 4.1 Arm matrix (all on ONE binary and ONE fork build)

| arm | env | expected histogram signature | what it proves |
|---|---|---|---|
| A1 `zmo`, policy auto | `CANG_FS_ZERO_MESSAGE_OPEN=1 CANG_FS_CACHE_POLICY=auto CANG_FS_STATS=1` | `OPEN 3`, `RELEASE 1`, `OPENDIR 1`, `READ` ~3.3% | the shipped default |
| A2 `zmo`, policy always | same, `=always` | **identical to A1** (same request count at the same milestone) | `always` adds nothing under ZMO |
| B1 `close-to-open` | `CANG_FS_ZERO_MESSAGE_OPEN=0 ...=auto` | `OPEN` ~8.3%, `RELEASE` ~9.0%, `READ` ~36% | today's opt-out, page cache dropped per open |
| B2 `close-to-open+cache` | `CANG_FS_ZERO_MESSAGE_OPEN=0 ...=always` | `OPEN`/`RELEASE` still ~8-9%, `READ` ~3% | retention bought, round trips kept |

A1 vs A2 is the answer to the parent's question; B1 vs B2 is the control that the
`always` arm is *capable* of producing the effect at all (otherwise "identical"
proves nothing about the flag, only about the build).

### 4.2 One probe gap to close first: a `readwarm` cell

The existing probe (`tools/rust-compile-bench/concurrency-scaling.sh`, heredoc C at
`:209-520`) has modes `open read stat statwarm readdirplus`. `read` calls
`posix_fadvise(POSIX_FADV_DONTNEED)` before every read (`:392-401`), i.e. it
**defeats page retention in both arms** and cannot discriminate. `open` measures
round trips only, which is exactly the half that does not differ between `auto`
and `always`.

Add `readwarm` = `openat(O_RDONLY)` + `read()` + `close()` with no `fadvise`:
under `auto` every iteration is a `FUSE_READ` (pages were dropped at open), under
`always` only the first one per file is. Edits, all inside that one file:
the enum (`:244`), `mode_names` (`:245`), the worker switch (a case after `M_READ`,
`:390-405`), and - if it should be in the default set - the warm-up loop (`:173`)
and `modes=` (`:62`). It is ~15 lines of C and is the cell that turns "predicted
zero" into a measured zero.

### 4.3 Micro arms (probe cells; one boot per arm, cells of 2 s)

```
cd /home/dev/cang/cang-vfio
# one invocation = one VM (the script launches cang itself, so export the env)
export CANG=/home/dev/cang/cang-vfio/result/bin/cang   # built from the fork under test
z=1; p=always                                          # A1: z=1 p=auto  A2: z=1 p=always
                                                       # B1: z=0 p=auto  B2: z=0 p=always
CANG_FS_STATS=1 CANG_FS_ZERO_MESSAGE_OPEN=$z CANG_FS_CACHE_POLICY=$p \
  tools/rust-compile-bench/concurrency-scaling.sh \
    --files 4096 --rounds 3 --threads "1 16 64" \
    --modes "open readwarm read stat statwarm readdirplus" \
    --bench-root /home/dev/cang-bench-cache/$p-zmo$z
```

Read: `<bench-root>/runs/concurrency-<stamp>/results/cells.txt` (one CELL line per
cell, guest begin/end clock) and `table.txt`; the `/dev/shm` cells are the in-boot
control (§4.6). Arms must be run **one VM at a time**, interleaved (A1 B1 B2 A2
A2 B2 B1 A1).

Expected shape, from note 23 §8.5 (its lever-off `open@1` was 1892 ops/s,
lever-on 5454): A1/A2 equal within noise; B2's `open` back to ~1900 ops/s while
its `readwarm` sits near A1's, B1's `readwarm` far worse. If B1 and B2's
`readwarm` cells are equal, the policy did not reach the server and the arm is
invalid, not a negative result.

### 4.4 Macro arms (full 2401-crate build)

`tools/rust-compile-bench/run.sh --arms guest-virtiofs --runs 1 --bench-root ...`
once per arm, interleaved by hand (note 23 §8 drove this with its own `arms.sh`;
`run.sh` loops one arm at a time), 4 runs per side, plus one `host` arm at the
start and the end as the drift yardstick. Export the same three env vars. Record
`summary-synthetic.tsv` rows (`bench_ms`, `fs_worker_pct`) and the histogram per
run.

Prediction to falsify: **A1 == A2 to within the host-drift band** (note 23 §8.2:
the lever-on arm was the stable one, 185.3 vs 185.1 s, while the lever-off arm
swung 366-563 s), and B2 ~= A1 + the `OPEN`/`RELEASE` cost.

### 4.5 Proving each arm exercised its path

| observable | how | A1/A2 | B1 | B2 |
|---|---|---|---|---|
| `OPEN`/`RELEASE` share at a fixed request milestone | fork histogram, `CANG_FS_STATS=1` | ~0 (3/1 probes) | ~8.3% / ~9.0% | ~8.3% / ~9.0% |
| `READ` share of the same build | histogram | ~3.3% | ~36% | ~3% |
| `readwarm` READs per iteration | histogram on a `readwarm`-only run | ~0 | ~1 | ~0 |
| guest sees a host in-place rewrite ~1 s later | note 23 §5's `coherence-immediate` probe | stale | fresh | stale |
| host added a file in a listed dir | note 23 §5 | visible | visible | visible |

The histogram is the only server-side witness and it does **not** print
`FOPEN_KEEP_CACHE`; if a direct count is wanted, add ~5 lines to the fork's
histogram report (`server.rs:76-181`, next to `count_opcode`): increment
`keep_cache`/`direct_io` in `passthrough::do_open` and `create` where the reply
opt is chosen (`passthrough.rs:893-909`, `:1472-1480`). Cheaper and equally
convincing for A1 vs A2: the request *counts* are bit-identical, and two arms
whose histograms match at 1 M requests cannot differ in reply flags that the
guest acted on.

**Histogram capture**: it is logged by the VMM/device threads inside the manager
helper, whose stderr cang captures to `<state-root>/tasks/<task>/helper.stderr.log`
and deletes at teardown (state root here: `/home/dev/disp`, `~/.config/cang/cang.toml`).
Tail it while the VM lives - `tail -F /home/dev/disp/cang/<ws>/tasks/*/helper.stderr.log
> fork-<arm>.log &` before the payload finishes - and grep
`opcode histogram`. A post-hoc read finds nothing.

### 4.6 Host-drift control

1. **In-boot**: every round of `concurrency-scaling.sh` runs the same cells on
   `/dev/shm`, alternating order per round (`:186-190`). If the shm cells move,
   the box moved; normalise to them (note 23 §3/§8.5 use exactly this).
2. **Across boots**: the `host` arm of `run.sh` is the same build with no VM and
   no FUSE in its path; quote `virtiofs/host` and `always/auto` ratios, never
   absolute seconds (note 23 §8.3: the same arm moved 1.25x between sessions).
3. One VM at a time, `cang ps` empty before each arm (`run.sh`'s
   `require_workspace_free` refuses otherwise, `:274-289`), and no co-tenant
   session's builds finishing mid-arm.

### 4.7 What to record

Per run: arm, `bench_ms`, `outer_ms`, `fs_worker_pct` (`summary-*.tsv`), the
opcode histogram file, the arm log, `Compiling`/`Fresh` counts and
`target/debug/deps` entry count (proves the build really rebuilt, note 23 §8.1),
the three env values, and the binary's store path + md5.

### 4.8 What the recipe must avoid (all measured in this map before)

- **Do not trust `cang`'s exit status or stdout.** The CLI can return while the
  guest still works, and the payload's stdout is not forwarded (`run.sh:227-238`,
  ticket 09). Results go to a uniquely-named file under `/workspace` and the
  harness waits for that file, then kills the CLI and any leftover task.
- **Do not skip the linger.** cang tears the guest down when the payload exits and
  can drop the last writes, so `LINGER=10` (`run.sh:47`, `:250-254`).
- **Do not read a stale helper log.** Check its mtime against the run just
  performed (project rule #196).
- **Do not measure on the `read` cell** - it fadvises the page cache away (§4.2).
- **Do not run two VMs on one workspace**, and do not `pkill`/`podman system
  reset` while another session may be running (rules #195; ticket 10's ENOENT
  storm was two VMs on one tree).
- **Do not launch these arms from the session shell.** Every arm runs delegated
  with its own log and an outer timeout (rules #193/#194).
- **Do not quote note 23's 372.8 s / 140.2 s**; they are session-scoped, and the
  `232 s` floor in notes 15/18/20/24 is a *lever-off* number (note 23 §8.7).

## 5. One connection: what one setting covers, and the stale-`/nix` question

- `cache_policy` lives in `passthrough::Config` (`passthrough.rs:399-401`), built
  once per `Fs` device (`devices/src/virtio/fs/device.rs:94-99`) and cloned into
  the worker at activation (`device.rs:287`). A setter mutates the device's config
  before that clone, i.e. before `attach` - the same lifecycle as
  `set_zero_message_open` (`device.rs:188`).
- cang registers **exactly one** virtio-fs device:
  `fs_device_new(CANG_FS_ROOT_TAG, &config.task_rootfs, false)`
  (`launcher.rs:317-321`, tag `/dev/root` = `crates/cang-libkrun/src/lib.rs:25`).
  `/workspace`, `/nix`, `~/.cargo`, `~/.cache/sccache` are all **bind grafts inside
  that one exported tree** (`runtime/vm/prepared_root/mod.rs:112-136`), and
  `cang-guest-init` mounts no virtiofs at all - it only validates that
  `/workspace` exists (`guest_init/runtime/cang.rs:23-31`, `:500-521`) and mounts
  guest tmpfs/disk filesystems (`:558`, `components/disk/*`). So one device, one
  superblock, one `fuse_conn`, one policy for everything: `always` is as
  unscopable as `writeback` (note 13 §3.1), and a `/workspace`-only version needs
  a second device (ticket 16 / note 13 §3.3-3.4).
- It does not endanger the boot: nothing in the mount options or the kernel
  cmdline changes. The device still exports the same tree, `FUSE_ATOMIC_O_TRUNC`
  negotiation and the ZMO `-ENOSYS` answers are untouched (they are separate
  fields/replies), and a policy value that the guest dislikes cannot exist - the
  guest only ever sees reply flag bits it already accepts.
- **Stale `/nix` after a host store rebuild?** Two reasons it is not a new risk:
  (a) with ZMO on the guest *already* keeps `/nix` pages today - the lever's win is
  not confined to `/workspace` (note 23 §8.4), so `always` changes nothing there;
  (b) `/nix/store` paths are content-addressed: a rebuilt store appears under new
  hashes, i.e. new paths, i.e. new inodes. The one host tree that can change in
  place is cang's per-workspace host `/nix` overlay (`upper/`+`work/`+`merged/`
  under the workspace state root, `runtime/session/nix_overlay.rs:19-47`), whose
  merged dir is grafted into the share; a host rewrite under it is visible after
  the attr timeout (5 s) at worst - the same window ZMO already ships, and there is
  no `FUSE_NOTIFY_INVAL_*` path in the fork to close it (note 13 §1.2).

## 6. Recommendation

1. **Do not switch any default.** `always` is a no-op on the shipped path (§1) and
   a worse trade than the shipped lever on the opt-out path (§3). Nothing to ship.
2. **If a policy knob is wanted at all**, ship the env-only variant (§2.4) as a
   measurement/diagnostic switch next to `CANG_FS_STATS`, and leave `--fs
   cache-policy:` until something needs it per launch.
3. **Do run the A1/A2 pair once**, with the `readwarm` cell, because the parent's
   question deserves a measurement rather than a mechanism argument - and the
   instrument (histogram + in-boot shm control) already exists. Expected result:
   identical histograms at 1 M requests and identical build seconds within the
   drift band.
4. **Worth keeping regardless**: the ~5-line `keep_cache` counter in the fork's
   histogram report (§4.5). Every cache-or-flags question in this map has had to
   be answered by inference; one counter retires that.

## Sources

| what | where |
|---|---|
| kernel, the one decision | `fs/fuse/file.c` v7.2 `:139`, `:155-157`, `:165`, `:296-297`; `fs/fuse/dir.c` v7.2 `:889`, `:915-916`, `:1906-1907` (fetched from `git.kernel.org/.../plain/fs/fuse/file.c?h=v7.2`) |
| fork, policy + consumers | `deps/libkrun` @ `d1c58a99` (`v2.0.0-cang.12`): `src/devices/src/virtio/fs/linux/passthrough.rs:335-363` (enum/FromStr), `:399-401` (field), `:466` (default), `:893-909` (open), `:1472-1480` (create), `:1286-1292`/`:1390-1398` (ZMO ENOSYS); `.../fs/device.rs:94-99`, `:168-202`, `:287`; `.../fs/server.rs:1015-1023`, `:1042`; `src/libkrun/src/api/device_builders.rs:634-717`; `src/libkrun/src/api/mod.rs:56-58`; `docs/bindings-generation.md:88` |
| cang chain | `crates/cang/src/cli/mod.rs:279-288`, `:615`, `:658`, `:669-716`, `:2162-2181`; `runtime/launch/plan.rs:46`, `:118-123`, `:145`; `config/model.rs:409`, `:487`; `config/mod.rs:135`; `config/codec.rs:159-164`, `:399`, `:457`, `:501`; `runtime/session/mod.rs:318`; `runtime/vm/libkrun/launcher.rs:76-113`, `:336-345`; `runtime/vm/libkrun/tests.rs:42-82`, `:277-284`, `:676-681`; `crates/cang-libkrun/src/{api.rs:52-53, linked.rs:340-361, lib.rs:25-80}` |
| the harness | `tools/rust-compile-bench/concurrency-scaling.sh:62`, `:173-194`, `:209-520` (probe C; `read` at `:390-405`), `run.sh:47`, `:227-289`, `:345-421`; `mon-threads.sh`, `summarise-threads.sh` |
| priors | notes [13](13-cache-semantics-and-scoping.md), [23](23-zero-message-open-results.md) §1-§8, tickets [16](../tickets/16-workspace-only-opt-in-cache.md), [19](../tickets/19-zero-message-open.md), [20](../tickets/20-zero-message-open-coherence-contract.md) |
| caveat on prior artifacts | note 23's scratch dirs (`/home/dev/cang-bench-zmo`, `/home/dev/cang-bench-vfio`, incl. `correctness.sh`) are **gone** as of this session; re-create the 6-command coherence probe from note 23 §5 rather than depending on them |
