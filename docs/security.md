# Security

Host-side sandboxing (Landlock, seccomp) and guest capability grants.

## Landlock

- Host-side cang Landlock is applied to the libkrun VM-worker process after
  prepared-root and libkrun setup that require broader host access, but before
  the VMM build. It is applied before seccomp so the Landlock syscalls are
  not blocked by the seccomp filter.
- For ordinary task launches, omitting `--landlock` is equivalent to
  `--landlock=relax`. Relax mode is fail-closed for the non-network Landlock
  feature families cang handles, including filesystem access rules, device
  ioctl access handling, IPC scopes for abstract UNIX sockets and signals, and
  audit-flag support. It intentionally does not handle TCP `BindTcp`, so
  guest-local listeners such as websocket or dev-server ports can bind inside
  the guest without disabling the rest of cang's host-side Landlock layer.
- `--landlock=all` preserves the stricter TCP bind behavior: cang additionally
  handles TCP `BindTcp` and constrains it to simple published TCP host ports when
  they are known.
- `--landlock=best-effort` uses the `relax` policy shape, including unrestricted
  TCP `BindTcp`, but applies only the supported subset and logs the effective
  policy plus any non-fully-enforced status. This is the explicit compatibility
  path for older kernels or hosts with partial Landlock support.
- `--landlock=off` disables only this host-side Landlock layer. It does not
  disable the default host-side seccomp policy; use `--seccomp=off` separately
  if you need to debug seccomp.
- The first cut confines the VM worker and its future children only. It does not
  claim to confine the guest kernel, guest Podman, the keep-id helper before the
  VM worker, or network manager/pasta/passt processes started before the VM
  worker.
- Filesystem rules are derived from the launch config: the prepared root is
  read/execute only, declared read-write bind mounts and disks are writable,
  declared read-only bind mounts remain read-only, and host `/nix` overlay paths
  are categorized by lower/upper/work/merged role. If a broader writable parent
  rule is required for profiling output, the effective-policy report labels
  affected read-only children as mount-enforced instead of Landlock-enforced.
- TCP `ConnectTcp` is intentionally unrestricted by this first cut to preserve
  existing guest/network behavior. Landlock's connect rules are per remote TCP
  port, and cang does not yet have an outbound allowlist. TCP `BindTcp` is
  unrestricted in `relax` and `best-effort`; it is handled and constrained to
  simple published TCP host ports only in `all`.
- Guest-local binds do not expose host ports by themselves. Host inbound
  exposure remains controlled by repeatable `-p, --publish SPEC`; without a
  publish rule, a process may bind inside the guest VM but incoming host
  connections are not forwarded to it.
- Before restriction, cang inventories retained file descriptors. Fail-closed
  modes (`relax` and `all`) fail on unexpected retained regular files because
  descriptors opened before Landlock can retain access outside the filesystem
  rules.
- The effective-policy report is emitted in debug logs and includes mode, path
  categories/access classes, whether BindTcp is unrestricted or restricted to
  published ports, the explicit `ConnectTcp` unrestricted-by-design marker, IPC
  scopes, audit flags, and retained-FD classifications.

## Seccomp

- Host-side cang seccomp is incubating. For ordinary task launches, omitting
  `--seccomp` makes cang enforce the packaged default policy at
  `$out/share/cang/seccomp/default.json`. This is fail-closed: if the packaged
  policy is missing, unreadable, invalid, or cannot be compiled for the host
  architecture, the launch fails before the VM worker enters libkrun.
- `--seccomp=off` is the explicit no-filter spelling and opt-out for a normal
  task launch. Maintenance/internal one-shot VMs such as
  `cang container-store resize/reset` remain default-off for this milestone.
