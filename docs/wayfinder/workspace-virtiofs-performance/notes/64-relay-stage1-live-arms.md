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
those pass.
