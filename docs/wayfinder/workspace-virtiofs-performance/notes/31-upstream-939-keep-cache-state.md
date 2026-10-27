---
label: wayfinder:research
title: Upstream #939 - is a keep-cache-on-open fix landed, and does `cache_policy = always` add anything to cang?
status: done
blocked_by: []
claimed_by: pi upstream child (2026-10-27)
---

# 31 - Upstream state of libkrun #939 / #924, the kernel contract, and whether `Always` buys cang anything

Read as the upstream half of the page-cache question raised against this map:
[13](13-cache-semantics-and-scoping.md) established what each `CachePolicy` sets,
[23](23-zero-message-open-results.md) established that the fork's zero-message
open already keeps the page cache and what that was worth. This note answers the
new question: **is cang's zero-message-open win the same mechanism as #939, and
would flipping the default to `CachePolicy::Always` add anything?**

Every claim below is fetched, not remembered; URLs are the citation. Where
something could not be verified it says so.

## Verdict

1. **Same mechanism, already shipped, already unconditional.** #939's symptom is
   produced by the guest kernel dropping the inode's pages at open when the
   server's open reply lacks `FOPEN_KEEP_CACHE`
   (`fs/fuse/file.c:296-297`), and the mechanism that removes it is exactly that
   flag. Under `FUSE_NO_OPEN_SUPPORT` the kernel sets `ff->open_flags =
   FOPEN_KEEP_CACHE | (isdir ? FOPEN_CACHE_DIR : 0)` itself, as the *default for
   no-open* (`fs/fuse/file.c:154-156`). cang's fork answers `FUSE_OPEN`/`FUSE_OPENDIR`
   with `-ENOSYS`, so every file and every directory in the guest is already on
   "keep the cache" semantics - the `Always` per-open flag set without the policy.
   Note 23's READ 35.76% -> 3.26% is that mechanism paying out.
2. **`CachePolicy::Always` would add nothing measurable for cang.** With
   zero-message open the open reply never travels (`open()` returns `-ENOSYS`
   before `do_open`, fork `passthrough.rs:1396-1398`), so the only place the
   `Always` flags are still produced is `create`'s reply opts
   (`passthrough.rs:1472-1478`) - the already-empty page cache of a brand-new
   file. It is a no-op on the read path and it does not touch invalidation at
   all; freshness is `AUTO_INVAL_DATA`'s job and `Always` leaves it alone.
   It would only matter if zero-message open were switched off.
3. **Nothing has landed upstream, and the fix that is proposed is not the
   `Always` switch.** #939 is open with **0 comments**; the linked PR #940 is
   open, unreviewed, 1 commit, +36/-3, and its base is still main's HEAD. There
   is no merged fix, no maintainer reply to either. #924 asked for the C API and
   the maintainer answered "reasonable" but nothing was implemented.
4. **PR #940 is not equivalent to `AUTO_INVAL_DATA`** - it is strictly earlier.
   The kernel's auto-inval check compares only **mtime and size**, and only when
   `attr_timeout` has expired *and* something asks for attributes (a `read(2)`
   or a stat); a pure `mmap` page fault never triggers it. The PR compares
   **mtime + ctime + size at every open**, so a reopen notices a host change
   immediately instead of up to 5 s later. It does restore close-to-open for the
   open path (which is what close-to-open means), and it does *not* fix the
   long-lived mapping / never-reopened fd case, which nothing short of
   `FUSE_NOTIFY_INVAL_INODE` fixes.
5. **For cang the PR is dead code on the share it targets.** `do_open` is
   unreachable with zero-message open, and the PR does not touch `create`. Its
   only portable value here is as documentation of the trade-off.

## 1. Upstream issue/PR state (all fetched 2026-10-27)

### 1.1 #939 - open, no comments

- https://github.com/libkrun/libkrun/issues/939 - `state: open`,
  `comments: 0`, created and last updated `2026-10-23T22:17:22Z`
  (`https://api.github.com/repos/libkrun/libkrun/issues/939`).
- Reporter `jgoyvaerts`; reproducer is a standalone C program that mmaps 64 MiB,
  has a *different* process `open()`+`close()` it, then checks residency with
  `mincore()`: host ext4 100% -> 100% (0.1 ms), muvm guest on libkrun 1.19.6
  `100% -> 0.0%` and 33-44 ms to re-touch, 3 of 3 runs.