- `--seccomp=audit:<trace>` (also accepted as `--seccomp=trace:<trace>`) runs
  the libkrun VM-worker entrypoint under `strace -f`, writes a tracer-owned raw
  log, and converts it to the requested JSONL trace when the helper observes
  the VM worker exit. The raw `.strace` sidecar can include VM-worker setup
  and cleanup syscalls; the finalized JSONL starts after the internal start
  marker emitted immediately before the VMM is built and then keeps only
  syscall lines from the traced PID that emitted that marker plus post-marker
  descendants linked by observed `clone3`, `clone`, `fork`, or `vfork` returns.
  This excludes unrelated parent cleanup syscalls such as
  post-VM unmounts from policy synthesis input while preserving the raw sidecar
  for diagnostics. Missing the start marker or its traced PID fails trace
  finalization instead of publishing an unscoped JSONL trace. The keep-id helper
  setup, including `newuidmap` and `newgidmap`, is not traced. Use the raw
  `.strace` sidecar only for debugging.
- `cang seccomp synthesize --input <trace> --output <policy>` extracts syscall
  names from the trace and writes a deterministic `seccompiler` JSON policy with
  a `main_thread` allowlist.
- `--seccomp=audit:<policy>:<denied-trace>` (also accepted as
  `--seccomp=trace:<policy>:<denied-trace>`) is a policy-aware gap audit. It
  still runs without installing a seccomp filter, but asks `strace` to record
  only syscall names that are not already listed in
  `<policy>`'s `main_thread.filter[*].syscall` allowlist. The raw gap sidecar
  still keeps the audit marker and `clone3`/`clone`/`fork`/`vfork` lines visible
  so finalization can reconstruct the VM-worker lineage even when those syscalls
  are already allowed. The resulting `<denied-trace>` JSONL uses the same
  lineage-scoped trace record shape as full audit, but remains missing-only by
  suppressing baseline-allowed lineage bookkeeping records during finalization.
  "Denied" here means "observed by strace but missing from the baseline policy";
  it does not mean a kernel seccomp denial occurred.
- `--seccomp=audit-default:<denied-trace>` (also accepted as
  `--seccomp=trace-default:<denied-trace>`) is the same gap audit against the
  packaged default policy at `$out/share/cang/seccomp/default.json`, without
  spelling that policy path. This is also fail-closed: if the packaged default
  policy is unavailable or invalid, cang fails before launching the traced VM
  worker instead of falling back to full audit.
- `cang seccomp extend --policy <baseline> --trace <denied-trace> --output
  <updated-policy>` additively appends missing syscall allow rules from a full
  or gap audit trace to an existing policy. Use `--default-policy` instead of
  `--policy <baseline>` to extend from the packaged default policy without
  spelling its path; exactly one of `--policy` or `--default-policy` is required.
  It preserves existing filter entries and appends new syscall-only entries in
  deterministic syscall-name order. The output is validated with `seccompiler`
  before cang writes it; the baseline policy file is not modified.
- `--seccomp=enforce:<policy>` loads that `seccompiler` JSON policy and
  installs it in the VM worker immediately before the VMM is built. Passing an
  explicit enforce path overrides the packaged default policy for that run.
- Gap audit is a debugging aid, not proof that enforcement is safe. It compares
  syscall names only; it does not diff or prove seccompiler argument-condition
  rules. Always test the updated policy explicitly with
  `--seccomp=enforce:<policy>`.
- This is cang host-helper filtering only. It does not change guest Podman's
  seccomp profile.
- On NixOS hosts where audit mode fails with ptrace errors such as
  `PTRACE_TRACEME: Operation not permitted`, first check:

  ```bash
  sysctl kernel.yama.ptrace_scope
  ```

  `kernel.yama.ptrace_scope=1` normally allows tracing a direct child, which is
  the audit-mode workflow. Only hosts that disable ptrace more broadly should
  need a temporary host-policy change such as:

  ```bash
  sudo sysctl kernel.yama.ptrace_scope=0
  ```

  Persisting any ptrace relaxation is a host policy decision, commonly
  represented with `boot.kernel.sysctl."kernel.yama.ptrace_scope"` in NixOS
  configuration.

## Packaged nested-container seccomp policy

The image includes the pinned `containers/container-libs` seccomp policy package
and writes global `/etc/containers/containers.conf` with:

