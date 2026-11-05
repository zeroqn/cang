# Note 52: a 23-crate workspace reproduction, the per-write getxattr, and a seccomp gap

2026-11-05. Follows note 51 (`51-cold-build-request-mix-and-job-count.md`).
Source: the reporting session's source tarball of a 23-crate proprietary Rust
workspace (1.05k dependency crates), unpacked on a btrfs volume with 100 GiB free
and mounted as the workspace. Guest: `7.2.10-hardened1`, 26 vCPU, 14 GiB.

## 1. The reporting session's 609 s reproduces as a request-bound build

`cargo test --profile fast --workspace --no-run`, `RUSTFLAGS='-Clinker=clang
-Clink-arg=--ld-path=wild'`, one boot per arm, `CANG_FS_STATS=1`:

| arm | wall | target/ | FUSE requests | requests/s |
| --- | --- | --- | --- | --- |
| `CARGO_BUILD_JOBS=6`, cold sccache | 464 s | 11 GiB, 8 462 files | 1 688 991 | 3 640 |
| same, `JOBS=6`, warm sccache | 284 s | - | 1 579 921 | 5 563 |

Opcode mix of the 464 s arm (1 688 991 requests): WRITE 30.1% (508 056), READ
22.3% (376 165), **GETXATTR 17.2% (291 184)**, LOOKUP 12.3% (207 917), GETATTR
5.6%, CREATE 3.8%, FORGET 3.5%, UNLINK 3.3%; lookups 0% negative, OPEN 3,
FLUSH 2 — i.e. all four shipped levers are live in that guest.

At the request ceiling measured in note 44 (7-8 k requests/s with 4 in flight),
1.69 M requests are 210-240 s of the 464 s, so the share owns roughly half of a
cold build of this tree on this host - a smaller share than the 2401-crate tree
of notes 23-45, because 1 053 registry crates come from `$HOME/.cargo` (also the
share) but their sources are read once.

## 2. `HANDLE_KILLPRIV_V2` is already acked, and acking it changes nothing

The 291 184 GETXATTR are not issued by userspace (`strace -f -e trace=getxattr`
over a crate build: 0 calls). They are the guest kernel's own
`file_remove_privs()` -> `cap_inode_need_killpriv()` asking
`getxattr("security.capability")` before a write, which costs one round trip
because `fs/fuse/inode.c:fuse_change_attributes_common()` clears the inode's
`S_NOSEC` on every attribute refresh.

Single-stream cost on the share against tmpfs, same guest, same file:

| call | share | tmpfs |
| --- | --- | --- |
| `getxattr security.capability` | 199.4 us | 2.0 us |
| `getxattr user.mime_type` | 146.0 us | 1.9 us |
| `stat` | 1.7 us | 1.6 us |

The intended lever was to acknowledge `FUSE_HANDLE_KILLPRIV_V2` so the client
sets `SB_NOSEC` and stops asking. Measured, it is a no-op: `server.rs` already
puts `HANDLE_KILLPRIV_V2` in the `supported` mask it answers every INIT with, so
every launch already has it. Interleaved A/B on one binary gated by
`CANG_FS_KILLPRIV_V2` (2 rounds, `JOBS=6`, warm sccache): 284 s / 289 s / 272 s,
and the histograms are identical to four digits (GETXATTR 291 450 / 291 347 /
291 346, totals 1.58 M). The knob and the plumbing were removed rather than
shipped as a switch that does nothing.

What the ack *does* oblige is the server side of the contract, and that part was
missing: with V2 acked the client stops clearing `security.capability` itself and
says so on the request (`FUSE_WRITE_KILL_SUIDGID`, `FUSE_OPEN_KILL_SUIDGID`,
`FATTR_KILL_SUIDGID`), and nothing in the passthrough acted on those flags - a
guest truncate or chown left a file's capabilities in place. Fixed in the fork
(`drop_file_privileges`, called from `open`/`create`/`setattr`), with a unit test
that asserts the set-id bits survive a request without `KILL_SUIDGID` and are
cleared with one.

Open hypothesis for the 291 k: attribute refreshes are what clear `S_NOSEC`
(95 k GETATTR over 8.4 k files), so every write that follows a refresh pays the
probe. A smaller refresh rate (`attr_timeout` above 5 s) or a kernel-side change
would be the lever; neither is free, so this is recorded, not claimed.

## 3. A guest xattr write used to kill the VM silently

Found while testing the above. On a stock cang 0.17.3 binary, in a guest:

```sh
python3 -c 'import os; p="/workspace/.x"; open(p,"w").write("x"); os.setxattr(p,"user.k",b"v")'
```

the guest produced no output, `cang` exited **0**, and the run left a task
registered as running. Cause: `crates/cang/assets/seccomp/default.json` is the
default enforcement policy (`mismatch_action: trap`), and it allowed `fsetxattr`
but not `fremovexattr`, `lremovexattr`, `lsetxattr` or `fchmod` - so the fs
server's own `SETXATTR`/`REMOVEXATTR` handlers (and the `fchmod` branch of
`SETATTR` when a handle exists, i.e. `--fs=zero-message-open:off`) killed the VM
worker with SIGSYS. Two fixes shipped: those four syscalls added to the policy,
and the repro above now completes (`SETXATTR ok`, `REMOVEXATTR ok`, exit 0),
verified against the same binary pair (stock dies, fixed completes).

The silent `exit 0` is ticket 09's territory (`09-cang-exit-status-lies.md`) and
is not addressed here.

## 4. Job count is not the pathology (already in note 51)

Interleaved 2x2 on this tree: `JOBS=6` share 58.1 s vs tmpfs 25.9 s (2.24x),
`JOBS=26` 49.2 s vs 17.3 s (2.85x) - the share's absolute cost is job-invariant
(32.2 vs 31.9 s), which is what makes the *ratio* worse with more jobs.
