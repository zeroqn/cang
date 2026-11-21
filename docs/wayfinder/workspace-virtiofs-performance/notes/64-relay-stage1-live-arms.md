# Stage 1 of the reverse share in cang - what the live arms proved, and the two bugs they found

2026-11-21. Ticket 29 stage 1 (`--workspace-relay`): the FUSE relay as cang's own
code. This note records the state of that work and, more importantly, the two
defects the live arms exposed. **Nothing here was committed**: the change is
staged in the working tree and its validation (`cargo test`, clippy, the image
builds) is blocked - see "Blocked" at the end.

## What stage 1 adds (staged, not committed)

- `crates/cang-guest-relay` (static musl, shipped in the image): an `AF_VSOCK`
  listener serving `/workspace` with `fuse-backend-rs` passthrough, INIT
  advertising `FUSE_BIG_WRITES` + `FUSE_MAX_PAGES` (`max_write` 1 048 576,
  `max_pages` 256) from the 8-line patch in
  `crates/cang-guest-relay/vendor/fuse-backend-rs` (note 63 measured 10 -> 87 MB/s).
- `crates/cang-relay-pump` (host helper in `$out/libexec/cang-helpers/relay-pump`):
  `_FUSE_COMMFD` handshake with `fusermount3`, then a serial frame relay to the
  vsock socket; on transport EOF it answers `-EIO` and never unmounts (contract
  decision 4(a), note 63).
- `cang --workspace-relay` (requires `--workspace-disk`, refused by name
  otherwise): allocates the relay socket, registers guest port **50 430** in the
  launch's own port registry, passes `CANG_WORKSPACE_RELAY_PORT` /
  `CANG_WORKSPACE_RELAY_ROOT`, starts the pump on the launch directory, watches
  its state file (`starting`/`ready`/`error`), and unmounts at exit.

## Bug 1 (found live, fixed): the relay socket was unusable by the host

libkrun's muxer creates the acceptor socket from the **worker** identity, so it
appeared as `srwxr-xr-x 165536:165536` - and a `connect(2)` on a Unix socket needs
*write* permission. The pump got `EACCES` on every attempt:

```
relay-pump: no workspace relay answered on '/tmp/cang-1000/wr-<task>.sock'
within 120s: gave up after 2396 attempts and 120.0s; last error:
Permission denied (os error 13)
```

Fix (`crates/cang/src/runtime/vm/libkrun/launcher.rs`): after registering the
relay's unix port, the worker hands the socket to the host user
(`chown` to the identity `managed_ready` already repairs the attach/exec sockets
to, then `chmod 0600`), in a bounded thread that waits for the path. Only the
creating identity can `chown` it, so this has to happen inside the worker; cang's
own process cannot.

Proven live: the next launch's socket came up `srw------- dev dev`, the pump
connected, and the mount appeared:

```
$ findmnt /tmp/rl/wsA
TARGET      SOURCE         FSTYPE OPTIONS
/tmp/rl/wsA cang-workspace fuse   rw,nosuid,nodev,relatime,user_id=1000,group_id=993
```

## Bug 2 (found live, fixed, awaiting a build): the relay served the guest's `/`

`RelayServer::new(root_dir, config)` built `PassthroughFs::new(config)` and used
`root_dir` **only in its error message**. `fuse-backend-rs` takes the served tree
from `config.root_dir`, whose default is `/`, so the host mount showed the guest's
whole root filesystem instead of `/workspace`:

```
$ ls /tmp/rl/wsA
bin  dev  dri  etc  home  include  install.sh  lib ...      # the image root
$ ls /tmp/rl/wsA/workspace
a.txt  cargo  GUEST.txt                                      # the actual tree
```

Fix (`crates/cang-guest-relay/src/server.rs`): set `config.root_dir = root_dir`
before constructing the passthrough, with a comment saying why the error message
is not enough. Regression test added -
`the_relay_serves_the_root_it_was_given_and_not_the_guest_root`: a server rooted at
`<tmp>/workspace` must answer `LOOKUP inside.txt` with status 0 and
`LOOKUP outside.txt` (a sibling of the root) with `-ENOENT`. It fails with the
default root and passes with the fix.

## Blocked (2026-11-21, after the arms above)

Both live arms after that point are blocked by the host, not by the code:

