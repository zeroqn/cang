---
label: wayfinder:note
ticket: 03-overcommit-mode-verdict
---

# `vm.overcommit_memory=2`: the limit counts zram as capacity that is not there

Authority: kernel source extracted from `/nix/store/1pmrvbg56rly33v67d14sghsxcdzp2pw-linux-7.3.5.tar.xz`
into `.scratch/kernel-7.3.5/`; every kernel `path:line` below is relative to that extraction (note 02
cites the same byte-identical tree as `linux-7.3.5/...`). The submodule's `tarballs/linux-7.3.1.tar.xz`
was not used. Repo cites are from this checkout; no VM was booted.

`M` = guest `MemTotal` = `--mem` (default 80 % of host RAM, `crates/cang/src/runtime/launch/config/components/resources.rs:134-138`); `r` = `vm.overcommit_ratio`; `c`
= the compression ratio zram actually achieves. `.../components/` abbreviates `crates/cang-guest-init/src/guest_init/components/`.

## 1. The accounting, exactly

`/proc/meminfo`'s `CommitLimit` is `vm_commit_limit()` (`fs/proc/meminfo.c:125`):

```c
mm/util.c:924   allowed = ((totalram_pages() - hugetlb_total_pages())
mm/util.c:925              * sysctl_overcommit_ratio / 100);  /* else overcommit_kbytes >> 10 */
mm/util.c:926   allowed += total_swap_pages;
```

