# Ticket 33: an mmap'd guest reader never sees a host in-place rewrite

2026-11-21. Measured on `cang` `/nix/store/zc0q7b70281b9aj87z86sqx6xjv7z1a4-cang-0.22.0`
(the same build ticket 20 used), guest kernel 7.3.1, in-place `printf > file` from
the host, marker-file handshake so the host rewrites only after the guest has
faulted its mapping.

Fixture: `/workspace/f.txt`, 49 B; the guest maps it (python `mmap`, `ACCESS_READ`
= MAP_SHARED or `ACCESS_COPY` = MAP_PRIVATE), reads 8 bytes *before* signalling
ready, then samples the mapped bytes every 0.5 s for 12 s, then re-maps the same
path and reports what a fresh mapping sees.

| arm | mapped bytes after the rewrite | fresh re-map |
| --- | --- | --- |
| MAP_SHARED, same size (49 -> 47 B) | **stale for the whole 11.5 s** | new bytes at once |
| MAP_SHARED, file grown (49 -> 71 B) | **stale for the whole 11.5 s** | new bytes at once |
| MAP_SHARED, host truncates to 0 | **stale for the whole 11.5 s**, no `SIGBUS` | `ValueError` (empty file) |
| MAP_PRIVATE, same size | **stale for the whole 11.5 s** | new bytes at once |
| MAP_SHARED, `--virtiofs zero-message-open:off` | **stale for the whole 11.5 s** | new bytes at once |

So:

1. **The staleness is the mapping, not the lever.** Turning zero-message-open off
   changes nothing: an already-faulted mapping keeps its pages, because nothing
   tells the guest kernel the host changed the file. In FUSE that notification is
   `FUSE_NOTIFY_INVAL_INODE` - which the *guest-side* relay watcher sends to the
   host (ticket 32), and which nothing sends in the host-directory direction.
2. **11.5 s is past the 5 s attribute/entry timeout**, so this is not a metadata lag
   that eventually heals: it heals only when the reader re-maps (proved by the
   `REMAP` line in every arm).
3. **A `read(2)` reader is fine** - ticket 20 measured a held descriptor and a reopen
   both fresh at the first 200 ms sample. The gap is exactly the `mmap` shape that
   ticket 20 could not speak for.
4. **Truncation does not `SIGBUS` the guest**: the stale pages stay readable, which
   is the safer of the two failures but also the quieter one - a reader sees
   plausible old bytes rather than an error.

What the docs may now claim: `/workspace` contents are fresh for `read(2)` readers
in the shapes measured (open, reopen, held descriptor), and an **`mmap`'d reader can
stay stale until it re-maps**; the host-directory share has no mechanism to
invalidate a guest mapping. A guest whose own tooling mmaps a workspace file it
expects the host to rewrite should re-open instead, or run under
`--workspace-relay` (where the guest is the writer).

Not measured: whether a `MAP_SHARED` **write** from the guest propagates to the host
file (a different direction, and the same class as the relay's write path), and
whether the virtual filesystem's `INVAL_INODE` (ticket 32) refreshes a guest mapping
when the writer is on the host - it cannot be, since nothing sends one in that
direction today.
