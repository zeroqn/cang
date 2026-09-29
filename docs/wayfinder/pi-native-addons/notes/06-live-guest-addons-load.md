# Ticket 06 - both addons load inside a real cang guest, and the ticket-05 wrapper is why

Status: **RESOLVED.** Both of a pi extension's glibc addons (`sharp`,
`onnxruntime-node`) load inside a real cang microVM booted from this tree's
image, under the guest's **default** allocator with **no hand-set
`LD_LIBRARY_PATH`**, and the paired control shows the ticket-05 wrapper is the
cause: the same `bun` invocation fails both addons when the wrapper's variable
is absent and succeeds when it is present. The view is rounded out by the
`LD_LIBRARY_PATH` of a live `pi` process, `LD_DEBUG=libs`, and the guest's
`/etc/ld-nix.so.preload`.

## Answer

The image is `localhost/cang:latest`, digest
`sha256:923d645e8e212064e3fc1f75973efd6291611070e04fef3300b0da3bd3ca8249`
(config id `78df820f0812`), loaded from the tar this tree builds
(`/nix/store/l28k4y2fqhhm68l7hhp0gzj0wvv5khy3-cang.tar.gz`). The guest ran
kernel `6.12.109-hardened1` (x86_64), `--alloc` omitted (default `mimalloc`).

**a. Which `pi` the image contains.** Guest `command -v pi` =
`/nix/store/9ha2nyk83f6wxdaxvvd84rdpgh6mgd68-cang-agent-layer/bin/pi`, a symlink
to `/nix/store/nwvcnfx3bma7h97gzhh79msr3g4jswww-pi-coding-agent-0.87.1/bin/pi`,
which is the ticket-05 **`makeWrapper` script** (not the raw ELF; the raw binary
stays at `.../lib/pi-coding-agent/pi`). It prepends
`/nix/store/0hpv152zh95hv36a8ksi0iywicgaphpn-cang-native-addon-runtime/lib` to
`LD_LIBRARY_PATH`; that directory's only entry is
`libstdc++.so.6 -> /nix/store/2ga5nd1m56n5cx2wh8vbf6nrdhqk2f0q-gcc-15.3.0-lib/lib/libstdc++.so.6`.

**b/c. Addon outcomes and the live `pi` process** (guest, default allocator):

| probe case | `sharp` | `onnxruntime-node` |
|---|---|---|
| plain `bun`, ambient `LD_LIBRARY_PATH` unset (**control**) | **LOAD_FAIL** | **LOAD_FAIL** |
| `bun` with exactly the `LD_LIBRARY_PATH` the wrapper exports | **LOAD_OK** | **LOAD_OK** |
| plain `node` (the known trap - not evidence) | LOAD_OK | LOAD_OK |

The failure verbatim is
`libstdc++.so.6: cannot open shared object file: No such file or directory`.

A **live `pi` process** (pid 957) was captured: its `/proc/957/environ` carries
`LD_LIBRARY_PATH=/nix/store/0hpv152zh95hv36a8ksi0iywicgaphpn-cang-native-addon-runtime/lib`
and its cmdline is `.../lib/pi-coding-agent/pi` - i.e. a real `pi` started
through the wrapper really does hand the runtime directory to the process that
`dlopen`s an extension's addon. The wrapper's value was also derived
mechanically (its final `exec` rewritten to `echo`), and it matches.

**d. Loader facts.** `/etc/ld-nix.so.preload` is the single line
`/nix/store/l1hlc2dd721m61pblg8rlp8xalfmay1w-mimalloc-3.3.2/lib/libmimalloc.so`
- cang's default allocator, so nothing else supplies `libstdc++.so.6`.
There is no `/etc/ld.so.cache`, no `/lib/libstdc++.so.6`, no
`/usr/lib/libstdc++.so.6`. `LD_DEBUG=libs` under the wrapper's environment shows
the search reaching the addon's own `RUNPATH` chain and then resolving at:

```
trying file=/nix/store/0hpv152...-cang-native-addon-runtime/lib/libstdc++.so.6
calling init: /nix/store/0hpv152...-cang-native-addon-runtime/lib/libstdc++.so.6
```

while the control (plain `bun`) searches the same chain plus the loader's system
path (`glibc-2.42/lib`, `xgcc-15.3.0-libgcc/lib`) and
errors: `libstdc++.so.6: cannot open shared object file`.

**e. The `node` trap.** Plain `node` loads both addons even with no
`LD_LIBRARY_PATH`, because nixpkgs' node carries a `DT_RUNPATH` that reaches the
image's `gcc-15.3.0-lib/lib`. It is recorded here only so a passing `node` is
never mistaken for evidence of the fix; the measurement is `bun`, the runtime
`pi` is built with.

## Why this is causal, not incidental

- The only difference between the two `bun` rows is the value the **wrapper
  exports**; the allocator, the image, the module tree and the boot are
  identical. Ticket 04 measured both rows as LOAD_FAIL pre-fix with the same
  image family; ticket 05's wrapper is the only change.
- The wrapper's value is the one a live `pi` process actually carries
  (`/proc/957/environ`), not a guessed string.