- Its own statement of the root cause matches the source: "The server never sets
  `FOPEN_KEEP_CACHE`, so the guest kernel invalidates the inode's mapping on every
  open (close-to-open consistency). `device.rs` builds the
  `passthrough::Config` with `..Default::default()`".
- The issue also runs the *other* direction, which any fix must keep: with
  `--host-change` the host rewrites the 64 MiB file in place (same size) and the
  guest must see the new bytes on the next open. "Stock libkrun passes it."

### 1.2 PR #940 - open, unreviewed, base == main HEAD

- https://github.com/libkrun/libkrun/pull/940 - `state: open`, `merged: false`,
  `commits: 1`, `additions: 36`, `deletions: 3`, `changed_files: 1`,
  `comments: 0`, `review_comments: 0`, `mergeable: true`,
  `mergeable_state: unstable`; head `jgoyvaerts:fs-keep-cache-unchanged`
  `cc69bd4a198a704041fd7602744df43219f53cdf`, base `libkrun:main`
  `a1d8616765da03980f3be1bb8714bfdd3842f33c`, created 14 s after the issue
  (`https://api.github.com/repos/libkrun/libkrun/pulls/940`).
- **CI did not run the build**: the head commit has exactly one check run,
  `DCO / success`
  (`https://api.github.com/repos/libkrun/libkrun/commits/cc69bd4a198a704041fd7602744df43219f53cdf/check-runs`).
  No compiler job, no review, no maintainer comment - the "unstable" merge state
  is that, not a red build.
- PR body claims, self-reported: patched 1.19.6 gives `100% -> 100%` resident and
  `0.2-0.5 ms` re-touch vs stock `100% -> 0%` / `33-44 ms`, and the
  `--host-change` direction still sees new contents; on `main` only `cargo fmt`,
  a `krun-devices` build and the fs unit tests were run, and the author states he
  **could not run `main` end to end** (muvm links `.so.1`, main builds `.so.2`).
  So the end-to-end evidence is against 1.19.6, not against the commit in the PR.

### 1.3 Main has not moved and carries no cache knob

- `git log` of the touched file, latest first:
  `438c667002 2026-10-10 virtio/fs/linux: do not assume UTF-8 file names in the
  lookup log`, then 2026-09-21, 2026-06 - nothing about caching, and no commit
  mentioning `cache`, `keep_cache` or `writeback` in that path's history
  (`https://api.github.com/repos/libkrun/libkrun/commits?path=src/devices/src/virtio/fs/linux/passthrough.rs&per_page=100`).
- Main's HEAD is `a1d8616765` (2026-10-17), i.e. exactly the PR's base - the PR
  is not stale, it is simply unmerged
  (`https://api.github.com/repos/libkrun/libkrun/commits?per_page=10`).
