# Guest-side invalidation: what the kernel honours, what goes stale, and what it costs

2026-11-21. Ticket 30 (research). Prototype and arms live in the untracked
`.scratch/inval-probe/` (its `NOTES.md` is the raw record: `probe-pump/`,
`design-pump/`, `arms/arm2..arm5.sh`, `tools/held_reader.c`, `guest/watch.py`).
Everything was measured through the **product path** - `cang --workspace-disk
/tmp/rl.raw --workspace-relay` with `CANG_RELAY_PUMP=<probe binary>`, so the mount,
the guest relay, the vsock transport and `/dev/fuse` are the shipped ones - with
cang `/nix/store/413b9amf0x2d7pw34v1hdbqqrl915sga-cang-0.22.0` and image
`localhost/cang:latest 4edac48b4182`; host kernel `7.3.4-xanmod1`.

## 1. The mechanism: honoured, with no feature bit

The mount's INIT is `major=7 minor=33 flags=0x406020 max_write=1048576 max_pages=256`
- nothing invalidation-related is negotiated, and nothing needs to be: a write to
the mount's `/dev/fuse` whose `out_header.unique` is `0` is dispatched as a
notification by `out_header.error`, and **the write's return value is the kernel's
verdict**.

| frame | bytes | live inode | nodeid/name the mount does not hold |
| --- | --- | --- | --- |
| `INVAL_INODE` (code 2: ino, off, len) | 40 | `written=40 errno=0` | `ino=9999` -> `written=0 errno=2` (ENOENT) |
| `INVAL_ENTRY` (code 3: parent, namelen, flags + name) | 16+16+namelen+1 | `errno=0` | `parent=9999` -> ENOENT; unknown name -> ENOENT |
| `DELETE` (code 6: parent, child, namelen, flags + name) | 16+24+namelen+1 | `errno=0` | - |

The `ENOENT` arms matter: they are the free check of a watcher's nodeid map - a
stale nodeid answers `ENOENT` instead of silently doing nothing.

**What each kind actually refreshes** (two reader shapes: a descriptor the host
holds open, and a fresh `stat` of the path):

| notification | held fd, size changed | held fd, **same size** | `stat` of the path |
| --- | --- | --- | --- |
| none (OFF) | 4.955-4.988 s (n=5) | **no change in 9 s** (n=3) | 4.87-4.96 s (n=4) |
| `INVAL_INODE` off=0 len=0 | 0.77-1.35 ms (n=8) | 1.03 / 1.18 / 1.27 ms | 0.55-0.92 ms (n=5) |
| `INVAL_INODE` off=0 len=-1 | 0.98 / 1.20 ms | 1.33 ms | 0.72 / 0.79 ms |
| `INVAL_INODE` off=-1 len=0 | 0.92 / 1.20 ms | **not invalidated** | 0.59 / 0.73 ms |
| `INVAL_ENTRY` parent=1 + name | 4.962-4.967 s (no help) | **not invalidated** | 0.68 / 0.70 ms |
| `DELETE` parent/child + name | 4.962 / 4.972 s (no help) | **not invalidated** | 0.63 / 0.74 ms |

So for **content** only `INVAL_INODE` works, and it needs `off=0` (`len=0` or
`-1`); `off=-1` is attributes-only, and `INVAL_ENTRY`/`DELETE` refresh names and
attributes but leave an open inode's page cache alone.

## 2. The stale window is a *held descriptor*, not `cat`

`CachePolicy::Auto` (OPEN without `FOPEN_KEEP_CACHE`) means **every `open()`
drops the cached pages**, so a host `cat`, `git` command or editor *open* is
always fresh - which is exactly what ticket 23 wrote down ("stale until it reopens
it"). The dangerous shape is a reader that keeps the descriptor (the agent harness
itself, a long-lived process, an mmap): 5 s for a size change, and for a
**same-size rewrite, never** - nothing in the 5 s attribute refresh drops pages
whose size did not change.

## 3. The pump: the serial drain is not enough, the polled one is cheap

The reply-path-only drain (noticing `unique == 0` while reading "the reply") only
delivers while a request is in flight, so the first host access after a guest
write - the one that matters - is not fixed by it:

| arm (`arms/arm5.sh`, same-size rewrite, frame queued) | result |
| --- | --- |
| queued, no other host traffic | fresh at **4.98 s** - the OFF window, because the only request that arrives is the 5 s attr refresh |
| queued, one unrelated host request at +0.54 s | fresh **1.78 ms** after delivery inside that reply path |

The design that works puts the **transport in the pump's poll set**
(`design-pump/src/pump.rs`): a readable transport with nothing in flight is the
guest speaking on its own, so the frame goes to `/dev/fuse` immediately. Offline,
through the real relay loop with a socketpair transport and no request ever sent,
the frame reaches `/dev/fuse` **135.6 us** after the guest writes it, and the
no-poll control never delivers it (two tests pin this). Combined with the live
0.5-1.4 ms, notification -> host-fresh is **~1 ms**, i.e. one FUSE round trip.

Cost of delivering: 1000 frames for one live inode in **1.570 ms = 1.57 us per
notification** (14.7 us with a log line each - logging dominates).

Lines touched for that design, against `crates/cang-relay-pump/src/pump.rs`:
**+138 / -40, 9 hunks, one file**, `main.rs` unchanged, and all 17 existing crate
tests pass unchanged (the `-EIO`-on-death contract survives). One required
correction inside that count: **a refused notification (the normal `ENOENT` from a
map that lags) must be logged and skipped, never treated as a transport failure** -
the first version returned an error there, which would have flipped every later
request to `-EIO`. A `POLLIN`-only gate is what keeps the `/dev/fuse` death
semantics unchanged; widening it to `POLLHUP` changed when a death was noticed
(the existing test failed until the gate was narrowed).

## 4. The watcher's price

| thing | number |
| --- | --- |
| watch limit in the guest (`fs.inotify.max_user_watches`) | **122 262** (queued events 16 384; host limit 524 288) |
| directories in this repo (`crates/` + `deps/` + `.git`) | 69 + 838 + 467 = **1 374** |
| the same with a populated `target/` | **3 641** (2 267 dirs, 25 k files) - ~3 % of the limit |
| storm: a 766-entry tar (666 files, 101 dirs) extracted twice | **2 899 / 2 910** events each, 101 watches in 0.002 s, 360-386 ms -> **~7.5-8.1 k events/s**, 4.4 events per file (CREATE, MODIFY, CLOSE_WRITE, DELETE from tar's unlink-then-create) |

So watches are not the constraint; event rate is ~4.4 per file written, and each
notification costs 1.57 us on the host. The guest side is still to be built
(~30-40 lines of path->nodeid map filled from the relay's own LOOKUP/READDIRPLUS/
CREATE replies, ~120-160 lines of watcher, ~30-50 lines of shared writer so
notifications and replies use one connection).

## 5. What this does and does not promise

- it closes the **held-descriptor** window: a consumer polling a file it already
  has open sees a guest write in ~1 ms instead of 5 s or never;
- it does **not** synthesise a host `inotify` event: FUSE invalidates cache, it
  does not deliver events. An editor that waits for `inotify` still needs a
  reopen (or, later, a polling re-stat);
- `mmap` readers were not measured; the evidence covers read(2) through the page
  cache.
