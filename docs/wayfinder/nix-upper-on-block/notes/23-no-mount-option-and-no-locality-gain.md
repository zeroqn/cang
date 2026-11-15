---
label: wayfinder:note
ticket: 07-acceptance-arms
---

# No mount option and no guest-local copy moves the walk; the lever that remains is cache
# lifetime, and every device-side lever is now unsupported by measurement

One boot (`run-loc-20261115T165746Z`), block lower, cold arms with guest `drop_caches` and
a mincore-proved host drop of the image, guest device counters per arm.

## 1. `noatime` is a no-op, exactly as a read-only mount predicts

| mount | walk | device reads |
|---|---|---|
| `ro,relatime` (default) | 528 / 494 / 522 / 451 ms | 941 each |
| `ro,noatime` (remounted, verified in `/proc/mounts`) | 460 / 453 ms | 941 each |

The lower is mounted read-only, so the kernel never writes an access time back; `noatime`
removes a bookkeeping decision, not an I/O. It is not a lever, and the counter column proves
it (941 reads either way).

## 2. A guest-local copy of the store is *worse* cold, not better

Copied the 8.0 GB store into the guest's own writable directory (`cp -a`, 277 s) and walked
that instead:

| root | walk | notes |
|---|---|---|
| lower (read-only image) | 494-528 ms | 941 device reads |
| guest-local copy, cold | **1 583 / 1 769 ms** | the copy is a fresh btrfs with no locality advantage, and its walk issued ~32 000 reads |

So "put the store on the guest's disk" does not pay on first use. What *does* pay is a warm
cache: earlier arms over the same tree with the page cache intact ran 75-101 ms. The gain
there is cache lifetime, not placement.

## 3. Bulk reads are already at device speed, so the staging copy is not a lever either

639 MB single-stream from the lower: **1 424 ms wall / 444 MB/s**, with 1 602 ms of summed
device service in that window - i.e. service overlaps wall almost exactly. The copy into
guest memory (34-50 % of the *morning's* arms, note 09) is not visible here because the
device, not the copy, is the limit on a healthy host; that share was measured while the host
was thrashing (notes 11-13).

## 4. So where is the lever?

Recomputing the metadata walk from today's counters: 941 requests, ~470 ms, ~430-530 us each,
of which the host's own service is ~150 us (60-162 ms total). The remaining ~300 us per
request is the virtio-blk round trip plus the guest's btrfs and VFS work. Nothing that has
been tried moves it:

| candidate | measured outcome |
|---|---|
| mount options (`noatime`) | null, 941 reads either way (this note) |
| uncompressed image | ~30 % of host service, ~0 end to end (note 21) |
| erofs lower | 2x the requests, 3-4x slower (note 22) |
| guest-local copy | 3x slower cold (this note) |
| direct-to-guest read (fork prototype) | 1.5x at one reader, nothing at eight (note 09) |
| vhost-user-blk | would remove the same copy the row above already removed, and bulk is device-bound (this note) |
| DAX | a virtiofs feature; the virtiofs lower is already 2.1-3.0x slower for metadata (note 14) |

The one thing that has ever moved it is a **warm cache**: 75-101 ms against 451-528 ms. That
makes the lever *cache lifetime*, not the transport: fewer cold walks per launch (a longer
lived guest, a reused upper generation that also carries metadata, or not re-walking the
store at all) rather than a different way to read it.

## Evidence

`run-loc-20261115T165746Z`: `results.txt` (per-arm device counters, both `find` and bulk
arms), `atime.txt` (the remount and the `/proc/mounts` lines before and after), `copy.log`
(the 277 s copy), `setup.txt`. Probe `probe-loc.sh`, runner `run-loc.sh`, control
`control11.sh`. Earlier warm arms: `run-rb-20261115T044546Z` (`w1`/`w2`), `run-find-*`.
