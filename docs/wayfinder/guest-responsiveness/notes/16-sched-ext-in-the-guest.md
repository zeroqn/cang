# sched_ext in the guest: what it actually took

Bob re-opened [the 2026-11-25 out-of-scope decision](../MAP.md) on 2026-11-26 and
asked for the opposite: land the firmware change and start `scx_bpfland` by
default, with `--scx=off` as the opt-out. This note is the experimental record
behind that ticket: what each symbol buys, what each one costs, and the boot
evidence. It supersedes the price recorded in the map's "Out of scope" entry,
which was derived from the configs and stopped at "a firmware rebuild".

## The chain, one boot per failure

Each boot below was a local `libkrunfw` build (plain `-kvm` config, gcc, host
28 vCPU) loaded through `CANG_LIBKRUNFW_LIBRARY`, with the guest's `--root`
session running the checks. The scheduler binaries came from a copied
`scx_full-1.1.4` closure mounted at `/scx` (the image ships none yet).

| boot | what failed | why | symbol added |
|---|---|---|---|
| 1 | `program of this type cannot use helper bpf_probe_read_str#45` | libbpf's `FEAT_PROBE_READ_KERN` probe loads a `BPF_PROG_TYPE_TRACEPOINT` program; `include/linux/bpf_types.h:38-45` registers that type only under `CONFIG_BPF_EVENTS`, which `kernel/trace/Kconfig:865-869` gates on `FTRACE`. Probe fails, so libbpf rewrites `probe_read_kernel_str`(115) to helper 45 in `bpf_object__sanitize_prog`, and a sched_ext `struct_ops` program (`.get_func_proto = bpf_base_func_proto`, `kernel/sched/ext/ext.c:8214`) has no 45 | `CONFIG_FTRACE=y` (+ `KPROBES`) |
| 2 | `Error: kernel is missing CONFIG_FUNCTION_TRACER` | `scx_utils::compat::function_tracer_available()` tests `/proc/sys/kernel/ftrace_enabled`, which only exists with `FUNCTION_TRACER` | `CONFIG_FUNCTION_TRACER=y` (`FUNCTION_GRAPH_TRACER` left off) |
| 3 | `libbpf: failed to open system Kconfig` then skeleton load `-EINVAL` | the object declares kcfg externs; libbpf reads them from the kernel config | `CONFIG_IKCONFIG=y` + `IKCONFIG_PROC=y` (22,900-byte `/proc/config.gz`) |
| 4 | `ldimm64 failed to find the address for kernel symbol 'sd_llc_id'`, `-ENOENT` | `sd_llc_id`/`sd_llc_size` are `DEFINE_PER_CPU` **data** symbols (`kernel/sched/topology.c:676-677`); libbpf resolved them to BTF vars but the verifier needs a kallsyms address | `CONFIG_KALLSYMS_ALL=y` |

Before any of that, the config needed the sched_ext set itself:
`CONFIG_SCHED_CLASS_EXT=y` (pulls `EXT_GROUP_SCHED`/`EXT_SUB_SCHED`),
`CONFIG_BPF_JIT=y` + `BPF_JIT_ALWAYS_ON`/`BPF_JIT_DEFAULT_ON` (all three were off
or absent), and `CONFIG_DEBUG_INFO_BTF=y` over `DEBUG_INFO_NONE` (which needs
pahole at build time). The derived delta against the pinned plain config is 87
lines; the four x86_64 configs carry it in the fork commit `59d9f4a` on
`scx-experiment`.

## Boot evidence (final firmware, `CONFIG_DEBUG_FS` still off)

`uname -r` `7.3.5-hardened1`, `/sys/kernel/btf/vmlinux` 5,610,168 bytes,
`/sys/kernel/sched_ext` present, `/proc/kallsyms` 164,711 lines:

| scheduler | result |
|---|---|
| `scx_simple` | `state=enabled`, `ops=simple`, `SCX_EV_REFILL_SLICE_DFL 1175` |
| `scx_bpfland` | `state=enabled`, `ops=bpfland_1.1.4_x86_64_unknown_linux_gnu`, `primary CPU domain = 0x3ffffff`, `ext.enabled: 1` |
| `scx_chaos` | `state=enabled`, `ops=chaos_1.1.4_...` |
| `scx_pair` | `state=enabled`, `ops=pair` |
| `scx_central` | loads, aborts itself: `EXIT: scx_bpf_error (scx_central.bpf.c:312: init from non-central CPU)` |
| `scx_qmap` | verifier refuses `qmap_init`: `invalid read from stack R1 off=0 size=8`, `-EACCES` |
| `scx_flatcg` | `Failed to attach struct_ops` `-EINVAL` |
| `scx_lavd` | `panicked ... No debugfs mount found`; refused by bob's rule ("if a scheduler depends on debugfs, we don't support it") |

Every enable left the guest healthy and unloaded cleanly (`state=disabled`,
`enable_seq` kept), which is the kernel's own revert path
(`Documentation/scheduler/sched-ext.rst:9-13,60-64`).

## tracefs comes with it (accepted 2026-11-26)

`FTRACE`, `KPROBE_EVENTS` and `FUNCTION_TRACER` all reach `CONFIG_TRACING`
(`kernel/trace/Kconfig:798` `select TRACING`, `:239` `select GENERIC_TRACER`
-> `:192`), and `CONFIG_TRACING` compiles `tracefs` in (`fs/Makefile:118`).
The pinned kernel had none of them, so bob's "don't enable tracefs" rule
collided with the scheduler head-on: without `BPF_EVENTS` no stock scheduler
loads at all (the first arm's `scx_simple` died on helper #45). Asked and decided:
**tracefs is accepted** rather than losing the scheduler. Nothing mounts it,
`CONFIG_DEBUG_FS` stays off, so `mount -t debugfs` still fails and a
debugfs-dependent scheduler (`scx_lavd`) remains unsupported.

## What it costs

- Firmware: 24,643,416 -> 36,636,456 bytes (+48.7 %). First DWARF build ~35 min
  on 28 vCPU, build tree 4.3 GB; a config flip afterwards ~3-6 min.
- Image: `pkgs.scx.full` closure 286 MB / 28 store paths before dedup against
  what the image already ships.
- `KALLSYMS_ALL` exposes every data symbol name in `/proc/kallsyms`; `IKCONFIG_PROC`
  hands any guest process the whole kernel config; `FUNCTION_TRACER` turns the
  ftrace surface on. Accepted by bob on 2026-11-26 for the shipping kernel and
  recorded in `docs/security.md`.
- Boot path: the start is detached and unwaited, so the enter path pays a fork
  plus an exec, not a BPF load. `scx_bpfland` itself needed ~2 s from spawn to
  `state=enabled` in these arms.

## Not measured

No latency or throughput comparison. The map's own re-open condition was that
tickets 04/09 show the residual latency is scheduler-shaped, and both closed
without that; this ticket ships the scheduler on bob's instruction, not on a
measured win. The `nice -20` interaction (note 36) and the `SCX_OPS_SWITCH_PARTIAL`
question are also unmeasured: `scx_bpfland` switches globally by default, so it
now decides the priority of every task in the guest.
