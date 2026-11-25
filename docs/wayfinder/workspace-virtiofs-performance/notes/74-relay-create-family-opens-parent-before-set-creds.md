# A directory create through the relay mount answered `EPERM`: the parent inode was opened inside `set_creds`

2026-11-25. Bob reported that after `4f5332b8` (the create-family uid/gid stamp,
which fixed a *file* create) a **file** create through the reverse-share mount
worked but a **directory** create still failed:

    $ mkdir d1           # in the launch directory, through the relay mount
    mkdir: cannot create directory 'd1': Operation not permitted

The stamp is not the cause, and it does cover `MKDIR` (9) and `SYMLINK` (6):
`stamp_create_identity` rewrites `fuse_in_header.uid`/`.gid` for all four
create-family opcodes (`crates/cang-guest-relay/src/server.rs:126-135`, set
`CREATE_FAMILY` at `:42`), and the
crate's unit test sends both CREATE and MKDIR as `4242:4243` and asserts the
entries exist. The failure is *inside* the pinned FUSE library, after the stamp.

## The mechanism

`fuse-backend-rs`'s passthrough runs the four create-family handlers as the
caller (`set_creds` -> `setresgid` + `setresuid`, `os/linux/credentials.rs:65-73`),
and an inode stored as a **file handle** is reopened by
`InodeHandle::get_file()` -> `OpenableFileHandle::open(O_PATH)` ->
`open_by_handle_at` (`os/linux/file_handle.rs:373-381`, `:302-313`).
`open_by_handle_at` requires `CAP_DAC_READ_SEARCH` **in the effective set**, and
changing euid away from root clears exactly that.

At the pinned revision (`506952b947cf4346d68550ebead4d244e2ad4a15`, which *is*
upstream `master` HEAD on 2026-11-24) the four handlers disagree about where the
parent is opened:

| handler | `get_file()` | line |
| --- | --- | --- |
| `create` | **before** the `set_creds` scope | `sync_io.rs:1023` |
| `mknod` | **before** | `:1393` |
| `mkdir` | inside the scope | `:890` |
| `symlink` | inside the scope | `:1456` |

So a file create works (its `get_file()` still has the capability) and a
directory or symlink create fails `EPERM` in the guest, which the host tool
prints as "Operation not permitted". Nothing else is involved: `write`, `unlink`,
`rename`, `setattr` never impersonate, which is why writing into an existing
file kept working.

## Why the VM-free suite cannot see it

`passthrough_config()` selects `inode_file_handles` from the process's own
`CapEff` (`crates/cang-guest-relay/src/server.rs:151-192`): the relay is guest
root and gets handles, while an unprivileged test process falls back to one
`O_PATH` descriptor per inode - `InodeHandle::File`, whose `get_file()` is a
no-op. The 31-test relay suite therefore passes both before and after the fix;
the fault needs a guest, a btrfs tree and the handle policy.

## The fix, and what carries it

A git dependency cannot carry a patch of upstream (the same reason the INIT
`BIG_WRITES`/`MAX_PAGES` limits are stated by the relay), so cang now pins a
fork: `https://github.com/zeroqn/fuse-backend-rs` branch `cang`, commit
`9c464cb0992dc271c477461656e2a8bd228458c1`, which is `506952b9` plus

    passthrough: open the parent before impersonating it

- `crates/cang-guest-relay/Cargo.toml`: `git`/`rev` -> the fork (the comment
  beside it records the mechanism);
- `Cargo.lock`: all six `fuse-backend-*` packages moved to the fork source;
- `deny.toml`: `allow-git` names the fork;
- `nix/pkgs/cang-rust.nix`: `cargoDeps` re-hashed to
  `sha256-VN38UzwqNx486hCBhOraRG2rvOV9nCQnwXTpPXIMTp4=`;
- `docs/usage.md`: the create-family paragraph no longer says a directory create
  through the mount is refused.

The vendored crate under `cargoDeps` was checked directly: its
`source-git-0/fuse-backend-passthrough-0.14.0/src/sync_io.rs` carries the hoisted
`get_file()` for both handlers (lines 884-894 and 1453-1463), so the built
`cang-guest-relay` is the patched code.

## Live readback (`.scratch/relaymkdir`, one guest per arm)

Host: 28 CPUs, uid:gid `1000:993`; workspace image `work.raw` (btrfs, created by
the first arm); launch directory `ws/` (the relay mount lands there), image
outside it so a command launch may carry a forced mount.

Arm 1 - **control**, the relay the *loaded image* carried (`x93xvnbw` 0.23.6,
booted via a fresh single-file `--guest-init` override):

    touch OK
    mkdir: cannot create directory 'd1': Operation not permitted
    ln: failed to create symbolic link 'link1': Operation not permitted
    cp: cannot create directory 'tree': Operation not permitted
    write-into-existing OK
    drwxr-xr-x 1 1000 993 96 cargo
    -rw-r--r-- 1 1000 993  0 file.txt

Arm 2 - the same probes with the image retagged to HEAD (`ybv3frr` 0.24.0, whose
`bin/cang-guest-relay` is the fork build):

    touch OK
    mkdir OK
    mkdir -p OK
    symlink OK
    cp -a OK
    drwxr-xr-x 1 1000 993   4 d1
    -rw-r--r-- 1 1000 993   0 file2.txt
    lrwxrwxrwx 1 1000 993   8 link1 -> file.txt
    drwxr-xr-x 1 1000 993  38 tree
    write-into-existing OK
    relay: inodes stored as file handles
    relay: ops 1:14 3:7 4:14 5:2 6:2 9:5 14:1 16:3 18:4 22:1 23:1 25:6 26:1 27:5 29:5 34:10 35:3 44:8

Both arms logged `relay: inodes stored as file handles`, i.e. the handle policy
that makes the fault reachable, and both dispatched the requests (`9:` = MKDIR,
`6:` = SYMLINK). Created entries are owned `1000:993` - the tree's owner, which is
what the stamp buys - so the hoist changed only whether the handler can open its
parent, not whose identity it creates as.

## The trap that produced a null first run

The first arm booted a fresh `--guest-init` and still saw `EPERM`: an override is
a **read-only override mount of that one file**
(`crates/cang/src/runtime/session/rootfs/guest_init.rs:17-33`,
`GuestInitOverrideMount`), so `sibling_binary()` still resolved the *image's*
`cang-guest-relay`. And `podman load` moved `localhost/cang:latest` only -
`cang:nixone` still named `3f2be435d370` whose entrypoint is
`x93xvnbw...-0.23.6` (`cang-live-image-and-guest-init-identity` §1). The arm driver
now prints the tag's `Config.Entrypoint` before launching, and `nixone` was
retagged to `8ce1ea10bf4c` (`ybv3frr` 0.24.0) for the second run.

## Limits

- VM-free: `cargo test -p cang-guest-relay` 31 passed, `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings` and
  `cargo deny check` (advisories/bans/licenses/sources ok) are green with the
  fork pin. None of them exercises the fault (see above).
- The published fix is a cang fork, not upstream: the hoist is upstream-consistent
  (`ScopedSuppGroups::new` already runs before `set_creds` for exactly this
  capability reason) and worth filing there, after which the pin can return to
  upstream `master`.