The sysctl documentation states the same two halves: `Documentation/admin-guide/sysctl/vm.rst:826-827`
(kbytes: "swap plus this amount of physical RAM") and `:862-864` (ratio: "swap plus this percentage of
physical RAM"); `Documentation/mm/overcommit-accounting.rst:20-25` is the same formula in prose. Defaults:
ratio 50 (`mm/util.c:795`), kbytes 0 (`:796`); writing one zeroes the other (`:809-810`, `:861-862`). So
`CommitLimit(r) = M*r/100 + SwapTotal`.

**In this guest `SwapTotal` is `M`, backed by 25 % of the same `M`.** The device capacity is 100 % of
`MemTotal` (`.../components/swap/zram.rs:62`, arithmetic `:302-304`, write `:268-270`), `mem_limit` is 25
% of it (`:71`, `:308-310`, write `:271-275`), then `mkswap`/`swapon` (`:276-281`); `SwapTotal` is exactly
that device's page count (`mm/swapfile.c:2988-2991`). `vm_commit_limit()` subtracts only hugetlb
(`mm/util.c:924`), never the device's `mem_limit`.

| `r` | CommitLimit | @12.7 GiB | @8 GiB |
|---|---|---|---|
| 25 | 1.25 M | 15.9 GiB | 10.0 GiB |
| 50 (default) | 1.50 M | 19.1 GiB | 12.0 GiB |
| 75 | 1.75 M | 22.2 GiB | 14.0 GiB |
| 100 | 2.00 M | 25.4 GiB | 16.0 GiB |

**Does it count RAM twice? Yes, by construction.** The second term is `M` of *uncompressed* swap capacity
whose store is 0.25 `M` of the *same* `M`. At `r=50` the limit is 1.5 `M`; 0.5 `M` of that is RAM already
in `totalram_pages()`, and 0.25 `M` of it is undeliverable even for perfectly compressible pages, because
zram refuses further pages at `mem_limit` (`.../components/swap/zram.rs:63-71`).

The physical ceiling is `C ≈ M + 0.25 M*(c-1)` (at most 0.25 `M` of compressed data representing `0.25
M*c` anonymous bytes), so the one ratio that "means something" is `r ≈ 25*(c-1)`: c=1 → 25, c=2 → 50, c=3
→ 75. `r=50` is thus not a fact about this guest but a silent assumption of 2:1 compression; `c` is
unknown at boot and differs per workload and phase.

**The limit moves under the session:** before the swap worker's `swapon`, `total_swap_pages` is 0 and
`CommitLimit` is only `M*r/100` = 0.5 `M`; that worker is detached and `--after-exec`-gated
(`.../components/swap/zram.rs:116-140`), activating ~1 s after the shell starts (`docs/images-and-storage.md` "Guest
memory and zram swap": 0.6-1.3 s of the 1.8 s enter path).

## 2. Which failures move

The charge sites are mapping sites only: anonymous private-writable `mmap` (`mm/vma.c:2608`, predicate
`:2487-2501`), `brk` (`:3061`), VMA grow (`:3263-3265`), `mprotect` (`mm/mprotect.c:815`), `mremap` grow
(`mm/mremap.c:988`), shmem objects (`mm/shmem.c:180,194,213`). **No charge in the fault path and none in
`fork`**: no `vm_enough_memory` call appears in `kernel/fork.c` or `mm/memory.c`. That is all of "refuses
commits, not touches".

Mode 0, today's setting, enforces only a one-request obvious-overcommit check (`mm/util.c:985-987`: `pages
> totalram_pages() + total_swap_pages`). Mode 2 compares the cumulative counter (`:991` `allowed =
vm_commit_limit()`), minus 8 MiB without `CAP_SYS_ADMIN` (`:995-996`, default `:799`) and minus
`min(total_vm/32, 128 MiB)` (`:1001-1005`, default `:798`), refusing when the request - already added at
`:977` - no longer fits (`:1007-1008`); the refusal unaccounts (`:1013`) and `pr_warn`s process, pid and
bytes (`:1010-1015`).

- **Refused early (ENOMEM at reservation time):** anonymous private-writable mappings and widenings -
  `malloc`'s mmap path, `brk`, `mprotect`, `mremap`, thread-stack growth, shmem objects.
- **Exempt:** shared mappings (`mm/vma.c:2498-2500`), hugetlb (`:2495-2496`), memfd objects
  (`mm/memfd.c:465,470`). `MAP_NORESERVE` is **not** a refuge: honoured only when the policy is not NEVER
  (`mm/mmap.c:563-566`, except hugetlb `:568-570`), matching
  `Documentation/mm/overcommit-accounting.rst:48`.
- **Still OOM-killed:** everything already committed and later touched. The fault path's allocator ends
  in the global killer (`mm/page_alloc.c:4127-4128`); `pagefault_out_of_memory()`
  (`mm/oom_kill.c:1174-1192`) is memcg-only, and this guest has no cgroup mount
  (`.../components/podman/config.rs:56`). Mode 2 adds an earlier failure mode; it does not remove the
  killer.

A refused `mmap` returns NULL and Rust's allocation-error handler aborts (`SIGABRT`) instead of returning
an error - **labelled: standard Rust behaviour, not measured in this image**. So one crate dies with an
attributed message in its own log, against the killer choosing by score: supervisor at `oom_score_adj =
-1000` (`crates/cang-guest-init/src/guest_init/process.rs:349,363`), workload child and the detached
`internal nix prep` worker at 0 (`:352,375`).

## 3. What would break in this guest

- **H1, swap-activation self-lock (derived, not measured).** With mode 2 set at boot, `CommitLimit` is
  0.5 `M` until `swapon` succeeds; `mkswap` and `swapon` allocate and are charged. A refusal during
  activation leaves the limit at 0.5 `M` for the guest's whole life, below the image's own steady state.
  From the ordering (`.../components/swap/zram.rs:116-140`, `:276-281`), not a run.
- **H2, nix daemon and build users.** 32 build users at gid 30000 (`nix/image/layers.nix:27-39`);
  `/etc/nix/nix.conf` (`nix/image/nix-config.nix:22-28`) sets no `max-jobs` and no `cores`, so `nix build`
  runs up to `nproc` concurrent builders. `fork` does not charge (§2), so the daemon's forks are free; the
  hazard is their aggregate reservation - **unverified, needs the live arm in ticket 03.**
- **H3, the allocator preload.** `mimalloc` is the default for Nix-linked binaries
  (`nix/image/container.nix:118`, `README.md:201-216`) while `rustc` runs under `bwrap` with an empty
  preload (`nix/image/layers.nix:79-99`), i.e. glibc `malloc`. Whether mimalloc's arena reservation uses
  `MAP_NORESERVE` (free before, charged now) is **unverified - needs the live arm.**
- **H4, rootless podman/buildah.** `cgroups = "disabled"`, btrfs storage, crun
  (`.../components/podman/config.rs:43,56,66-68`), rootless mapping via `newuidmap`/`newgidmap`
  (`.../components/rootless/idmap.rs:21-22`). Go binaries and building are heavy mappers/forkers;
  **reservations unverified - needs the live arm**, and "cannot `podman load` at all" is the ticket's own
  worse outcome.
- **H5, `/tmp` tmpfs, and what is safe.** Guest init mounts tmpfs on `/tmp` without `size=`
  (`crates/cang-guest-init/src/guest_init/runtime/cang.rs:700-712`): ceiling half of RAM, drawn "from the
  same pool" (`Documentation/mm/overcommit-accounting.rst:67`). Zram's `disksize` write is kernel
  metadata rather than a charge, and the balloon only reports pages the guest already freed
  (`crates/cang/src/runtime/vm/libkrun/launcher.rs:915-928`), so neither is a hazard beyond H1.

## 4. The guest's current values

Compiled-in: `vm.overcommit_memory = 0` (`OVERCOMMIT_GUESS`, `mm/util.c:794`,
`include/uapi/linux/mman.h:13`), ratio 50 (`mm/util.c:795`), kbytes 0 (`:796`), `user_reserve_kbytes` 128
MiB (`:798`), `admin_reserve_kbytes` 8 MiB (`:799`). Nothing in the image changes them: the only
`/proc/sys/vm` write under `crates/` or `nix/` is `drop_caches`
(`crates/cang-guest-init/src/guest_init/components/memory.rs:30,55`), the other sysctl writers being
`fs/file-max`, user-namespace and hardening knobs (`components/rootless/kernel.rs:34-36`,
`components/hardening/{dmesg,perf,io_uring}.rs`), and no cmdline fragment sets `sysctl.*`. **The actual
numbers come from `/proc/sys/vm/overcommit_{memory,ratio,kbytes}` and `/proc/meminfo` in the live arm** -
nothing offline can confirm them.

## 5. Verdict

**Refuse it as a guest default: at `r=50` mode 2 bounds something real only after someone measures the
workload's compression ratio, and this guest cannot know it.** Confidence **high** on the accounting
(formula-level, code and docs agreeing), **medium** on §3, where three of five hazards are explicitly
unverified.

1. The second term is `M` of zram capacity whose store is 25 % of the same `M`; no `overcommit_ratio`
  fixes that, because the ceiling is set by compressibility - only `r ≈ 25*(c-1)` matches it.
2. Mode 2 ignores `MAP_NORESERVE` (`mm/mmap.c:563-566`), so every "just in case" reservation becomes a
  hard commitment: the class of program `Documentation/admin-guide/sysctl/vm.rst:849-851` names.
3. Touch-time exhaustion still goes through the global killer, and note 02's MGLRU `min_ttl_ms` (a
  decisive kill with the supervisor already at `-1000`) serves the map's "resilience first" clause better
  than a reservation-time veto.

Smallest settling arm (one boot; the ratio sweep is a write to `/proc/sys/vm/overcommit_ratio`, so no
extra boots):

- **A (control, mode 0)** and **B (mode 2, `r=50`)**: ticket 01/02's rustc storm at the same `--mem`,
  recording `/proc/meminfo` `CommitLimit`/`Committed_AS`/`SwapTotal`, `/sys/block/zram0/mm_stat`
  (`orig_data_size` vs `compr_data_size` gives `c`), `dmesg` for `not enough memory for the allocation`
  and OOM victims, whether `swapon` succeeded (`/run/cang/swap.status`), and whether `nix build` and
  `podman load` start (H2/H4).
- **C (ratio sweep inside B)**: `r` = 25/50/75/100 plus `25*(c-1)` from B's own `mm_stat`, one storm
  each; the limit "means something" only where refusals stop arriving before the killer does.
- Kill the lever if `swapon` fails under mode 2 (H1), a `nix build`/`podman` refusal arrives (H2/H4), or
  refusals arrive while `Committed_AS - C` is still large.

Measuring `c` is what ticket 01 needs anyway: Arms A/B are shared work, not a cost of this question.
