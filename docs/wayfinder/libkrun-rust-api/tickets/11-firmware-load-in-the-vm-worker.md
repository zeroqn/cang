---
label: wayfinder:task
title: Make the firmware load reachable in the VM worker
status: closed
blocked_by: ["05-create-cang-libkrun-crate"]
claimed_by: pi session (2026-09-28)
---

## Question

Surfaced by ticket 09's live boot, not by charting: with the Rust-API binding the
first boot fails with

```
cang sandboxed VM worker: libkrun setup failed: krun_payload_load_krunfw: file not found
[ERROR krun::api::payload] could not load libkrunfw.so.5
```

while the same image and flags boot fine with the previous (dlopen-based) cang.
`Payload::load_krunfw` opens the firmware by bare soname, so the question is what
made that lookup work before and not now - and what the replacement lookup should
be.

## Resolution

**The old binding found the firmware by absolute path; the new one relied on
`DT_RUNPATH`, which the VM worker does not get to use. cang now preloads the
firmware by absolute path, and the live boot passes.**

What was measured, in order:

- `--seccomp=off --landlock=off` (the smoke's flags) fails deterministically;
  `--seccomp=audit-default:<trace>`, which runs the worker *under strace*,
  succeeds. Same binary, same image, same state shape.
- The firmware file is present and loadable: `libkrunfw.so.5.3.0` (23 MB, only
  `NEEDED libc.so.6`) exists in the store, and `ctypes.CDLL("libkrunfw.so.5")`
  loads it both plain and inside a `unshare --user --mount` namespace.
- `LD_LIBRARY_PATH=<cang>/lib/cang` made no difference, and the worker produced
  none of `LD_DEBUG=libs`' `trying file=` output - the signature of glibc's
  **secure-execution mode**, which drops both `LD_LIBRARY_PATH` and the `$ORIGIN`
  token in `DT_RUNPATH`. That fits cang's shape: the worker is exec'd through
  `unshare --user --mount --setuid 0 --keep-caps` and then changes its filesystem
  uid (`configure_vm_worker_filesystem_identity`), so the kernel marks the exec
  `AT_SECURE`. The old binding was immune because its candidates were *absolute*
  paths (`<exe-root>/lib/cang/libkrun.so.2`), and the firmware came in as
  `libkrun.so`'s `DT_NEEDED`, resolved from libkrun.so's own runpath - a
  dlopen'd object's runpath is not the setuid program's runpath.
  (Strace's own child keeps the firmware reachable because strace, not cang, is
  what gets exec'd with the mismatched uid - consistent with the audit run
  working.)

The fix (`crates/cang-libkrun/src/firmware.rs`): before `Payload::load_krunfw`,
`dlopen` the package-relative firmware by absolute path with
`RTLD_NOW|RTLD_GLOBAL`. Loading it publishes the SONAME in the global scope, so
libkrun's later bare-soname lookup finds the loaded object instead of searching.
`CANG_LIBKRUNFW_LIBRARY` overrides the path for tree builds (the counterpart of
the removed `CANG_LIBKRUN_LIBRARY`), and with no package-relative firmware the
soname lookup is all there is - the preload is best-effort and libkrun still
reports the miss itself.

Verified: `cargo test --workspace` (including four new `candidates()` tests),
`cargo clippy --all-targets --all-features -- -D warnings`, `nix build .#cang`,
and a live boot - `echo live-boot-ok; uname -r; nproc` printed
`6.12.109-hardened1` and `26` inside the guest, exit 0, on
`--mem 4 --alloc hardened --seccomp=off --landlock=off`.

Note for whoever touches the packaging next: this is why the cang package puts
the firmware in `<prefix>/lib/cang` and why the binary's rpath matters less than
the preload does. A future `libkrun` that accepts an explicit firmware path (or
an rpath-immune firmware lookup) would make the preload unnecessary.
