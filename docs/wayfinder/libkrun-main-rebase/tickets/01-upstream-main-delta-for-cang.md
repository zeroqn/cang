---
label: wayfinder:research
title: Upstream-main delta for cang's libkrun integration
status: open
blocked_by: []
claimed_by: unclaimed
---

## Question

Rebasing `cang` from the stable-1.19.x tip (`fb988873`, v1.19.5) onto upstream
`main` (`a980e779`, `FULL_VERSION=2.0.0`) crosses ~369 commits. What does that
change *on the surface cang actually touches*?

Enumerate, with file:line evidence from the rebased tree:

1. **The C ABI cang binds.** cang loads libkrun dynamically and resolves the
   symbols declared in `crates/cang/src/runtime/vm/libkrun/api.rs` /
   `dynamic.rs`: `krun_create_ctx`, `krun_free_ctx`, `krun_init_log`,
   `krun_set_log_level`, `krun_set_vm_config`, `krun_set_gpu_options3`,
   `krun_check_nested_virt`, `krun_set_nested_virt`, `krun_set_root`,
   `krun_add_disk`, `krun_disable_implicit_console`, `krun_set_console_output`,
   `krun_add_virtio_console_default`, `krun_add_net_unixstream`,
   `krun_add_vsock_port2`, `krun_set_port_map`, `krun_set_workdir`,
   `krun_set_exec`, `krun_set_rlimits`, `krun_set_profile_path`,
   `krun_set_kernel_cmdline_append`, `krun_start_enter`. Which of these changed
   signature, semantics, or disappeared between 1.19.5 and main? Which new
   `krun_*` entry points exist that cang might now want (not to wire - just to
   record)?
2. **Soname / install layout.** cang's loader probes `libkrun.so.1` then
   `libkrun.so` (`DEFAULT_LIBKRUN_NAMES`). `main`'s Makefile declares
   `FULL_VERSION=2.0.0`; what soname, symlinks and `LIBDIR_Linux` does main
   actually install, and what does the fork's existing asset (`libkrun-<arch>-linux-full.tgz`)
   end up containing? Does `nix/pkgs/libkrun.nix` still find what it expects?
3. **Build shape.** Which cargo features does the fork's CI and the in-tree
   `nix/dev` build enable (`gpu`, `init-blob`, `net`, `blk`, `input`, `virgl_resource_map2`,
   `krun_display`), and did main add/rename/remove any of them?
4. **init/kernel protocol.** What does main expect of its kernel side
   (`libkrun_init.so`, kernel cmdline, virtio feature advertisement) compared
   with 1.19.5 - i.e. the input to ticket 05?

## Deliverable

`notes/01-upstream-main-delta.md`: the delta as a table (symbol -> unchanged /
changed / gone / new, with the main-side definition), the soname/asset/layout
facts, the feature-flag delta, and a concrete list of cang-side edits the pin
will need. Raw command output kept in the same directory.
