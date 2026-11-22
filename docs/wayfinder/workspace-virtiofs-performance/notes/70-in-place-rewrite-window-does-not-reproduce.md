# Ticket 20: the 5 s in-place-rewrite window does not reproduce on the shipped tree

2026-11-21. Measured on `cang` `/nix/store/zc0q7b70281b9aj87z86sqx6xjv7z1a4-cang-0.22.0`
with the loaded image, guest kernel 7.3.1, `CANG_VIRTIOFS_ZERO_MESSAGE_OPEN` at its
default (on), `CachePolicy::Auto`, `PermissionSemantics::LinuxComplete` (cang
selects neither, so both take the defaults), `attr_timeout`/`entry_timeout` 5 s.

Ticket 20 asks whether to relax ticket 03's exact-visibility promise for
zero-message-open's 2.66x. Its premise is that a host-side **in-place** rewrite of a
file the guest already read is invisible to the guest for up to `attr_timeout`
(5 s) - note 23 recorded "6 s, not 1 s". On this tree that premise did **not**
reproduce, in either reader shape, at either rewrite shape.

Method: one `cang -- bash -lc` launch, the fixture inside the guest, a handshake so
the host rewrite happens *after* the guest has read the file (`/workspace/ready`),
and a marker (`/workspace/mark`) the guest polls for so both sides agree on "after
the rewrite" without comparing clocks. Samples every 200 ms; the host rewrite takes
6.9 ms and is a `printf > file` truncate-and-rewrite of the same inode.

| arm | sample right after the rewrite | later samples |
| --- | --- | --- |
| reopen each sample, size changed (49 -> 50 B) | new content at **0.00 s** | all new |
| held descriptor (`f.seek(0); f.read()`), **same size** (51 B -> 51 B) | new content at **0.00 s** | all new |
| held descriptor, size changed | new content at **0.00 s** | all new |

So on today's configuration there is no stale window to trade away: a host in-place
edit reaches a guest reader, held descriptor or not, within one 200 ms sample. The
`attr_timeout` window note 23 measured was either a different kernel's behaviour,
or a different reader shape (a long-lived mmap was not tested here, and note 63
already records that mmap readers were never measured).

What this does *not* say: that the window can never exist. It says the objection
ticket 20 was opened to decide did not reproduce under the configuration cang
actually ships, so the cost side of the trade-off (2.66x) has no visible benefit to
forgo.

Two consequences worth writing into the map:

1. the user-facing docs (and `crates/cang-libkrun/src/lib.rs`) state the 5 s window
   as a fact about the default; that claim is **unverified on the shipped tree** and
   should either be re-measured under the conditions it was first observed, or
   restated as "attribute-cached lookups may lag by attr_timeout; file *contents*
   were measured fresh in both reader shapes we tested";
2. the escape hatch (`--fs zero-message-open:off`) stays as it is either way - it
   costs only the round trips.
