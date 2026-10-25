# Diagnostics and troubleshooting

## Log levels

For host-side and direct-libkrun diagnostics, use `--log-level` with one of
`off`, `error`, `warn`, `info`, `debug`, or `trace`. The same effective level is
used by the parent process, the keep-id libkrun helper, and libkrun logging -
except that libkrun's own logger is never set below `warn`, because its warnings
(a GPU backend that failed to come up, `--zero-copy-shm` falling back to the copy
path, a guest handle about to be mis-routed) describe a degraded launch that
`--log-level off` silences cang's own records for without hiding;
`debug` and `trace` also set `CANG_GUEST_DEBUG=1` so `cang-guest-init` prints
early guest-entry breadcrumbs to stderr. `CANG_LOG_LEVEL` provides the same
setting through the environment. When neither `--log-level` nor
`CANG_LOG_LEVEL` is set, `--debug` remains accepted as a compatibility alias
for `--log-level debug`; otherwise a scalar/global `RUST_LOG` value such as
`debug` or `trace` can enable cang tracing. Target-specific `RUST_LOG` filters
still drive Rust tracing, but are not guessed into a libkrun numeric level.

## File-descriptor pressure

For guest-side file-descriptor pressure, `cang-guest-init fd-report` prints the
worst descriptor consumers in the guest (`pid`, command, count, soft limit, and
an `socket`/`pipe`/`anon_inode`/`regular` target breakdown), the guest-wide
`/proc/sys/fs/file-nr` allocation against `file-max`, and the origin of any
exhaustion it observes. `origin=guest` means a guest-local open failed with
`EMFILE`/`ENFILE`; `origin=host` means a guest-local open succeeded while the
virtiofs probe target `/workspace` failed with `EMFILE`/`ENFILE`, which points at
the host libkrun VM worker that backs every virtiofs mount rather than at the
guest. Pass `--watch` (with `--interval-secs`, default 10) to keep printing a
fresh report. Managed guest sessions sample the same report every 10 seconds
into `/run/cang/fd-pressure.status` and print a warning to `cang-guest-init`
stderr (captured in the task's `helper.stderr.log`) when a process crosses
50/75/90% of its soft `RLIMIT_NOFILE` or grows by 256 descriptors between
samples, so pressure is visible before a command fails with `EMFILE`.

## Startup and lifecycle profiling

For timing diagnostics, `cang --profile` emits `cang host profile` and
`cang-guest-init profile` reports to stderr for completed btrfs-snapshot host
and guest-init phases such as launch-plan build, task rootfs materialization,
persistent disk preparation, guest-init lookup, launch config build, helper
session, task state cleanup, and early guest bootstrap. Btrfs rootfs profile
metadata includes `task_rootfs_cache_status` (`hit`, `miss-populated`,
`miss-rebuilt`, or `direct-uncached`), `task_rootfs_cache_digest_key` when a
known digest keys the cache entry, optional `task_rootfs_cache_path`, and
`task_rootfs_cache_uncached_reason` for direct uncached runs. The
`task_rootfs_materialization` row remains the aggregate rootfs phase; when
profiling is enabled, subordinate rows such as
`task_rootfs_materialization:reset_task_dir`,
`task_rootfs_materialization:buildah_version`,
`task_rootfs_materialization:select_image_attempt`,
`task_rootfs_materialization:resolve_image_digest`,
`task_rootfs_materialization:cache_entry_read`,
`task_rootfs_materialization:cache_snapshot`,
`task_rootfs_materialization:buildah_materializer`, and
`task_rootfs_materialization:cache_population` show the host-side path that ran.
Cache-hit runs usually stop at `cache_snapshot`, direct-uncached runs skip cache
population, and `buildah_materializer` intentionally treats the Buildah
unshare child as a black box. These detail rows are diagnostics for the path
taken and should not be treated as an additive replacement for the aggregate
row. The host report keeps
the aggregate `helper_session` row and, when profiling is enabled, also emits
scoped helper/VM-worker host reports with `profile_scope` metadata for the
helper command build/spawn/wait path, helper setup, passt handoff, VM-worker
fork/wait, prepared-root setup, libkrun open, libkrun pre-enter configuration,
and the blocking libkrun guest session when control returns to Rust. The helper
report also imports VM-worker child phase timings under
`helper_wait_vm_worker_child_*` rows from a pre-handoff artifact written before
the VMM is built. A libkrun built with the fork's profile-path entry point
appends opt-in internal `libkrun_*` TSV rows to that same artifact.
cang prints those rows as a separate `libkrun profile` section with raw
nanosecond (`ns`) values plus a derived millisecond rendering, instead of
merging them into cang's millisecond host profile rows. The libkrun section can
show event-manager creation, context take, firmware/block/kernel-cmdline/net/
vsock/gpu-console/identity setup, and selected microVM build phases such as
payload choice, guest-memory creation, vCPU start, and event-subscriber
registration. `helper_wait_vm_worker_child_unattributed` covers any remaining
wait time outside the known cang-owned child setup phases, usually guest
runtime or libkrun event-loop time after the handoff. `--profile` does not raise
cang, guest-init, or libkrun debug logging;
use `--log-level debug`, `--log-level trace`, or the compatibility form
`--debug` separately when verbose diagnostic logs are needed. Stdout remains
reserved for guest command output.

## Attach-loop latency profiling

For managed PTY attach-loop latency diagnostics, set
`CANG_ATTACH_PROFILE=1` when launching `cang`. This is separate from
`--profile`: it records interactive attach hot-path counters rather than startup
and lifecycle phases. When enabled at launch time, cang propagates the flag to
`cang-guest-init` and both sides emit one `cang attach profile` summary line
to stderr on detach or exit. The host summary includes frame-read, payload size,
stdout batch, stdout write, and stdout flush timings. Host `stdout_batches`,
`stdout_batch_frames_*`, and `stdout_batch_bytes_max` describe how many
immediately available guest data frames were coalesced into each host stdout
write/flush; `stdout_write_count` and `stdout_flush_count` are the resulting
terminal write/flush calls. Compare these host counters with host `frames` and
the guest drain counters to see whether output fragmentation was reduced. The
guest summary includes PTY readable events, PTY read sizes, full-buffer read
count, attached-drain/coalescing counters, terminal normalize/parser time, and
guest frame-write time. Guest summaries keep the compatibility
`normalize_parse_total_us` and `normalize_parse_max_us` fields as combined
terminal-processing timings for each forwarded burst. After attached-drain
coalescing, one forwarded burst can contain multiple PTY reads, so these fields
are no longer necessarily one original PTY read. Split `normalize_*` and
`parser_*` fields use the same forwarded-burst basis for latency analysis.
Nonzero `pty_drain_coalesced_*` counters show that immediately available PTY
reads were combined before forwarding; `WouldBlock` is the expected normal
attached-drain exit, and `pty_drain_bound_hit_count` shows when the conservative
drain caps stopped a burst. Attaching to an already-running managed task
profiles the host attach path immediately, but guest-side attach metrics are
available only if that task was originally launched with `CANG_ATTACH_PROFILE=1`.

## PTY modes and terminal tracing

For live-output compatibility diagnostics, pass `--pty=raw` when launching a
new `cang` task. The default is `--pty=normalize`. This default-off raw mode is
intended for terminal-rendering A/B checks such as comparing a TUI under the
normal managed PTY path versus raw live PTY forwarding. It only changes bytes
sent from the guest PTY to the attached host client: live `Frame::Data` payloads
carry the original PTY bytes, while guest-init still keeps its normalized parser
copy for detach/reattach restore state. It does not change the attach protocol,
stdin forwarding, detached restore frames, or the default behavior. It only
affects newly launched tasks, not `cang attach` to an existing task.

Add the `trace` token, or set boolean-style `CANG_TERMINAL_TRACE=1`, to collect
terminal diagnostics. On the host, trace output writes to
`./cang-terminal.trace` in the current working directory used for the launch.
Inside the guest, guest-init writes the same workspace-mounted file as
`/workspace/cang-terminal.trace`. Custom paths are intentionally ignored so the
host and guest stay on that single shared workspace trace file. A new traced
launch truncates the host workspace trace file before appending fresh events.
When a traced data burst contains alternate-screen enter or exit sequences, the
line also includes bounded hex and escaped-byte context around those hits so the
surrounding terminal output can be inspected without dumping the full PTY burst.
For host-to-guest stdin and guest PTY-input bursts that contain ESC, C0 control,
or DEL bytes, the line also includes bounded `input_contexts=` hex and
escaped-byte context. Terminal tracing is opt-in diagnostic output and can
therefore include small bounded snippets of terminal input/control-byte payloads.
The falsey values `0`, `false`, `no`, `off`, and an empty value disable the
environment opt-in. Raw mode and tracing are independent; when `--pty` contains
only modifier tokens such as `trace`, `no-focus-input`, or
`focus-report-guard`, cang uses the default `normalize` mode. The bounded
focus-report guard is enabled by default and suppresses exact host terminal
focus gained/lost reports (`ESC[I` and `ESC[O`) only during a 750 ms guard after
guest output enables or reasserts focus reporting (`ESC[?1004h`). The guard also
ends early after the first non-focus host input is forwarded. Add
`focus-report-guard` only for explicitness. Add the stronger `no-focus-input`
token to suppress those exact focus reports for the whole initial-launch stdin
path. These input-side diagnostics do not change guest-to-host PTY output,
detached restore frames, or later `cang attach` sessions.

```bash
cang --pty=focus-report-guard
cang --pty=no-focus-input
cang --pty=trace
cang --pty=trace,focus-report-guard
cang --pty=trace,no-focus-input
cang --pty=normalize,trace
cang --pty=raw
cang --pty=raw,trace
cang --pty=normalize,focus-report-guard,trace
cang --pty=normalize,no-focus-input,trace
cang --pty=raw,focus-report-guard,trace
cang --pty=raw,no-focus-input,trace
CANG_TERMINAL_TRACE=1 cang --pty=normalize
```

## PTY benchmark

To collect repeatable PTY benchmark artifacts, run the repo-local benchmark
script. It records synthetic PTY baselines, launches a finite live cang command
with `CANG_ATTACH_PROFILE=1`, parses host attach summaries plus guest summaries
when visible, and writes machine-readable reports under
`.omx/benchmarks/cang-pty/` by default:

```bash
scripts/cang-pty-benchmark.sh --iterations 3
```

Use `--cang <path>` or `CANG_BIN=/path/to/cang` when testing a specific
binary; `--cang-cargo-run` is available as an explicit opt-in for source-tree
runs. Repeat `--cang-arg <arg>` for environment-specific launch flags, for
example `--cang-arg --rootfs-backend --cang-arg btrfs-snapshot`. The
generated `metrics.jsonl` contains per-run records, `summary.json` contains
aggregate synthetic timings plus parsed host/guest profile objects, and
`logs/` preserves raw captured output for failed live runs; the live PTY path
records the combined PTY stream in stdout and may leave stderr empty.
`--skip-live` is only for local synthetic smoke checks; PTY optimization
evidence should use the live run so a missing host profile fails visibly. The
default live run is the `live-cang-shell` smoke/profile scenario. To add
interactive live PTY samples, pass `--live-iterations <n>` and optionally
`--live-warmup <n>`; this adds `live-cang-redraw-typing` records where the host
drives stdin marker lines through the PTY while the guest emits redraw bursts
and distinct output markers. These samples use one persistent live cang session
by default, so larger runs measure the interactive PTY hot path without
repeating VM/libkrun/guest startup for every sample. Each persistent record is
tagged with `measurement_model: "persistent-session"`,
`persistent_session: true`, a shared `persistent_session_id`, the child process
pid, sample ordinal/count, and `session_lifecycle_elapsed_us`. Hot-window
elapsed time, per-marker latency stats, read-gap stats, and bytes-drained
evidence remain under `profile`, with aggregate values and `measurement_models`
under `summary.json`'s `scenario_profiles.live-cang-redraw-typing`. Pass
`--live-per-sample-vm` to request the legacy VM/process-per-sample model; those
records are tagged `measurement_model: "per-sample-vm"` and are useful for
startup+lifecycle diagnostics rather than persistent-session hot-path
comparison. For a higher-sample comparison, prefer n=100, for example:

```bash
scripts/cang-pty-benchmark.sh \
  --cang .omx/builds/cang-main/bin/cang \
  --guest-init .omx/builds/cang-main/bin/cang-guest-init \
  --iterations 3 \
  --warmup 1 \
  --live-iterations 100 \
  --live-warmup 5 \
  --timeout 240
```

The benchmark uses
`--mem 2` for the live run by default to avoid measuring huge-memory VM boot
delay instead of PTY latency; pass `--no-default-live-mem` to test cang's
default memory behavior, or repeat `--cang-arg --mem --cang-arg <GiB>` to
choose another size. The live run uses a btrfs-backed state directory under
`/home/dev/.local/share/containers` when available, even if the parent shell has
a non-btrfs `XDG_STATE_HOME`; override that with `--state-home <path>`. When
`result/bin/cang-guest-init` exists, the runner also passes it as the live
guest init by default so the host and guest benchmark artifacts match; use
`--guest-init <path>` or `--no-default-guest-init` to override that behavior.
The optimized live benchmark requires the host attach profile; the guest profile
is recorded when the guest/libkrun console is visible. For a strict guest-profile
diagnostic run, add `--cang-arg --log-level --cang-arg debug` and
`--require-guest-profile`, but do not treat that debug-logging run as the clean
performance baseline. Optional `--rmux` records a non-nested rmux attach-drain
comparison when an executable rmux is available: the runner creates an isolated
detached rmux session, attaches through a child PTY, drains a finite redraw
workload, and adds `optional-rmux` elapsed stats plus
`profiles.rmux_attach_drain` read-gap/byte metrics to `summary.json`. The rmux
comparison is still optional and threshold-free; absent or unusable rmux records
a structured skip/failure without editing `/mnt/rmux`. The `--tmux` hook still
records a structured skip until an isolated finite tmux comparison is added.

## rust build benchmark over `/workspace`

`tools/rust-compile-bench/` measures what the mounted workspace costs a rustc
build, in three arms: `host` (no VM), `guest-virtiofs` (built in the mounted
`/workspace`) and `guest-local` (the same tree in the guest's `/dev/shm`, so no
FUSE is involved). `guest-virtiofs / guest-local` is the virtio-fs penalty;
`host / guest-local` is the VM penalty.

```bash
nix build .#cang
tools/rust-compile-bench/run.sh --runs 3
```

It writes `logs/<arm>-<n>.log` and `summary-synthetic.tsv` under `$BENCH_ROOT`
(default `/home/dev/cang-bench`), and its README carries the recorded numbers.
Two cang behaviours to know about when reading the harness: a guest command's
stdout is not forwarded and its exit status is not propagated (`cang -- false`
exits 0), and a launch into a workspace that already has a running task does
nothing at all - so the arms report through files in the shared workspace, and
the harness refuses to start while a task is running there.

## Inspecting a preserved `launch.conf`

To inspect a preserved task `launch.conf`, decode its internal hex line format:

```bash
cang decode-launch-conf <task-state-dir>/launch.conf
```

The decoder prints `KEY=decoded-value` lines with control characters escaped for
readability. It is a debugging aid for files preserved through `--preserve-debug`;
the launch path still consumes the encoded private handoff format.

## Troubleshooting FAQ

- When a task ends under memory pressure, the guest kernel's own account of the
  kill is kept in `guest-kernel-console.log` in the task state directory, and
  cang reports it when the task ends, for example:

  ```text
  cang: guest kernel OOM-killed python3.13 (pid 705), anon-rss 3895792 kB
  ```

  A managed task keeps its supervisor out of the guest OOM killer's reach
  (`oom_score_adj` of -1000) while the shell and its children stay killable, so
  a single runaway process is killed instead of ending the whole microVM. The
  guest console also captures a kernel panic, which cang reports as
  `guest kernel found no killable task and panicked` when the OOM killer had no
  victim left. Without the console capture a guest death under memory pressure
  is indistinguishable from a task that finished normally.

- If the interactive shell appears to hang during startup, check the host
  `RLIMIT_NOFILE` limits inherited by the process that launched cang:

  ```bash
  ulimit -Sn
  ulimit -Hn
  ```

  Cang raises the helper's soft `nofile` limit to the inherited hard limit
  before starting libkrun, then asks libkrun to set the guest VM's
  `RLIMIT_NOFILE` soft and hard limits to that same inherited hard limit. It
  cannot raise above the parent launcher's hard limit. If `ulimit -Hn` is low,
  raise the hard limit in the actual parent launcher context first, such as the
  shell, tmux session, systemd unit, or service that starts `cang`, then start
  cang again from that context. Cang treats guest nofile setup as required:
  startup fails if the loaded libkrun does not provide `krun_set_rlimits` or
  rejects the nofile limit request.

- A pi extension that ships a prebuilt native addon can fail to load in the
  guest with

  ```text
  libstdc++.so.6: cannot open shared object file: No such file or directory
  ```

  The addon is a glibc object opening a C++ runtime that neither Pi's bundled Bun
  runtime nor the guest's library search path provides: Bun has no `RUNPATH` and
  no `libstdc++` in its `DT_NEEDED`, the guest's `/lib` and `/usr/lib` are
  compatibility farms that are not on the loader's default path and carry no
  `libstdc++`, and the default `mimalloc` preload does not link it either.
  Current images serve it through the `pi` wrapper (see the container environment
  summary). On an image built before that wrapper, `cang --alloc=hardened ...`
  also unblocks the load, because that allocator library happens to carry
  `libstdc++` in its own `DT_NEEDED`; that is incidental rather than a fix -
  `--alloc=glibc` removes the same preload file and fails the same way. Note that
  probing with `node` hides the problem: node links `libstdc++` itself and loads
  the addon even when the guest cannot, so use `bun` (or `pi`) to reproduce it.

- A C++ binary the agent runs fails to start once `pi` has set its environment:

  ```text
  /nix/store/...-zvec-grep-0.3.0/bin/zg: /nix/store/...-cang-native-addon-runtime/lib/libstdc++.so.6: version `GLIBCXX_3.4.35' not found (required by /nix/store/...-onnxruntime-1.27.1/lib/libonnxruntime.so.1)
  ```

  In `fish` the same failure reads `'zg --version' terminated by signal SIGSEGV
  (Address boundary error)`: the fault is in the loader, not in the program. The
  wrapper's directory holds a single `libstdc++.so.6` (next to the ONNX Runtime
  ABI shim, which nothing else asks for), and because
  `LD_LIBRARY_PATH` is searched ahead of a binary's own `DT_RUNPATH`, that one
  file is the C++ runtime **ceiling** for every process `pi` launches. A package
  built against a newer libstdc++ than the directory exposes therefore cannot
  start from `pi` (or from any process `pi` started), while the same binary runs
  from a plain shell or under `env -u LD_LIBRARY_PATH zg ...`. Current images
  fill the directory from the newest C++ runtime the image needs
  (nixpkgs-unstable's gcc, because `zg`'s ONNX Runtime is built against it); if
  the guest's `...-cang-native-addon-runtime/lib/libstdc++.so.6` still resolves
  into a `gcc-15.3.0-lib`, the image predates that and its ceiling is too low.

- An extension's `onnxruntime-node` binding fails to load, or `zg` stops
  starting once it does:

  ```text
  version `VERS_1.24.3' not found (required by .../onnxruntime_binding.node)
  version `VERS_1.27.1' not found (required by /nix/store/...-zvec-grep-.../bin/zg)
  ```

  Both consumers NEED a library named `libonnxruntime.so.1` and import
  `OrtGetApiBase` under different version nodes (`@VERS_1.24.3` for a vendored
  binding whose own `$ORIGIN` holds no library, `@VERS_1.27.1` for `zg`), and no
  nixpkgs ONNX Runtime exports both - each release exports its own node
  (`@@VERS_1.24.4`, `@@VERS_1.27.1`), and the loader checks the node before ONNX
  Runtime's own append-only `OrtApi` negotiation can run. Current images answer
  both with one ABI shim, installed in the pi wrapper's addon-runtime directory as
  `libonnxruntime.so.1` and `libonnxrtabi24.so.1` (see ADR 0009); an image whose
  runtime directory holds only `libstdc++.so.6` predates it. The shim loads the
  real library from `ONNXRUNTIME_LIB_PATH` (the image exports it; the wrapper
  defaults it), so a wrong value there fails with the shim's own message on
  stderr:

  ```text
  onnxruntime ABI shim (libonnxruntime.so.1): cannot load the real ONNX Runtime from '<path>': ...
  ```

  The path has to name a *directory* containing `libonnxruntime.so.1`
  (`ORT_SHIM_REAL_LIB` is the exact-file form, for tests). Two further traps are
  worth knowing. A node the shim does not define cannot be served at all - if a
  vendored binding is rebuilt against a newer ONNX Runtime, its node has to be
  added to `versionScript` in `nix/pkgs/onnxruntime-shim.nix`. And because the
  shim's directory precedes every binary's own run path, it is the ONNX Runtime of
  everything `pi` launches, including `zg` as a child - which is exactly why it
  answers `zg`'s node too, and why its forwarded library has to be at least as new
  as the newest node it advertises. A *host* can still fail where a guest does
  not: on 2026-10-16 the unstable 1.27.1 ONNX Runtime died in this host's shell
  with ``libm.so.6: version `GLIBC_2.43' not found`` - its
  `/etc/ld-nix.so.preload` malloc provider carries the host's older glibc into the
  process - and with `librt.so.1: undefined symbol: __pointer_chk_guard, version
  GLIBC_PRIVATE` under a 26.05-built consumer, while the same probes loaded it and
  answered `GetApi(27)` in a cang guest. Masking the preload for one command
  (`unshare -r -m sh -c 'mount --bind /dev/null /etc/ld-nix.so.preload; exec
  <probe>'`) is the quickest way to tell the two causes apart.


- A tool dies by signal at startup in a guest but runs on the host, as `zg` did:

  ```text
  'zg --version' terminated by signal SIGSEGV (Address boundary error)
  ```

  The guest preloads `pkgs.mimalloc` into every nix-linked process - that is the
  default allocator - and ld.so initialises a preload's constructor *after* the
  binary's dependency initialisers, so whatever those do first runs on an
  allocator that has not been through its own initialisation yet. `zg` dies in
  that window: libstdc++'s static initialiser runs there, and under the glibc the
  unstable toolchain brings (2.44, newer than the 2.42 the preloaded object was
  built against) mimalloc 3.3.2 faults in it, so `zg` is killed before `main`. The
  same nixpkgs' 3.5.3 does not fault, with nothing else changed. `free(NULL)`
  alone is not enough to trigger it, which is why the culprit looked like the
  locale path. Only the guest shows it, because the development host preloads
  `hardened_malloc` instead. Confirm it by
  masking the preload for one command (`unshare -r -m sh -c 'mount --bind
  /dev/null /etc/ld-nix.so.preload; exec zg --version'`, which prints the
  version) or by booting `--alloc=glibc`; leaving the allocator out of the
  picture entirely is what `--alloc=glibc` does. Images whose
  `/nix/store/*-mimalloc-3.3.2` is the preload predate the fix; current ones
  preload mimalloc 3.5.x.

## Host GPU runtime (`cang doctor`)

cang's GPU path needs two host libraries that are not stock upstream: the
**virglrenderer** it links (`libvirglrenderer.so.1`) and execs as
`libexec/cang-helpers/virgl_render_server`, and the **mesa** `libgallium` the
render server loads for GL/Vulkan and whose VA driver the host-side encode path
calls. `cang doctor` reports whether the ones this cang uses are cang's patched
builds:

```
$ cang doctor
cang doctor: host GPU runtime

PASS     virglrenderer  /nix/store/…-virglrenderer-1.3.0/lib/libvirglrenderer.so.1.11.0
         4 patch markers present
         render server: /nix/store/…-cang-0.12.0/libexec/cang-helpers/virgl_render_server
         markers: encode raw headers: %u, render encode raw header failed, …
PASS     mesa           /nix/store/…-mesa-26.1.8/lib/libgallium-26.1.8.so
         VA driver directory: /run/opengl-driver/lib/dri
         1 patch marker present
         markers: fill_enc_raw_headers
PASS     mesa (render server)  /nix/store/…-mesa-26.1.8/lib/libgallium-26.1.8.so
         CANG_MESA_LIBDIR=/nix/store/…-mesa-26.1.8/lib
         1 patch marker present

Host GPU runtime is cang's patched build.
```

A stock library fails and says what breaks:

```
FAIL     mesa           /nix/store/…-mesa-26.1.8/lib/libgallium-26.1.8.so
         VA driver directory: /nix/store/…-mesa-26.1.8/lib/dri (cang's VA driver override: CANG_VA_DRIVER_PATH)
         no cang patch marker found: this is a stock mesa, so the packed-header reader can loop
         for ever (a constant-QP VA-API encode hangs) and the encoder attributes a B-frame GOP
         needs are not forwarded
         patches not in this build: mesa-virgl-encode-raw-headers.patch
```

It resolves the virglrenderer from `/proc/self/maps` (the library this process
actually loaded - ground truth, not a path cang hopes the loader used), requires
the render server to come from the same build, and for mesa follows libva's own
search order for a VA driver: cang's interposer override directory first
(`CANG_VA_DRIVER_PATH`, or the package's `<prefix>/lib/cang/dri`), then
`LIBVA_DRIVERS_PATH`, then the NixOS opengl-driver farm and the system
directories. The file it lands on is the `libgallium` libva would open - the one
whose `vl_rbsp_ue` decides whether a constant-QP host encode hangs - and the
report names both the file and the rule that selected it. Each build is then
identified by a
**marker**: a literal that only cang's patch puts into the library
(`nix/pkgs/patches/virglrenderer-*.patch`, `mesa-*.patch`). No nix evaluation is
involved, so the check works for any cang package, prebuilt or not. The
marker-to-patch table is asserted against the patch files in the crate's tests, so
renaming a marker in a patch fails the build rather than silently reporting a
patched library as stock.

The third check covers the mesa the external `virgl_render_server` loads for GL and
Vulkan, named by `CANG_MESA_LIBDIR`. `nix/lib/render-server-env.nix` takes cang's
mesa (`pkgs.mesaVaApi`) for it - not nixpkgs' plain drop - so a virtio-gpu host
gets the DMA-BUF modifier fix in the render server's own stack as well as in the
VA driver the second check resolves. The image's guest-side mesa is a different
thing and stays the plain drop; the image's wrapper contracts assert the two
differ, so a stock mesa cannot quietly take over the VA path.

A stock library is a **FAIL** (exit 1) and says what goes wrong with it - a stock
vrend decodes a guest's video into the wrong planes and drops its reference
pictures; a stock mesa can loop for ever on a truncated packed header (the
constant-QP encode hang of `docs/wayfinder/guest-vaapi-video/tickets/08-*`) and
does not forward the encoder attributes a B-frame GOP needs. On a NixOS host the
fix is to apply the flake's `overlays.default` to the system mesa (that is what
puts the patched `libgallium` behind `/run/opengl-driver/lib/dri`). A host with no
VA driver anywhere is a **WARN** (exit 0): it is reported, but a host that never
starts a `--gpu=drm` guest has no VA path to break.

- The guest has two descriptor ceilings, and cang keeps them consistent: the
  per-process `RLIMIT_NOFILE` and the guest-kernel-wide
  `/proc/sys/fs/file-max`. The guest kernel derives `file-max` from guest RAM at
  boot, which at small `--mem` values (for example `--mem 4`) lands below the
  guest `RLIMIT_NOFILE` hard limit. Guest bootstrap raises `fs.file-max` to at
  least that hard limit so a process cannot fail with system-wide `ENFILE`
  before it reaches its own limit. A kernel value already above the hard limit
  is left alone, and `cang --mem <GiB>` still raises the kernel-derived
  default. `cang-guest-init fd-report` prints both ceilings
  (`process.N.soft_limit`, `process.N.hard_limit`, and `system_fds_max`).
