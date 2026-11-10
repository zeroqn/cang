---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Prototype: a packed (erofs) `/nix` lower

**Verdict. It works, it packs 1.8-2.4x, and it loses on read wall time — and the
firmware experiment in the second half settles it: with `ZIP_ZSTD` and
`PCPU_KTHREAD` rebuilt in, the zstd image mounts and the per-cpu workers start,
and packed erofs is *still* 1.7-2.2x slower cold than the btrfs lower on the same
manifest** (zstd 50-69 s against 25-36 s; 1.4 GB of device bytes against 2.6 GB,
i.e. fewer bytes and more time).

## What was built (host, `mkfs.erofs` 1.9.1, from the image's own store)

| image | bytes | vs 7.69 GiB logical | mounts on the shipped kernel |
|---|---|---|---|
| lz4, 4 KiB pcluster | 4 568 891 392 | 1.80x | yes |
| lz4, 64 KiB chunks (`-C 65536`) | 4 201 680 896 | 1.96x | yes |
| lz4, 256 KiB chunks (`-C 262144`) | 4 180 512 768 | 1.97x | yes |
| zstd (`-zzstd,15`) | 3 215 405 056 | 2.55x | **no** |
| uncompressed | 8 267 096 064 | 0.93x | yes |
| *the btrfs `.raw` it replaces* | 12 395 216 896 apparent / 7.90 GiB used | — | yes |

`CONFIG_EROFS_FS=y`, `XATTR/POSIX_ACL/SECURITY=y`, `ZIP=y` — but
**`ZIP_ZSTD`, `ZIP_LZMA`, `ZIP_DEFLATE`, `PCPU_KTHREAD` are all "not set"** in
`deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto`. So the shipped kernel reads
LZ4 erofs only, single-threaded in the submitting reader.

## What it costs to read (4 boots, same guest, same 20-file / 2.56 GB manifest)

Same boot, same device (`vdb`), same request shape (~2 MiB per request, ~850
requests) — this is the apples-to-apples half:

| arm (boot 4) | ms | device GB | device MB/s | guest CPU s |
|---|---|---|---|---|
| uncompressed erofs | 54.8 | 2.68 | **49** | 7.3 |
| lz4 4 KiB, `cache_strategy=readaround` | 84.6 | 1.79 | 21 | 19.0 |
| lz4 4 KiB, `readahead` | 87.5 | 1.79 | 20 | 17.5 |
| lz4 64 KiB chunks | 66.2 | 1.70 | 26 | 13.8 |
| lz4 64 KiB chunks, 4 parallel readers | 60.7 | 1.70 | 28 | 34.3 |
| 20 plain files on the same device (control) | 48.3 | 2.35 | 49 | 5.4 |

Compression buys 0.9-1.0 GB fewer device bytes and costs 45-100 % more wall time;
per-byte device throughput drops from 49 to 20-28 MB/s. The uncompressed erofs
image matches the plain-file control exactly (49 MB/s), so **packing, loop and
erofs themselves are free — the whole cost is decompression**: 17-19 s of guest
CPU for 1.8 GB, serialized in one reader (4 readers only lift the rate to 28 MB/s
while burning 34 s of CPU). That is precisely the shape of `PCPU_KTHREAD=n` with
4 KiB pclusters.

Against the thing it would replace, in the same campaign, block lower on a
dedicated device: **58.6 s / 2.58 GB / 44 MB/s**. Best packed arm, cold-ish
(64 KiB): **66.2 s**. So today: 13 % slower, not faster.

Also measured, for the record: no page losses anywhere — 0 `csum failed`, 0
`I/O error` in all four boots and every arm, and 685 store entries visible through
the mounted image with `user_xattr,acl` in force (the overlay lowerdir contract is
unchanged).

## The firmware experiment: the prediction was tested, and it failed