1. **The image cannot be rebuilt.** `nix build .#container` evaluates
   `nix/image/container.nix:192` and throws
   `imageChecks.missingImageConfigNixDbRefs != [ ]` - "cang image config
   references store paths outside the generated image Nix DB metadata". Whether
   that list is non-empty *right now* is not yet established: evaluating it forces
   `pkgs.closureInfo { rootPaths = layers.imageContents; }`, and the nix daemon
   dies while realising it (`Nix crashed ... nix::Interrupted`). Those crashes are
   from interrupted builds (my cell timeouts and `pkill`), not from store
   corruption - `nix-store --verify` reports no deleted paths.
2. **The dev shell's toolchain was collected.** After
   `nix-collect-garbage --delete-old` (bob, 07:4x), `nix develop --command cargo`
   cannot get `cargo-1.99.1` / `rustc` back: every substituter times out
   (`nixpkgs-wayland.cachix.org`, `nix-community.cachix.org`,
   `cache.nixos.org` - all three unreachable from this host right now), so no
   `cargo test`, `clippy`, `fmt` or `deny` can run.

So: bug 1 is proven live; bug 2 is proven live and fixed but its regression test
and the post-fix arm have not run. The staged tree must not be committed until
those pass. (Both have since run - see "After the unblock" below.)

## After the unblock (2026-11-21, network back)

The dev shell re-materialised, and everything below ran:

- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo deny check` (advisories ok, bans ok, licenses ok, sources ok),
  `cargo test` (all crates, including the new
  `the_relay_serves_the_root_it_was_given_and_not_the_guest_root`) - all green;
- `nix build .#cang`, `.#cang-musl`, `.#container` - all built, and the image
  loaded. The `missingImageConfigNixDbRefs` throw from the blocked window does
  **not** reproduce: it was the collected store, not the change.

**The post-fix arm, live** (`/tmp/rl.raw`, a 256 MiB btrfs image seeded with a
git repo, `cang --workspace-disk /tmp/rl.raw --workspace-relay`):

```
findmnt /tmp/rl/wsC
/tmp/rl/wsC cang-workspace fuse rw,nosuid,nodev,relatime,user_id=1000,group_id=993
ls /tmp/rl/wsC        -> a.txt  cargo  from-guest.txt  GUEST.txt   # the workspace
cat /tmp/rl/wsC/from-guest.txt -> GUEST-NONCE-7b3                # written in the guest
```

The relay's own INIT, from the pump's log of a real launch:

```
relay-pump: INIT major=7 minor=33 flags=0x406020 max_write=1048576 max_pages=256 flags2=0x2
relay-pump: mounted '/tmp/rl/wsE' (fuse_fd=5) through "/run/wrappers/bin/fusermount3"
```

`0x406020` is `FUSE_BIG_WRITES | FUSE_MAX_PAGES | FUSE_READDIRPLUS`, so note 63's
vendored patch is live in the product path, not just in the spike.

**`git status --porcelain` on the same tree**, three runs each:

| arm | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| host, through the relay | 75 ms | 76 ms | 75 ms |
| guest-native on `/workspace` | 230 ms (cold) | 87 ms | **11 ms** |

75 ms is the product-relevant number for host tooling (pi's reads, an editor
save, a `git` command) and it is ~7x the warm guest-native cost - the transport's
price, not a defect.

### The `-EIO` arm: not observable in this shape, and that is a finding

Contract decision 4(a) says a dead serving guest leaves the mount in place
answering `-EIO`. Two arms were run to observe it:

1. `kill -9` of the vm worker (pid found in `cang internal libkrun-vm-worker-enter`):
   the host's very next `ls`/`cat` got **`ENOENT`**, because cang noticed the
   worker and tore the launch down, and the pump unmounted on the stop signal;
2. `pkill -x cang-guest-relay` inside the guest (`--root`, since the relay runs as
   root): the same - teardown, unmount, `ENOENT`.

So in stage 1 the mount never outlives the launch, and `-EIO` has no window to
appear in: it is the correct answer for a mount that outlives its task, which is
the workspace-server VM (ticket 29), not this flag. A first attempt at arm 2 also
showed why `pkill -f cang-guest-relay` must not be used: the pattern matches the
task's own shell, which kills itself and ends the launch early.

Stage 1 was committed as `e47250c6`.
