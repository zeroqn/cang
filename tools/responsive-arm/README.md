# responsive-arm

The arm kit for measuring a cang guest's *responsiveness under pressure*: whether
a memory-heavy build drives the guest to its zram wall, who the kernel kills when
it gets there, and what the interactive paths (a keystroke round trip, a command
start) cost while it happens.

Nothing here decides anything. It launches one guest, samples it, and leaves raw
files behind; the note that cites the files owns the verdict.

It exists because four separate arms each rebuilt the same instrument by hand
(`notes/01`, `04`, `09` and `12` of
`docs/wayfinder/guest-responsiveness/`), and the second one to build it hit bugs
its predecessor had already fixed - `printf '--'` under dash, and per-field zram
sysfs attributes being root-only.

The `printf` trap is worth spelling out because it bit this kit too: writing a
sample marker as `printf '--- SAMPLE %s\n'` fails under `dash` **and** under an
image whose `/bin/sh` is bash in POSIX mode (`printf: --: invalid option`), and the
result is a capture with no timestamps at all - 210 blocks that can only be
anchored by the payload's own markers. The marker is written as
`printf '%s\n' "--- SAMPLE ..."` here, and note 13 records the arm that found the
old form still in this file.

## What it measures

`sample-guest.sh` writes one block per tick: the timestamp marker, verbatim
`/proc/meminfo`, `/proc/vmstat`, `/proc/pressure/memory`, the zram device's
`mm_stat` / `disksize` / `comp_algorithm` / `stat` / `io_stat`, the swap line, and
a census of every process as `proc <pid> <comm> <oom_score_adj> <size> <resident>`
pages. From those five reads you can reconstruct the whole ladder:

| what you want | where it is |
|---|---|
| the effective ceiling | `MemTotal` vs `AnonPages` vs `SwapFree`, and `mm_stat` fields 1-3 (`orig_data_size`, `compr_data_size`, `mem_used_total`) |
| the observed compression ratio `c` | `orig_data_size / compr_data_size` |
| whether the budget binds | `mem_used_total` == its 4th field (`mem_limit`) |
| whether the device refused stores | `io_stat`'s `failed_writes` |
| how close kswapd is to thrash | `/proc/pressure/memory` `full avg10`, plus `pgscan_*`/`pswpout` deltas |
| who the kernel can kill | each process's `oom_score_adj` (`-1000` is out of reach) |
| who it did kill | the console copy (below), since `kernel.dmesg_restrict=1` hides `dmesg` from the workload |

## The two traps this kit carries over

1. **A zero-filled presser measures nothing.** `zram_write_page()` stores a page
   whose every byte is identical as one flag plus a fill value, allocating nothing
   (`page_same_filled`, `linux-7.3.5`'s `drivers/block/zram/zram_drv.c`), so
   `vec![0u8; n]` can grow to 8 GiB inside a 4 GiB guest without reaching the wall.
   `presser.rs` gives each page its own xorshift32 stream so they are distinct and
   effectively incompressible - the worst case for the budget, which is what a wall
   test needs.
2. **The console is not in the guest.** `guest-kernel-console.log` was 0 bytes in
   every arm that tried it; the kernel's lines arrive in the task dir's
   `helper.stderr.log` through libkrun's passthrough, and cang deletes the task dir
   on exit - so the driver's 1 s copier is the only capture. Quote
   `host/vmm/*.helper.stderr.log`.

## Usage

Put the guest-side scripts in the arm's own directory under `out/`:

```sh
ARM=arm1 MODE=session PAYLOAD=storm.sh MEM=4 \
  WORKLOAD_SRC=.scratch/ladder01/workload \
  ./tools/responsive-arm/run-arm.sh
```

`PAYLOAD` is a guest script under `/workspace` (the arm's directory is the
workspace, so `/workspace/storm.sh` is `<dir>/storm.sh`) that starts
`sh /workspace/sample-guest.sh /workspace/out/samples.txt 0.5`, does its work,
and writes a `done` sentinel to `/workspace/out/done.txt`. A minimal session
payload:

```sh
#!/bin/sh
set -u
mkdir -p /workspace/out
sh /workspace/sample-guest.sh /workspace/out/samples.txt 0.5 &
rustc -O /workspace/presser.rs -o /workspace/out/presser
CHUNK_MB=32 MAX_MB=8192 PROGRESS=/workspace/out/press-progress.txt \
  /workspace/out/presser
printf 'done\n' > /workspace/out/done.txt
```

`MODE=exec` instead boots a managed session with an idle payload and sends the
script through `cang exec`, which is how you measure a workload that is not the
session's own child (and how `notes/12` proved the OOM-score inheritance fix).

## The rules the driver enforces

- **One arm per boot, one at a time.** The gate refuses to start while any foreign
  `libkrun-vm-worker-enter` runs, and refuses if this workspace already has a
  running task. A co-tenant invalidates every timing number - the arm that ignored
  this had to discard two boots.
- **An explicit `--mem`.** A guest defaults to 80% of host RAM; two of those do
  not fit on one host, and a host whose only swap is zram gains no capacity from
  it.
- **Fresh `--out-dir` per arm.** A rootless podman btrfs graphroot cannot be
  removed with `rm -rf`; reclaim it with `podman unshare rm -rf <out-dir>`, and a
  half-deleted one makes the next `podman load` die with "payload does not match
  any of the supported image formats".
- **Nothing is salvaged by hand afterwards.** Every claim in a note points at a
  file the driver copied: `host/driver.log` (gates and phase boundaries),
  `host/cang-<mode>.log`, `host/cang-exec.log`, `host/vmm/*.helper.stderr.log`
  (the console), and the arm's own `out/`.