The two symbols were rebuilt into the fork's firmware (below), the same three arms
were re-run against it, and **the prediction (1.5-2.5x win from device-byte
savings) did not hold**: decompression, not device bytes, is the limit, and the
per-cpu workers do not remove it for a workload that reads a handful of large
files.

### What was built

`deps/libkrunfw` at HEAD, `MakefileLto`, `KERNEL_SOURCES=linux-7.3.1-kvm-lto`,
`KERNEL_CONFIG=config-libkrunfw_x86_64-kvm-lto`, with exactly two config lines
flipped in the work copy:

```
CONFIG_EROFS_FS_ZIP_ZSTD=y        (was "# ... is not set")
CONFIG_EROFS_FS_PCPU_KTHREAD=y    (was "# ... is not set")
```

Result: `libkrunfw.so.5.6.2`, 24 511 528 bytes,
`sha256 bed03226ec3134b7419346aee63426e5716d0834a71671926330a6f24b3a0990`,
embedded `Linux version 7.3.1-hardened1` (the shipped pin is
`7.3.1-hardened1` too — same version, different config, so the version string
cannot be the discriminator). Build traps hit, for the record: the nix-wrapped
`clang` in the flake's devShell injects `-nostdlibinc`, which the kernel's
`scripts/mod/empty.o` rejects under `-Werror`, so `LLVM=1` must resolve an
**unwrapped** clang on `PATH` (`llvmPackages.clang-unwrapped`); the kernel wants
the `lz4` host tool for its own image compression; and the final `-shared` link
needs `-lgcc`/`-lgcc_s`, i.e. `LIBRARY_PATH` must carry
`<gcc>/lib/gcc/x86_64-unknown-linux-gnu/15.2.0` and `gcc-15.2.0-lib/lib`. With
`MAKEFLAGS=-j26` the whole thing builds in ~10 minutes, not the ~105 minutes the
CI job takes.

### The arms discriminate, so the substitution is proved

Control boot (packaged firmware, `CANG_LIBKRUNFW_LIBRARY` unset) vs substituted
boot, same guest, same images:

| boot | zstd erofs | lz4 64 KiB erofs | kernel log |
|---|---|---|---|
| packaged `v5.6.2-cang.5` | **mount fails**, `fsconfig() failed: Operation not supported`, kernel: `erofs (device loop0): algorithm 3 isn't enabled on this kernel` | mounts | — |
| rebuilt (`bed03226…`) | **mounts** | mounts | `erofs (device loop0): initialized per-cpu workers successfully.` |

### The numbers, on the rebuilt firmware (two boots, 17 arms)

`--mem 4`, guest 26 vcpus, the same 20-file / 2.56 GB manifest in every arm,
`drop_caches` before each arm:

| arm | ms | device GB | device MB/s | guest CPU s |
|---|---|---|---|---|
| btrfs lower, sequential | 36.4 / 29.1 / 29.8 / 25.0 | 2.57-2.61 | 72-103 | 3.0-5.8 |
| btrfs lower, 4 readers | 31.0 | 2.57 | 83 | 18.8 |
| **zstd erofs, sequential** | 70.5 / 65.2 / 69.0 / 60.2 | 1.39-1.41 | 20-23 | 26.5-27.8 |
| **zstd erofs, 4 readers** | 54.4 / 50.1 | 1.40 | 26-28 | 54.2 |
| lz4 64 KiB, sequential | 64.3 / 55.8 / 61.7 | 1.69-1.70 | 26-28 | 20.0 |
| lz4 64 KiB, 4 readers | 60.0 / 42.3 | 1.69 | 28-40 | 38.9-55.5 |

