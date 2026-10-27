# 35 - The two levers that shipped: no-flush and cacheable negative entries

Campaign evidence (raw, on this host, not in the repo):
`/home/dev/cang-bench/evidence/levers/` (switch A/B, 12 interleaved arms),
`/home/dev/cang-bench/evidence/ladder/` (negative-lifetime ladder + staleness),
`/home/dev/cang-bench/evidence/ship/` (end-to-end check of the shipped defaults).
Fork build: `v2.0.0-cang.13`. Workload: the 2401-crate synthetic tree from
`tools/rust-compile-bench`, guest image `ghcr.io/zeroqn/cang:latest`.

## What was asked

Issue #939 claims libkrun's default `CachePolicy::Auto` makes every guest open
drop that file's page cache, and proposes remembering inode attributes at open
time to set `FOPEN_KEEP_CACHE` when they are unchanged. The question here was
whether cang's zero-message open is the same mechanism, whether `always` would
add anything, and - if not - where the remaining virtio-fs cost actually is.

## Findings

1. **The cache policy is already spent.** `CachePolicy::Always` sets
   `FOPEN_KEEP_CACHE`/`FOPEN_CACHE_DIR` in the *open reply* and nothing else
   (`linux/passthrough.rs`, the `CachePolicy::Always` arm of `open`). Under
   zero-message open `open` answers `-ENOSYS`, the kernel sets `no_open`, and
   `fs/fuse/file.c` initialises `ff->open_flags = FOPEN_KEEP_CACHE |
   FOPEN_CACHE_DIR` as "default for no-open" - exactly the flag `always` would
   have shipped, and why the shipped lever was worth 2.42x rather than the 17.9%
   of requests it removed. `always` would now change only CREATE replies (their
   only remaining reader), on a brand-new empty inode. Issue #939's proposed fix
   lives in `do_open`, which this share no longer reaches.
2. **Metadata timeouts are spent too.** Measured split of a build's lookups:
   **91.5% are negative** (a name that does not exist). Raising `entry_timeout`
   5 s -> 60 s cut positive lookups 28.5% and total lookups 2.3%, but every
   removed LOOKUP returned as a GETATTR (+39.8%): `LOOKUP+GETATTR` moved -0.15%
   and build time -0.2%, inside the rig's spread. The reason the arm could never
   work is finding 3.
3. **`-ENOENT` is not cacheable, and that is what the lookups are.** The kernel
   drops the dentry for an `-ENOENT` reply; only `nodeid == 0` with a valid
   `entry_valid` caches a miss (`fs/fuse/dir.c`, v7.2.10 - the tarball
   `deps/libkrunfw/Makefile` builds). So the whole 91.5% class was immune to
   every timeout. Answering with the negative entry is the only lever that
   touches it: **LOOKUP 541k -> 262k (-72%), requests 1.71M -> 0.89M, build
   171.4 s -> 135.8 s (-20.8%)**.
4. **FLUSH is a server-side decision, not a kernel flag.** `fuse_flush` honours
   `FOPEN_NOFLUSH` as an early return (`file.c:479-480`), which also skips
   `write_inode_now` and `AS_EIO`; answering `-ENOSYS` instead sets `no_flush`
   *after* those (`:505-507`), so the server-side answer keeps them and drops only
   the round trip - with no firmware rebuild. Nothing lock-related is lost: the
   share negotiates neither `POSIX_LOCKS` nor `FLOCK_LOCKS`, so the guest locks
   locally and `filp_flush` -> `locks_remove_posix` releases them whatever
   `->flush` does (and the histogram shows zero SETLK/GETLK). Measured: **FLUSH
   211,877 -> 2 at 1 M requests, build 171.4 s -> 153.5 s (-10.5%)**.
5. **They add, because each removed request costs the same.** -17.9 s and
   -35.6 s separately, **-55.1 s** together (-32.2%), against a two-point fit of
   **~53 us of build wall time per FUSE request**; the fit predicts the flush arm
   to 0.1 s. Guest 116.3 s against a 115 s host-native build: the share costs
   ~1% where it cost 49% before zero-message open.
6. **The negative lifetime is the price, so make it short.** Invisibility of a
   host-created name equals the entry lifetime. The ladder: 0.1 s keeps 86.3% of
   the LOOKUP reduction, **0.25 s keeps 92.3%**, 1 s 97.9%, 2 s 99.1%, 5 s 100%.
   At 0.25 s the measured invisibility was **74 ms**; at 5 s it was 2.0-2.8 s.
   cang ships 250 ms, which converts an undocumentable 5 s hole into a
   quarter-second one for 8% of the request reduction.

## What shipped

- fork `v2.0.0-cang.13`: `FsDevice::set_no_flush(bool)` and
  `FsDevice::set_negative_entry(bool, timeout_millis)` (+ the C API, `CANG.md`),
  a positive/negative/error split in the opcode histogram, and the removal of
  the temporary cheap-lookup debug counters an earlier pass left in `lookup`.
- cang: `CANG_FS_NO_FLUSH = true`, `CANG_FS_NEGATIVE_ENTRY_TIMEOUT_MILLIS = 250`
  in `crates/cang-libkrun/src/lib.rs`, set on the share in the launcher, each
  overridable per launch (`CANG_FS_NO_FLUSH=0`,
  `CANG_FS_NEGATIVE_ENTRY_TIMEOUT=0` or a millisecond value), README's visibility
  section rewritten around the bounds those settings create.

## What is left, and why it was not taken

- **Cheapening a LOOKUP does not pay.** A lookup is ~130-144 us of wall but only
  8-19 us of server CPU; the rest is waiting across six wakeups. Everything
  removable inside the server (5 small allocs, 2 mutexes + 1 irqfd write per
  reply, and the double host path walk on the negative path) is 5-6%.
- **`entry_timeout=60` on top of the negative entry**: +3.5%, inside the noise,
  and it stretches the invisibility hole to 60 s. Rejected.
