# Note 53: writeback, attr/entry timeouts and guest RAM, priced on a 23-crate build

2026-11-05/06. Follows note 52 (same tree: 23 crates, 1 053 dependencies, mounted
from btrfs, `cargo test --profile fast --workspace --no-run`, `JOBS=6`,
`clang -fuse-ld=wild`, warm sccache). One binary, knobs behind environment
variables so every arm boots the same code; arms interleaved, one boot per arm.

## The three levers, rejected by measurement

Warm-sccache baseline is 269 s (round 1) and 347 s (round 2) - the host drifted
between rounds, so only same-round deltas mean anything.

| arm | round 1 | round 2 | vs control | requests | WRITE | READ | GETXATTR | GETATTR |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| control | 269 s | 347 s | - | 1 595 553 | 517 749 | 293 230 | 291 670 | 91 024 |
| `CANG_FS_WRITEBACK=1` | 289 s | 375 s | **+7.4%, +8.1%** | 1 355 780 | 79 998 | 127 313 | 291 740 | 330 488 |
| `CANG_FS_ATTR_TIMEOUT=30000` | 280 s | 330 s | +4.1%, -4.9% | 1 606 321 | 519 734 | 300 006 | 291 671 | 89 582 |
| both | 363 s | 345 s | +35%, -0.6% | 1 371 654 | 80 343 | 136 268 | 291 765 | 323 990 |

**Write-back caching removes 15% of the requests and makes the build 8% slower.**
Mechanically it does exactly what it should: `WRITE` 518 k -> 80 k (-85%, the same
11 GiB in 128 KiB flushes instead of 21 KiB requests) and `READ` 293 k -> 127 k
(-57%, served from the client's own page cache). What it adds is `GETATTR`
91 k -> 330 k (+263%): a write-back client can no longer trust the attributes it
has, so it revalidates far more often. Net requests -15%, net wall clock +8%.

**A 30 s attribute timeout does nothing.** Requests, and `GETXATTR` to five
digits, are unchanged. That kills the hypothesis note 52 raised - that attribute
refreshes clear `S_NOSEC` and each write then pays the capability probe. The probe
is per `write(2)`, not per refresh: a phase test showed 300 appends to one file
producing exactly 300 `GETXATTR`.

Kernel-side reason, for the record: `fs/inode.c: file_remove_privs_flags()`
returns early once `S_NOSEC` is set, and sets it via `inode_has_no_xattr()` after
a successful check - so something clears it on every write. Both the write path's
attribute update (`fuse_change_attributes_common` clears `S_NOSEC` explicitly) and
`security_inode_need_killpriv` are client-side; no server-side knob reaches it.
Acknowledging `FUSE_HANDLE_KILLPRIV_V2` (note 52) does not either.

## What the request count no longer predicts

Note 51 read a cold build as "10 k requests = 1 s". These arms falsify that as a
general rule: -240 k requests bought **+20 s**. The classes are not
interchangeable - `WRITE` carries ~21 KiB and is bounded by the data path
(~166 MB/s measured), `GETATTR`/`LOOKUP` are sub-100 us metadata answers, and
`GETXATTR` is one round trip per guest `write(2)` whatever else is negotiated.
Price an arm by its own class, never by the total.

## Where the remaining wall clock is

1.6 M requests over 269 s is ~6 k requests/s, i.e. the build sits at the request
ceiling across the whole run, and the classes that remain are the ones no shipped
lever removes: `WRITE` 518 k (data), `READ` 293 k (data), `GETXATTR` 292 k
(client-side, proven unreachable above), `LOOKUP` 190 k (fresh names in a fresh
`target/`), `GETATTR` 91 k, `CREATE` 64 k.

That leaves two structural answers, both outside the `--fs` surface:

- **build outputs off the share** (a block device or a tmpfs for `target/`) - the
  share's marginal cost is 2.2-2.9x on the synthetic tree (note 51), and the whole
  of `WRITE` (11 GiB) plus most `READ` lives in `target/`;
- **DAX for `/workspace`** (tickets 04/13), which is the only way those byte
  classes stop being requests at all.