Read that as: **46 % fewer device bytes, 1.7-2.2x more wall time.** The device is
asked for ~750 (zstd) or ~820 (lz4) requests averaging ~1.8-2.0 MiB and answers at
20-28 MB/s, while the btrfs lower's ~1000 requests average 2.2-2.5 MiB and answer
at 72-103 MB/s. Parallel readers lift the erofs arms 15-30 % and cost 2-3x the
CPU, which is the shape of a decompression-bound path, not an I/O-bound one: zstd
decompresses this store at roughly **95 MB/s per core** (2.5 GB in 27 s of CPU),
below the device rate the btrfs lower already achieves on this host.

So the mechanism is now measured rather than assumed: **a packed lower trades
device bandwidth (which this host has) for CPU-side decompression (which this
workload does not need to spend)**, and no erofs knob closes that gap while the
lower's job is streaming multi-gigabyte files. `PCPU_KTHREAD` does what its name
says — the kernel says so — and it does not change the conclusion.

## If it were pursued, the cang-side work

1. Host: build the erofs image where the btrfs `.raw` is built today
   (`launch/components/persistent_disks/nix_lower.rs`), i.e. add `erofs-utils` to
   the runtime tools next to `mkfs.btrfs` — 2-4 min for 7.7 GB, and the image must
   be rebuilt when the store identity changes, exactly like the `.raw`.
2. Guest: mount it as its own read-only virtio-blk device
   (`mount -t erofs -o ro /dev/vdc /run/cang/nix-lower`) — no loop, no build-disk
   copy; `components/disk/nix_lower.rs` is a 5-line change (fs type + options).
   The overlay contract, `CANG_NIX_LOWER_BLOCK` marker and the readiness gate are
   untouched.
3. Keep the btrfs lower as the fallback for a firmware that predates the symbols —
   the same marker mechanism block mode already uses.

## Caveats, stated because they bound the result

- **The erofs arms read a file on the build disk through a loop device**, not a
  dedicated virtio-blk device; the btrfs arm reads `vdc` directly. The control that
  ties this off: uncompressed erofs through the loop matched 20 plain files on the
  same device to the MB/s (49 vs 49, boot 4), so the loop is not what the lz4/zstd
  arms are paying for.
- **The host page cache cannot be dropped from inside a guest**, so absolute cold
  seconds drift between boots (the btrfs lower alone ranged 25.0-36.7 s across
  arms that differ only in what ran before them). Every comparison above is
  within one boot, alternating arms, and the ordering never flips: the lower is
  fastest in all 6 boot-7/8 pairings.
- **Read set**: 20 largest files, 2.56 GB of a 7.9 GiB store — bandwidth-shaped,
  not metadata-shaped. `find`/`du` on erofs were not measured, so nothing here
  says whether erofs metadata matches btrfs's 1.5-3.9 s.
- **Compression options are not exhaustive**: 4 KiB pclusters, 64/256 KiB chunked
  forms and zstd-15 were tried; larger zstd pclusters and `-T` tuning were not.
  Nothing in the two boots suggests block size is the binding constraint (64 KiB
  chunks beat 4 KiB pclusters by only 10-15 %, and 256 KiB chunks were worse), but
  that is an extrapolation, not a measurement.

**Evidence.** `/home/dev/disp/blkread/` — `erofs/{zstd,lz4c64}.erofs` and their
`mkfs` logs, probes `probe3.sh`…`probe9.sh`, booters `run3.sh`…`run9.sh`, and the
per-boot out-dirs (`run4-*`…`run9-*`, each with
`workspace/evidence/{phases.txt,ds-*,cpu-*,trace.txt,kernel-tail.txt}`).
Firmware: `/home/dev/disp/fwbuild/` (`build.log`, `config-libkrunfw_x86_64-kvm-lto`,
`linux-7.3.1-kvm-lto/.config`), `fwbuild.sh`, `fwshim/` (unwrapped clang + lz4).
Boots: cang 0.21.0 HEAD, `localhost/cang:latest`, `--mem 4 --alloc hardened
--seccomp=off --landlock=off --root`, reused state root, guest 26 vcpus. Every arm
of every boot: 0 `csum failed`, 0 `I/O error`.