- `LD_DEBUG` shows `libstdc++.so.6` resolving *in the runtime directory* on the
  wrapper path, and nowhere on the control path.
- `/etc/ld-nix.so.preload` is still mimalloc, so the success is not the
  `--alloc=hardened` accident that ticket 04 documented.

## Inputs

- repo HEAD at image build: `4f950c4` (+ a dirty working tree; the only
  non-doc change, `nix/image/checks.nix`, was committed mid-run as `927a21b`).
  The VM launch log recorded HEAD `60cb5a6` (a later docs commit). Neither
  `927a21b` nor `60cb5a6` touches `nix/pkgs/pi-coding-agent.nix`,
  `nix/image/layers.nix` or `flake.nix`, and the wrapped pi store path
  `nwvcnfx3...` is byte-identical to ticket 04's labelled post-fix run, so the
  measured image **is** the current tree's image content.
- ticket-05 fix commit present: `1d8272d` ("pi: wrap the pi binary with a native
  addon runtime directory").
- `nix build .#container .#cang .#cang-musl` (first attempt, exit 0):
  - image tar `/nix/store/l28k4y2fqhhm68l7hhp0gzj0wvv5khy3-cang.tar.gz`
  - cang `/nix/store/22i0vnx2672phbgvgwjp11rkc76x1z69-cang-0.10.1`
  - guest-init `/nix/store/1j99c1xhp2jffvi5xphzric6708skzqq-cang-static-x86_64-unknown-linux-musl-0.10.1/bin/cang-guest-init`
- image loaded into a hermetic btrfs-backed podman store under
  `/home/dev/cang/disk/pi-native-addons-06/container-storage` (never the ambient
  `~/.config/containers/storage.conf`), referenced as `localhost/cang:latest`.
- cang state/config isolated under
  `/home/dev/cang/disk/pi-native-addons-06/{state,config}` with
  `[task-rootfs] backend = "btrfs-snapshot"`.

Exact launch (also `06-raw-launch-and-image.txt`):

```
cd /home/dev/cang/disk/pi-native-addons-06/workspace
. /home/dev/cang/disk/pi-native-addons-06/env.sh       # hermetic CONTAINERS_STORAGE_CONF/TMPDIR/XDG_*
CANG_IMAGE=localhost/cang:latest script -q -e -c "/nix/store/22i0...-cang-0.10.1/bin/cang --mem 4   --seccomp=off --landlock=off   --guest-init /nix/store/1j99c...-cang-static-...-musl-0.10.1/bin/cang-guest-init   -- sh /workspace/probe.sh" /dev/null
```

`vm-exit=0`. No `--alloc` flag, so the guest's default mimalloc allocator and
`/etc/ld-nix.so.preload` are in effect.

## Harness

The probe is `workspace/probe.sh`, driven by `run-vm.sh`; the workspace is
mounted at `/workspace` and the addons probed are the host's grafted `~/.pi`
tree (`/home/dev/.pi/agent/git/github.com/zeroqn/pi`), the same tree a pi
extension loads in the guest. The probe emits `evidence/probe.txt` plus per-case
`.out`/`.err`, `lddebug-*.txt` and `verdict-*.txt`.

One harness bug was fixed before the recorded run: the case tag was built with
`echo ... | tr -c 'a-zA-Z0-9' '_'`, which converted `echo`'s trailing newline
into a trailing `_`, so the summary and `LD_DEBUG` filenames never matched and
those sections printed empty. Switching to `printf '%s'` and aligning the
references (see `probe.sh`, kept in place; the pre-fix copy is
`workspace/probe.sh.pre-fix`) produced the self-contained run recorded here.

## Limits / honesty

- The probe drives `bun -e "require(...)"` from the pi package tree - the same
  `dlopen` resolution path a pi extension uses - rather than calling into a pi
  session's extension loader. The wrapper's own variable is independently
  confirmed on a live `pi` process, so the two together cover the claim.
- x86_64 only (`@img/sharp-linux-x64`, onnxruntime `linux/x64`).
- The wrapper reaches pi's own process tree; a dynamic process started from the
  guest task shell outside it is the residual gap the map already records.

## Raw evidence (beside this file)

- `06-raw-launch-and-image.txt` - the verbatim `logs/launch.txt` (HEAD, digests,
  exact launch, `vm-exit=0`), the hermetic storage.conf and the isolated
  `cang.toml`.
- `06-raw-guest-probe.txt` - the complete `workspace/evidence/probe.txt` from
  this run (pi store path, wrapper text, wrapper-exported and live-pi
  `LD_LIBRARY_PATH`, `/etc/ld-nix.so.preload`, all six cases, `LD_DEBUG`,
  summary).
- `06-raw-guest-module-outcomes.txt` - the six probe cases verbatim.
- `06-raw-guest-lddebug-libstdc.txt` - `LD_DEBUG=libs` libstdc++.so.6 search for
  the wrapper and control environments, both modules.
- `06-raw-nix-build-and-pi-mapping.txt` - build outputs, the store's pi
  derivations, the wrapped `bin/pi`, the runtime directory, and the ticket-05
  commit.
