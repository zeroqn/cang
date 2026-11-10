---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# Chasing the cold-read wall: `max_sectors_kb`, fragmentation, and the O_DIRECT gap

**Verdict.** Both hypotheses in the plan are settled **no**, and the third finding
- that guest *buffered* reads are 22x slower than `O_DIRECT` on the same bytes -
is real in one boot and **not reproducible in another**, so it stays a hypothesis
with a named blocker: the host page cache cannot be dropped from this account
(no root), so an arm's device service time is never known on the host side. Six
boots, ~90 arms. Nothing here changes cang's code.

## 1. `max_sectors_kb` does not rescue the block lower - settled no

Boot 10, one guest, one manifest (20 files, 2.56 GB), `drop_caches` before each
arm, device counters per arm (request count and sectors off `/sys/block/vdc/stat`):

| knob | ms | requests | avg KiB/req | device GB | device MB/s |
|---|---|---|---|---|---|
| default (4096) | 39.5 | 977 | 2569 | 2.57 | 65 |
| 512 | 48.9 | 5312 | 474 | 2.58 | 53 |
| 128 | 51.5 | 19998 | 126 | 2.58 | 50 |

Smaller requests make it *worse*, monotonically. The first campaign's apparent
5x win at 512 was the host page cache, not the knob. `read_ahead_kb` 0 / 8192 /
131072: 34.1 / 44.1 / 36.4 s - inside the run-to-run spread.

## 2. Image fragmentation is not the wall - settled no

The premise was: 89 488 extents, 87 792 of them under 32 KiB, so a 2.5 MiB guest
read is ~100 short host reads. **Refuted from inside the guest.** Reading that
same fragmented image:

| arm (boot 10) | ms | device GB | device MB/s |
|---|---|---|---|
| buffered | 39 529 | 2.57 | 65 |
| `iflag=direct`, 1 MiB | 1 807 | 2.59 | **1 433** |
| `iflag=direct`, 8 readers | 760 | 2.58 | **3 400** |
| `iflag=direct`, 256 KiB | 3 856 | 2.57 | 666 |
| `iflag=direct`, 4 KiB | 137 476 | 2.56 | 19 |

`O_DIRECT` with *more, smaller* requests (5185 x 488 KiB) beats buffered with
*fewer, larger* ones (977 x 2.5 MiB) by 22x, and 8 direct readers reach 3.4 GB/s.
Fragmentation cannot be the limit if the same extents stream at 3.4 GB/s. From the
host the same `.raw` reads at 1.5-3.1 GB/s direct and 4-7 GB/s from cache, so the
host's storage does not impose 60 MB/s either.

## 3. The open question, and why it is still open

The gap above says the cost is in the guest's **buffered page-fill**, not in the
transport, the request shape, the filesystem's packing or the host disk. What it
does not survive is a second boot: on boot 18 an ext4 image on the guest's own
block disk read the same 20 files at 32 MB/s buffered, 58 MB/s `O_DIRECT` and
85 MB/s `iflag=nocache` - no 20x effect at all, on any arm. (That arm is also not
a clean filesystem comparison: ext4 there sits on a loop device over guest btrfs,
so every read crosses two filesystems.)

What is common to every boot, and therefore all this note can lean on:

- the btrfs lower is never slower than the alternatives tried;
- packed erofs is 13 % slower on the shipped firmware and 1.7-2.2x slower with
  `ZIP_ZSTD` + `PCPU_KTHREAD` rebuilt in ([note 05](05-erofs-packed-lower-prototype.md));
- the same-boot spread of an unchanged arm is 25.0-36.7 s (btrfs lower,
  sequential, `drop_caches` before each) - i.e. **±20 %**, so only a >1.5x effect
  is measurable at all in this harness.

**The blocker, named.** `echo 3 > /proc/sys/vm/drop_caches` fails on the host
(not root), and the host holds 11.5 GB of page cache for a 7.9 GiB image, so a
guest arm's reads may be served from host RAM (fast) or from the device (slow)
and nothing inside the guest can tell which. That is enough to move an arm by
20x, which is the size of the effect being chased. The measurement that would
settle it: sample the **host's** `/proc/diskstats` for `sdb` and the host's
PSI/`vmstat` at 1 Hz during each guest arm, so every arm carries its own device
service time; with an externally-idle host, that separates "guest page-fill
costs" from "the host served these bytes from cache that boot". Until then,
treat any single-boot 2x+ claim about the block lower's read path as unproven -
including the 22x above.

## Tooling faults found in this campaign (both mine, both now screened)

- **`dd bs=1m` is rejected by this guest's dd (coreutils 9.11)** - `dd: invalid
  number: '1m'`. Six arms in one boot silently read nothing and reported 50 GB/s
  because the speed was computed from the file's size, not from dd's byte count.
  Every arm from boot 16 on carries a `bs=1m` probe line in its evidence.
- **A one-field path list paired with `while read -r sz f`** makes `f` empty, so
  `dd if=""` fails instantly; two "buffered" arms in boot 18 read 0 bytes and
  looked like 12 GB/s. Any arm now either reads `size path` pairs or is scored
  only from the device counters.

**Evidence.** `/home/dev/disp/blkread/` - boots `run9`…`run18` (each
`workspace/evidence/{phases.txt,ds-*,cpu-*,trace.txt}`), probes `probe9.sh`…
`probe16.sh`, booters `run9.sh`…`run18.sh`, host-side `hostio.sh`/`hostio.log`
(host curve on the `.raw` and on a fresh 8 GiB file), `fsimg/` (the ext4 image
built with `mkfs.ext4 -d` from the same 20 files). Host facts: `/dev/sdb`
150 GiB btrfs at 85-89 % used, 11.5 GB cached, load 0.8-2.4 during the campaign.