- Main's `FsDevice` C API is `krun_fs_device_new` / `_new_read_only` / `_new_null`
  / `krun_fs_device_set_overlay` / `krun_fs_device_set_dax_window_size`
  (`include/libkrun.h:211-253` in
  https://raw.githubusercontent.com/libkrun/libkrun/main/include/libkrun.h):
  **no cache-policy and no writeback setter**.

### 1.4 #924 - the C API request, maintainer says "reasonable", nothing shipped

- https://github.com/libkrun/libkrun/issues/924 - open, 1 comment. Body documents
  `config.num_request_queues = 1`, `..Default::default()`, `writeback: false`,
  one worker thread, no setter in the C API, and asks for one.
- Maintainer `slp`, 2026-10-14
  (https://api.github.com/repos/libkrun/libkrun/issues/924/comments):
  "Enabling writeback can be pretty dangerous, since you need to make sure to
  quiesce every virtio-fs device before shutting down the VM or you risk losing
  data." and "Extending the API to be able to set `writeback` and `cache_policy`
  is reasonable." (and, for the CI case: virtio-fs for workload+results, tmpfs or
  virtio-blk for transient data).
- Released 1.x has the same hole: `krun_add_virtiofs4(ctx_id, c_tag, c_path,
  shm_size, read_only, semantics)` - five arguments, no policy
  (`include/libkrun.h:385-387` at tag v1.19.7,
  https://raw.githubusercontent.com/libkrun/libkrun/v1.19.7/include/libkrun.h).
  v1.19.7 is the newest tag
  (`https://api.github.com/repos/libkrun/libkrun/tags?per_page=15`).
- So **there is no released or merged way for any consumer to select
  `CachePolicy::Always`**; the only way to ship it today is to patch the fork's
  default or add the API - which is exactly what this map's fork would do.

## 2. The kernel contract, from the authoritative sources

### 2.1 `FOPEN_KEEP_CACHE` and the no-open default

- `include/uapi/linux/fuse.h:387-389`: "FOPEN_KEEP_CACHE: don't invalidate the
  data cache on open" / "FOPEN_CACHE_DIR: allow caching this directory";
  `:402-404` the bits
  (https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/include/uapi/linux/fuse.h).
  Also `:426-431`, `:480` `FUSE_AUTO_INVAL_DATA` "automatically invalidate cached
  pages", `:485` `FUSE_NO_OPEN_SUPPORT` "kernel supports zero-message opens".
- The open-time invalidate itself is 3 lines: in `fuse_open_common`,
  `if (is_truncate) truncate_pagecache(inode, 0); else if (!(ff->open_flags &
  FOPEN_KEEP_CACHE)) invalidate_inode_pages2(inode->i_mapping);`
  (`fs/fuse/file.c:291-298`,
  https://raw.githubusercontent.com/torvalds/linux/master/fs/fuse/file.c).
- The no-open default, which is what makes cang's share cache everything:

```c
 154	ff->fh = 0;
 155	/* Default for no-open */
 156	ff->open_flags = FOPEN_KEEP_CACHE | (isdir ? FOPEN_CACHE_DIR : 0);
```

  `fs/fuse/file.c:154-156`. `-ENOSYS` on the first `FUSE_OPEN` sets
  `fc->no_open = 1` (`:169-177`) and the kernel then skips the request entirely
  (`:139`, `:157`) - the code path cang's fork provokes deliberately
  (fork `passthrough.rs:1396-1398`, `device.rs`/`server.rs`
  `ZERO_MESSAGE_OPEN`/`ZERO_MESSAGE_OPENDIR` in `supported`).
- libfuse documents the same two ends: `FUSE_CAP_NO_OPEN_SUPPORT` /
  `FUSE_CAP_NO_OPENDIR_SUPPORT` mean "may return ENOSYS from the open() handler
  to indicate success. Further attempts to open files will be handled in the
  kernel" (`include/fuse_common.h:365-380`, `:448-461`,
  https://raw.githubusercontent.com/libfuse/libfuse/master/include/fuse_common.h).

### 2.2 `FUSE_AUTO_INVAL_DATA` - the only freshness channel, and how lazy it is

- libfuse, `include/fuse_common.h:279-309` (verbatim): "If this flag is set, FUSE
  will check the validity of the attributes on every read. If the attributes are
  no longer valid (i.e., if the *attr_timeout* ... has passed), it will first
  issue a `getattr` request. **If the new mtime differs from the previous value,
  any cached file *contents* will be invalidated as well.**" Then: "To see
  changes made outside the kernel without this flag, set `fi->direct_io` in
  open(), or call `fuse_lowlevel_notify_inval_inode()` ... Without
  `fi->keep_cache`, the cache is also dropped on every open."
- Kernel side, `fs/fuse/file.c:1116-1135` `fuse_cache_read_iter`: with
  `fc->auto_inval_data`, **every `read(2)`** calls
  `fuse_update_attributes(inode, file, STATX_SIZE)` first. That call only sends a
  `GETATTR` when the attribute timeout has expired:
  `sync = time_before64(fi->i_time, get_jiffies_64())` (`fs/fuse/dir.c:1558-1594`,
  https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/fs/fuse/dir.c).
- `fs/fuse/inode.c:363-386` then compares and invalidates:

```c
 363	if (!cache_mask && S_ISREG(inode->i_mode)) {
 364		bool inval = false;
 366		if (oldsize != attr->size) { truncate_pagecache(inode, attr->size); ... }
 370		else if (fc->auto_inval_data) {
 380			if (!timespec64_equal(&old_mtime, &new_mtime))
 381				inval = true;
 384		if (inval)
 385			invalidate_inode_pages2(inode->i_mapping);
```

  Note the two limits: **only mtime and size** are compared (no ctime), and
  `cache_mask` is non-zero (so the whole check is skipped) under
  `FUSE_WRITEBACK_CACHE` (`inode.c:304-312`, `:330-341`) - the same fact note 13
  §1.4 recorded.
- **Two consequences for #939's scenario**, neither of which is in the issue:
  1. A `mmap` page fault does *not* go through `fuse_cache_read_iter`; it goes
     through `filemap_fault`. So a mapped shared library whose pages are faulted
     in is **not** covered by the auto-inval refresh unless something also does a
     `read()`/`stat()`. The reproducer's "another process opens it" case is fixed
     by `KEEP_CACHE` alone (no invalidation happens at all), which is why
     `Always` fixes the reported symptom while the auto-inval check never fires.
  2. Under `KEEP_CACHE` the freshness window is `attr_timeout` **plus a
     read/stat**, not a timer. Note 23 measured exactly this shape: the guest saw
     a host rewrite after ~6 s, not at 1 s, with the lever on.
- The kernel's I/O-mode doc is silent on per-open invalidation: `fuse-io.rst`
  documents direct-io / cached / write-through / writeback and stops there
  (`Documentation/filesystems/fuse/fuse-io.rst:7-45`,
  https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/filesystems/fuse/fuse-io.rst);
  `fuse.rst` mentions the page cache exactly once. So `FOPEN_KEEP_CACHE`'s
  contract lives in `fuse.h`'s one-line comment and in the code, not in the docs.

### 2.3 Does `always` require exclusive access? Yes, in virtiofsd's own words

- `gitlab.com/virtio-fs/virtiofsd` `src/passthrough/mod.rs:117-146`, the enum
  doc:

```
 117	/// The caching policy that the file system should report to the FUSE client. By default the FUSE
 118	/// protocol uses close-to-open consistency. This means that any cached contents of the file are
 119	/// invalidated the next time that file is opened.
 ...
 141	/// The client should always cache file data. This means that the FUSE client will not
 142	/// invalidate any cached data that was returned by the file system the last time the file was
 143	/// opened. This policy should only be selected when the file system has exclusive access to the
 144	/// directory.
 145	Always,
```

  (https://gitlab.com/virtio-fs/virtiofsd/-/raw/main/src/passthrough/mod.rs). The
  same file's `Config` doc repeats the precondition for `entry_timeout`,
  `attr_timeout` and `writeback` (`:271-299`), including the writeback sentence
  "can increase the risk of data corruption if the file contents can change
  without the knowledge of the FUSE client".
- The shipped CLI doc is thinner: `--cache <cache>` = "The caching policy the
  file system should use (auto, always, metadata, never). Default: auto."
  (`README.md:279-284`), and `--allow-mmap` "should only be enabled when the file
  system has exclusive access to the directory" (`README.md:286-292`,
  https://gitlab.com/virtio-fs/virtiofsd/-/raw/main/README.md) - i.e. the
  isolation of `Always` from mmap was deliberate.
- The canonical trade-off sentence is on the QEMU docs page:
  "`--cache=none|auto|always` Select the desired trade-off between coherency and
  performance. none forbids the FUSE client from caching to achieve best
  coherency at the cost of performance. **auto acts similar to NFS with a 1
  second metadata cache timeout. always sets a long cache lifetime at the expense
  of coherency.** The default is auto."
  (https://virtio-fs.gitlab.io/qemu/tools/virtiofsd.html - the 1 s figure is
  stale: both current implementations default to 5 s, note 13 §1.2.)
- **What you lose when the host changes a file under a cached guest**: nothing
  detects it. Neither implementation sends `FUSE_NOTIFY_INVAL_INODE`/`INVAL_ENTRY`
  (note 13 §1.2, `grep` count 0 in both servers). With `Always` the guest keeps
  serving the old bytes until attr expiry + a read/stat; for a pure `mmap`
  consumer, indefinitely. With writeback on top, `cache_mask` suppresses even
  that (kernel `inode.c:363`). #939 is the *perf* half of that trade; the
  correctness half is why upstream's enum carries the exclusive-access clause.
- muvm's shared root is explicitly the counter-example the issue names: "`Always`
  would need exclusive access to the directory, which a shared root such as
  muvm's (the live host `/`) doesn't have" - muvm runs "programs from your
  system" against that root (`README.md`,
  https://raw.githubusercontent.com/AsahiLinux/muvm/main/README.md).

## 3. Did muvm / oaktrout patch in response? No.

- `https://api.github.com/search/issues?q=repo:AsahiLinux/muvm+939` -> 0 results;
  `+is:pr+author:jgoyvaerts` -> 0; `+page+cache` -> two unrelated issues (#165,
  #116); `+cache+policy` -> only #131 ("Steam works on other ARM platforms with
  Box64 ..."), unrelated.
- No patch surface either: the repo tree (72 paths) contains no libkrun patches -
  `crates/krun-sys/build.rs` binds the **installed** library:
  `pkg_config::probe_library("libkrun")` then bindgen over `wrapper.h`
  (https://raw.githubusercontent.com/AsahiLinux/muvm/main/crates/krun-sys/build.rs).
  So muvm's cache behaviour is whatever the system `libkrun` ships, and with no
  policy in the C API there is nothing for muvm to set.
- The thread author is a muvm user (the report is muvm 0.7.0 + libkrun 1.19.6 on
  an M4 Pro), but the fix was filed upstream, not in muvm.

## 4. Do siblings/descendants ship `cache_policy = always`? None can.

Checked, with the API surface as the reason:

| project | how it reaches virtio-fs | can it select a policy? |
|---|---|---|
| krunkit | `krun_add_virtiofs4(ctx, tag, path, shm_size, read_only, semantics)` (`src/virtio.rs:64-71`, called at `:701-708` with `shm_size = 0`) | no - five arguments, no policy, and no DAX window either |
| podman machine (macOS) | via krunkit, which "is a tool to launch configurable virtual machines using the libkrun platform" (`krunkit/README.md`) | no, inherits krunkit |
| microsandbox | `msb_krun = "=0.1.41"` bindings crate (`Cargo.toml`), `vendor/libkrunfw` submodule only, no libkrun patch dir | only through the same C API - no policy |
| muvm | pkg-config `libkrun` (§3) | no |
| cang | own fork, Rust path dependency, `deps/libkrun` | **yes** - it already overrides the server (its own `Config`, its own `supported` set), which is why it can also hand out `KEEP_CACHE` by zero-message open |

Sources: https://raw.githubusercontent.com/containers/krunkit/main/src/virtio.rs,
https://raw.githubusercontent.com/containers/krunkit/main/README.md,
https://raw.githubusercontent.com/microsandbox/microsandbox/main/Cargo.toml,
`https://api.github.com/repos/microsandbox/microsandbox/git/trees/main?recursive=1`.
`crc` was not checked (it is not a libkrun consumer in any path I could confirm);
`msb_krun`'s internals were not read, only its version pin. **A GitHub-wide code
search for `CachePolicy::Always` in forks was attempted and could not be
completed** (grep.app returned HTTP 429 four times; code search needs auth), so
"ships `always` by default" is verified for the five consumers above and
**unverified for arbitrary forks** - the honest statement is that no consumer can
do it through the public API, not that no fork exists.

Also worth carrying: flipping the default is one line in the fork's enum
(`#[default]` on `CachePolicy::Auto` -> `Always`, note 13 §2.2), so "what
happened" for anyone who wanted it is "they would have had to carry a patch",
which is precisely the situation described in #924's code walk.

## 5. Verdict on #939's proposed fix (PR #940)

The patch, in full, is one new method plus three call sites
(https://github.com/libkrun/libkrun/pull/940.diff):

- `InodeData` gains `open_key: Mutex<Option<[i64; 5]>>` (mtime, mtime_nsec,
  ctime, ctime_nsec, size);
- `keep_cache_on_open(inode, file)` `stat()`s the just-opened fd, compares to the
  remembered key, stores the new one and returns "unchanged";
- `do_open` computes `keep_cache = matches!(cfg.cache_policy, Auto) &&
  !O_DIRECTORY && keep_cache_on_open(...)` and, in the `Auto` arm that used to
  emit nothing, sets `OpenOptions::KEEP_CACHE`.

### 5.1 Is it equivalent to `AUTO_INVAL_DATA`? No - it is earlier, and its trigger is different

| | PR #940 | kernel `AUTO_INVAL_DATA` |
|---|---|---|
| compared | mtime (+nsec), ctime (+nsec), size | mtime, size only (`inode.c:366-382`) |
| trigger | every `FUSE_OPEN` (server side, one `fstat`) | every `read(2)` and any stat/attr reply, but only after `attr_timeout` (`file.c:1126-1132`, `dir.c:1567`) |
| baseline | the host state at the inode's *previous open* (server memory) | the guest inode's cached attrs (which the guest itself updates on local writes) |
| `mmap` faults | covered on the next open | **not covered at all** - page faults bypass `fuse_cache_read_iter` |
| cost | one `stat()` + one `Mutex` on the open path | one `GETATTR` per attr_timeout per inode |
| latency of a host change becoming visible | immediately at the next open | up to `attr_timeout` (5 s in the fork) + a read/stat |

So the PR is a *tighter* freshness trigger than the auto-inval channel, on a
narrower set of file states (it can only act at an open). Given the choice
between the two, cang's fork has neither problem to solve: it already keeps the
cache unconditionally, and its freshness comes from `AUTO_INVAL_DATA` at 5 s
(fork `server.rs:1018` in `supported`), which note 23 measured at ~6 s.

### 5.2 Does it preserve close-to-open? For the open path, yes; not globally

- Preserved: a reopen after a host change computes a different key -> no
  `KEEP_CACHE` -> `invalidate_inode_pages2` at open (`file.c:296-297`). That is
  exactly close-to-open, which is what NFS-style sharing means: revalidate at
  open. Taking the host's ctime as well as mtime strengthens it over the kernel's
  own check (a `chmod` after a same-mtime rewrite is caught).
- Not preserved: an fd that stays open, or a mapping nobody reopens. Those keep
  old pages until attr expiry + a read/stat, and (mmap-only) indefinitely. The
  issue's own `--host-change` check passes only because it deliberately opens the
  file again before checking.
- Costs/risks the PR introduces, none of them mentioned in it: a `Mutex` lock per
  open on the inode (new contention on the hot open path of a parallel build - the
  same path note 23 deleted), one `stat()` per open, per-inode memory, and the
  `macos` backend left with the old behaviour.
- A hole worth naming if the fork ever carries this: the key is recorded at
  open, *before* the reads that populate the pages. A host write that lands
  between the `stat()` and the reads leaves mixed pages, and the *next* open
  fixes it - same class of window as everything else here, but it means "unchanged
  since the previous open" is not the same predicate as "the pages in memory are
  current".

### 5.3 Would it change anything for cang?

No. With the fork's zero-message open, `open()` returns `-ENOSYS` before
`do_open` (fork `passthrough.rs:1396-1398`), so `keep_cache_on_open` would never
run; the `create` reply (fork `passthrough.rs:1472-1478`) is untouched by the
patch. cang's share is therefore **already in the state #939 asks for**, and the
only remaining lever from the issue's family is the one it is not about:
invalidation (a server-side `FUSE_NOTIFY_INVAL_INODE` pushing host changes),
which exists in neither implementation (note 13 §1.2, §6).

## 6. What to quote when this map's decision is written up

- The mechanism, cang already has it: `fs/fuse/file.c:154-156` (no-open default =
  `KEEP_CACHE|CACHE_DIR`) vs `:296-297` (the invalidate that `Auto` produces).
- `Always`'s precondition is upstream's own: virtiofsd `passthrough/mod.rs:141-145`
  ("should only be selected when the file system has exclusive access"), which
  cang's live `/` share and muvm's live host `/` both fail.
- `Always` adds nothing here because the fork already emits, for free, what the
  flag would set; and because `Always` does not touch invalidation, which is the
  only remaining semantic difference.
- Upstream status, to avoid re-checking: #939 open/0 comments, #940 open/0
  reviews/1 commit/DCO-only, main unchanged since 2026-10-17, #924 answered
  "reasonable" by `slp` on 2026-10-14, no policy in any released header
  (`v1.19.7 include/libkrun.h:385-387`, `main include/libkrun.h:211-253`).

## 7. Not verified here

- Whether PR #940 was ever discussed off-thread (no comments, no reviews, no
  cross-references visible in the issue or PR JSON as of 2026-10-27).
- `msb_krun` internals, `crc`, and arbitrary libkrun forks (no code search).
- The macOS backend's behaviour (the PR leaves it untouched by design; the issue
  says it has the same `Auto` behaviour but the author cannot test there).
- Whether upstream would accept #940: no maintainer has answered; the only
  maintainer position on record (#924, 2026-10-14) is that an *API* to choose
  `cache_policy`/`writeback` is reasonable, which is a different change.