```toml
[containers]
seccomp_profile = "/nix/store/...-container-lib-policy-seccomp-json-.../share/containers/seccomp.json"
```

This makes inner Podman use the packaged policy by default while still allowing
per-user containers config to override it. To refresh the policy, update the
`containerLibPolicySeccompJson` revision/hash in `nix/pins.nix`, then rebuild
`.#container-lib-policy-seccomp-json` and `.#container`.

## Guest permissions

```bash
./result/bin/cang --new-perms=io-uring
./result/bin/cang --new-perms=perf
./result/bin/cang --new-perms=io-uring,net-admin,net-raw,bpf,perf,sys-admin
```

- `--new-perms` grants the comma-separated additional permissions `io-uring`, `net-admin`,
  `net-raw`, `bpf`, `perf`, and `sys-admin`. Values are order-independent and duplicates are
  ignored.
  The former `--permissions`, `--io-uring`, and `--perf` flags have been removed.
- No optional permission is enabled by default. Normal initial commands, managed PTY
  commands, hidden `as-dev` commands, and later `cang exec` commands run without
  effective, permitted, inheritable, or ambient capabilities. Cang retains only the
  required rootless-ID-map and authorized grant capabilities in the guest bounding set.
- Without `io-uring`, cang disables creation of new io_uring instances
  guest-wide by setting `kernel.io_uring_disabled=2` during root guest
  initialization. This happens before Nix and Podman preparation, Wayland
  startup, managed-session startup, or the task command. Guest initialization
  fails closed if the sysctl cannot be applied.
- `io-uring` allows processes in the dynamic guest `dev` group to create
  io_uring instances without `CAP_SYS_ADMIN`. Guest-init writes the `dev` GID to
  `kernel.io_uring_group` and keeps `kernel.io_uring_disabled=1`; processes
  outside that group remain denied unless permitted by the kernel's
  `CAP_SYS_ADMIN` exception. `io-uring` itself does not grant `CAP_SYS_ADMIN`;
  an explicit `sys-admin` grant independently satisfies that exception for a
  command launched through `cang-granted`.
- `net-admin`, `net-raw`, `bpf`, and `sys-admin` authorize `CAP_NET_ADMIN`,
  `CAP_NET_RAW`, `CAP_BPF`, and `CAP_SYS_ADMIN`, respectively, for the explicit
  `cang-granted COMMAND [ARG ...]` helper. `CAP_SYS_ADMIN` is exceptionally broad;
  it remains absent from normal guest commands and is granted only to commands launched
  through this helper. Every helper invocation receives all capability-bearing permissions
  authorized for the task; the helper refuses to run when none were authorized. For example:

  ```bash
  ./result/bin/cang --new-perms=sys-admin -- cang-granted fish
  ```

  The helper is installed root-owned with the exact authorized file capabilities under
  the read-only `/run/cang/wrappers` tree. It does not read grants from its arguments,
  environment, or a policy file. A capability-bearing subtree still needs to drop its
  capabilities before invoking programs such as Bubblewrap that reject unexpected
  permitted capabilities.
- The cang guest image includes `perf` and `strace` on `PATH`. Without `perf`,
  cang leaves the guest kernel's hardened `kernel.perf_event_paranoid=3`
  setting unchanged. `perf` sets `kernel.perf_event_paranoid=-1` and
  `kernel.kptr_restrict=0` before the task starts, enabling unprivileged kernel
  software events, tracepoints, and nonzero `/proc/kallsyms` addresses while
  weakening guest performance-event and kernel-pointer isolation.
- Hardware PMU events such as cycles and instructions are not guaranteed. The
  current x86 libkrun CPUID configuration disables the architectural PMU, so
  software events and available tracepoints are the supported profiling scope.
- These permissions affect only processes inside the guest VM. They do not
  alter cang's host VM-worker capabilities, host seccomp, host Landlock, or
  host networking.
- Nested Podman capability and seccomp policy remains independent. In
  particular, the packaged nested-container profile blocks io_uring syscalls,
  and selected guest capabilities are not automatically granted inside nested
  containers.
