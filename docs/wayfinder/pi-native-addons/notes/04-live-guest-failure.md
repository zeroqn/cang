# Ticket 04 - the addon failure inside a real cang guest

Status: RESOLVED - the failure ticket 01 predicted is confirmed on the real
target, with the allocator control that ticket 01's host sandbox implied.

## Answer

Inside a real cang microVM booted from this tree, with the guest's default
allocator (`/etc/ld-nix.so.preload` -> cang's `pkgs.mimalloc`):

| runner | module | outcome |
|---|---|---|
| `bun` (the `pi` runtime) | `sharp` | **FAIL** - `Could not load the "sharp" module using the linux-x64 runtime` / `ERR_DLOPEN_FAILED: libstdc++.so.6: cannot open shared object file: No such file or directory` |
| `bun` | `onnxruntime-node` | **FAIL** - `libstdc++.so.6: cannot open shared object file: No such file or directory` |
| `node` (control) | `sharp` | LOAD_OK |
| `node` (control) | `onnxruntime-node` | LOAD_OK |

And with `--alloc hardened` (the guest preloads the GrapheneOS hardened_malloc,
which itself NEEDs `libstdc++.so.6`):

| runner | module | outcome |
|---|---|---|
| `bun` | `sharp` | **LOAD_OK** |
| `bun` | `onnxruntime-node` | **LOAD_OK** |

The mimalloc failure reproduced identically in two separate boots. `node` loads
both addons even under mimalloc because nixpkgs' node carries a long `DT_RUNPATH`
that reaches `gcc-15.3.0-lib/lib`; `bun`/`pi` carry no RUNPATH at all (ticket 01).

This is exactly ticket 01's host-sandbox table, now measured in the guest:

| guest condition | sharp | onnxruntime-node |
|---|---|---|
| default (`pkgs.mimalloc` preload) | FAIL | FAIL |
| `--alloc hardened` (`libhardened_malloc.so` preload) | LOAD_OK | LOAD_OK |

## The image build - and a build finding

`nix build .#container .#cang .#cang-musl --no-link --print-out-paths` failed on
the first attempt, twice over (verbatim in
`04-raw-nix-build-attempt1.txt`; full logs at
`/home/dev/cang/disk/pi-native-addons-04/logs/nix-build-attempt1.*`):

1. A flaky `cang-guest-init` test in the musl package's checkPhase:
   `guest_init::components::podman::service::tests::rootless_info_verification_preserves_failure_stderr`
   panicked at `crates/cang-guest-init/src/.../podman/service_tests.rs:61:5`
   (`assertion failed: err.to_string().contains("idmap setup failed")`), 297
   passed / 1 failed, failing
   `cang-static-x86_64-unknown-linux-musl-0.10.1.drv`.
2. A substitution failure from the USTC mirror
   (`... nar.zst: HTTP error 416`) propagated into the image config nix-db refs
   check: `missingImageConfigNixDbRefs` non-empty at `nix/image/checks.nix:397`
   -> `builtins.throw` at `nix/image/container.nix:170`.

A plain retry rebuilt the same musl derivation and its suite passed, so the
podman test is **flaky, not deterministic**. The retry produced the whole set in
~4.5 minutes because most of the store was already substituted. Both logs are
kept. This is a real friction point for a cold `.#container` build, exactly as
the ticket's risk list warned; it is not caused by anything this ticket did.

## Inputs

- repo HEAD at image-build time: `fd8c0a1449aa32969f6b315fb456d303768df10d`
- `nix build .#container .#cang .#cang-musl`:
  - image archive `/nix/store/2jzzdyj4ckpmmlwjswvmgr7z1hymm08i-cang.tar.gz`
  - cang `/nix/store/cj14gd195wvigrrkipadx7iilq30g8w7-cang-0.10.1` (`cang 0.10.1`)
  - guest-init `/nix/store/qw5cm13a0xc6mdlqyvk2338lh7w6f86z-cang-static-x86_64-unknown-linux-musl-0.10.1/bin/cang-guest-init`
- image loaded into a hermetic, btrfs-backed podman store under
  `/home/dev/cang/disk/pi-native-addons-04/container-storage` (never the ambient
  `~/.config/containers/storage.conf`), then referenced as `localhost/cang:latest`:
  - **digest `sha256:b821b52eb4a1cc88d7a4f94512b87bc0debe75255b14fd729b1f537599dfcb9d`**
  - config id `c7cc57913b744916f1c6e9696c338e3edfde9d2d564feb78e933c77035667ef5`
  - `Config.Entrypoint = [".../cang-static-...-musl-0.10.1/bin/cang-guest-init","enter","--"]`
- cang state: `/home/dev/cang/disk/pi-native-addons-04/state` with
  `[task-rootfs] backend = "btrfs-snapshot"`.

## Which `pi` the measured image carries (pre-fix, labelled)

This measurement is the **pre-fix baseline**. The image's agent layer resolves
`pi` to the **unwrapped** derivation - no wrapper, no `nativeAddonRuntimeDir`:

- guest `command -v pi` =
  `/nix/store/p9lzng046snafrmv9xakmd8dl90wd474-cang-agent-layer/bin/pi`
- -> `/nix/store/yj15wxdjj7vba7z1cwki520qaswyrywg-pi-coding-agent-0.87.1/bin/pi`,
  which is a **symlink** `-> ../lib/pi-coding-agent/pi` (the raw bun ELF binary);
  nothing in that output references `cang-native-addon-runtime` (0 hits).
- produced by
  `/nix/store/5055xiwdkvdh6s1fvni5bmcy4zgh62aa-pi-coding-agent-0.87.1.drv`, the
  derivation nix built during this ticket's run.

The ticket-05 fix's outputs are **not** in this image:
`/nix/store/nwvcnfx3bma7h97gzhh79msr3g4jswww-pi-coding-agent-0.87.1` (makeWrapper
`--prefix LD_LIBRARY_PATH .../cang-native-addon-runtime/lib`) and the
installPhase-time wrapper
`/nix/store/0ik8k841n595v9kvif0rfdrr8w5486jn-pi-coding-agent-0.87.1` (its `bin/pi`
execs `... pi "installPhase"`). Full evidence: `04-raw-pi-store-path.txt`.

Consequence: the wrapper only affects processes *started as* `pi`, so a probe
that invokes `bun` directly (as this one does) reports the same FAIL even against
a fixed image. A post-fix confirmation has to exercise the `pi` wrapper's
`LD_LIBRARY_PATH`, not `bun`.

The exact launch, three times, all `vm-exit=0` (~28 s each):

```
cd /home/dev/cang/disk/pi-native-addons-04/workspace
CONTAINERS_STORAGE_CONF=.../container-storage/storage.conf \
XDG_CONFIG_HOME=.../config XDG_STATE_HOME=.../state CANG_IMAGE=localhost/cang:latest \
script -q -e -c "/nix/store/cj14...-cang-0.10.1/bin/cang --mem 4 --alloc <mimalloc|hardened> \
  --seccomp=off --landlock=off \
  --guest-init /nix/store/qw5c...-cang-static-...-musl-0.10.1/bin/cang-guest-init \
  -- sh /workspace/probe.sh" /dev/null
```

(`--guest-init` is needed because the tree's guest-init is the musl static one;
the image's entrypoint names the same store path. Host LSM layers are off exactly
as the house recipe does; the guest allocator is what varies.)

## The guest environment at measurement time

From `04-raw-guest-probe-mimalloc.txt` / `...-hardened.txt`:

- `uid=1000(dev) gid=993(dev) groups=...video,render`, `HOME=/home/dev`,
  kernel `6.12.109-hardened1`, cwd `/workspace`.
- `bun 1.4.2` (tooling layer), `node v24.21.0` (dynamic toolchain layer),
  `pi 0.87.1` (agent layer), `sh` = bash-interactive-5.3p9.
- `~/.pi` is the grafted host tree - the addons probed are the host's:
  `/home/dev/.pi/agent/git/github.com/zeroqn/pi/node_modules/@img/sharp-linux-x64/lib/sharp-linux-x64-0.35.5.node`
  and `.../onnxruntime-node/bin/napi-v6/linux/x64/onnxruntime_binding.node`.
- `/etc/ld-nix.so.preload` (default): the single line
  `/nix/store/l1hlc2dd721m61pblg8rlp8xalfmay1w-mimalloc-3.3.2/lib/libmimalloc.so`.
  Under `--alloc hardened` it is
  `/nix/store/mpkgijfbcl7i2p7gw1sphwrlp4hnwprd-graphene-hardened-malloc-14/lib/libhardened_malloc.so`.
- No `/etc/ld.so.cache` (`ls: cannot access ... No such file or directory`).
- The C++ runtime **is** in the image:
  `/nix/store/2ga5nd1m56n5cx2wh8vbf6nrdhqk2f0q-gcc-15.3.0-lib/lib/libstdc++.so.6 -> libstdc++.so.6.0.34`.
  Nothing puts that directory on the loader's path.

## LD_DEBUG=libs: how libstdc++.so.6 is sought

Default allocator (`04-raw-guest-lddebug-libstdc-mimalloc.txt`,
`04-raw-guest-lddebug-bun-sharp-mimalloc.txt`). The addon is dlopen'd last, so the
loader searches the addon's own `$ORIGIN` runpath chain first, then the system
default path - and `libstdc++.so.6` is nowhere:

```
find library=libstdc++.so.6 [0]; searching
  search path=<the sharp addon's RPATH: @img/sharp-libvips-linux-x64/lib and its
               glibc-hwcaps variants>   (RPATH from file .../sharp-linux-x64-0.35.5.node)
  ... trying file=.../sharp-libvips-linux-x64/lib/libstdc++.so.6
  search cache=/nix/store/lm3pk...-glibc-2.42-84/etc/ld.so.cache
  search path=/nix/store/lm3pk...-glibc-2.42-84/lib:...:/nix/store/m07sy...-xgcc-15.3.0-libgcc/lib
              (system search path)
  trying file=/nix/store/lm3pk...-glibc-2.42-84/lib/libstdc++.so.6
  trying file=/nix/store/m07sy...-xgcc-15.3.0-libgcc/lib/libstdc++.so.6
  ...
-> error: libstdc++.so.6: cannot open shared object file: No such file or directory
```

The loader also printed the preload's own RUNPATH, which *does* contain
`gcc-15.3.0-lib/lib`:

```
search path=...:/nix/store/2ga5nd...-gcc-15.3.0-lib/lib   (RUNPATH from file .../libmimalloc.so)
```

but this is used only for the preload's own `DT_NEEDED` list, which for mimalloc
is `libpthread/librt/libatomic/libc` - no libstdc++. It does not satisfy a later
`dlopen`'s NEEDED, exactly as ticket 01 argued. `onnxruntime_binding.node` is
worse off still: its `$ORIGIN` has only `libonnxruntime.so.1`, so the search goes
straight to the system path.

With `--alloc hardened` (`04-raw-guest-lddebug-libstdc-hardened.txt`) the same
lookup succeeds, because `libhardened_malloc.so` NEEDs `libstdc++.so.6` and its
RUNPATH reaches `gcc-15.3.0-lib/lib`, so libstdc++ is in the global scope before
the addon is opened:

```
find library=libstdc++.so.6 [0]; searching
  ... trying file=/nix/store/2ga5nd...-gcc-15.3.0-lib/lib/libstdc++.so.6
  calling init: /nix/store/2ga5nd...-gcc-15.3.0-lib/lib/libstdc++.so.6
```

and `bun -e '.../proc/self/maps'` in the hardened guest already shows
`libstdc++.so.6.0.34` mapped at process start, while the mimalloc guest's maps
show only `libmimalloc.so.3.3`, `libatomic.so.1.2.0` and no libstdc++.

This is the same mechanism that makes the NixOS host work today
(`/etc/ld-nix.so.preload` -> a hardened malloc that NEEDs libstdc++), now
demonstrated on the real target.

## Refinement to ticket 01's loader picture

Ticket 01 said the guest has "no `/lib`, no `/usr/lib`". The live guest has both
as directories, but they do not change the conclusion:

- `/lib` is a large compatibility/symlink farm (bash, clang, `libEGL_mesa`, dri,
  gbm, musl stubs, ...) and **`/lib/libstdc++.so.6` does not exist**.
- `/usr/lib` holds only `cang-fontconfig`, `cang-mesa-runtime`,
  `cang-software-renderer`; **`/usr/lib/libstdc++.so.6` does not exist**.
- Neither appears in the loader's `system search path` above, which is the
  loader's own store `lib` plus `xgcc-15.3.0-libgcc`.

So the operative fact stands: nothing on the loader's default path supplies
`libstdc++.so.6` under the default (mimalloc) allocator.

## Caveats

- **Shared, mutable checkout.** The repo is shared with another session that
  committed docs (ADR 0009 etc.) and then began editing
  `nix/pkgs/pi-coding-agent.nix` (a `makeWrapper`/`LD_LIBRARY_PATH` native-addon
  runtime dir) at 21:58, *after* this image and these binaries were realised
  (21:54) and before the VM runs (22:00-22:02), and committed the fix as
  `1d8272d` ("pi: wrap the pi binary with a native addon runtime directory") at
  22:05:51. The measurement is therefore of the **pre-fix** tree - which is what
  the ticket asked for. The delta `fd8c0a1..ea2faf7` was docs-only; the later
  `1d8272d` changes only `nix/pkgs/pi-coding-agent.nix`, which postdates the
  realised image. See the header of `04-raw-launch-and-image.txt`.
- The probe drives `bun -e "require(...)"` from the pi tree's `node_modules`,
  the same resolution path a pi extension uses. It does not launch the `pi`
  binary itself (that needs a TTY/session); the addon load is the measurement.
- Only the last mimalloc boot's (run 3) full `LD_DEBUG` stderr was kept; the hardened
  run's LD_DEBUG evidence is the filtered (verbatim) excerpt in
  `04-raw-guest-probe-hardened.txt` and
  `04-raw-guest-lddebug-libstdc-hardened.txt`.

## Raw evidence (beside this file)

- `04-raw-launch-and-image.txt` - HEAD, nix outputs, image digest, storage
  config, all three launch commands and exit codes.
- `04-raw-nix-build-attempt1.txt` - the verbatim build failure (flaky test +
  mirror 416 + image nix-db refs throw).
- `04-raw-guest-probe-mimalloc.txt` - full guest probe, default allocator.
- `04-raw-guest-probe-mimalloc-repeat.txt` - the same probe in a second boot.
- `04-raw-guest-probe-hardened.txt` - full guest probe, `--alloc hardened`.
- `04-raw-guest-module-outcomes.txt` - the four `require()` stdouts verbatim.
- `04-raw-guest-lddebug-bun-sharp-mimalloc.txt` - full `LD_DEBUG=libs` stderr
  for the failing `bun` sharp load.
- `04-raw-guest-lddebug-bun-onnxruntime-mimalloc.txt` - the same for
  onnxruntime-node.
- `04-raw-guest-lddebug-libstdc-mimalloc.txt` - the libstdc++ search lines plus
  the preloaded mimalloc RUNPATH.
- `04-raw-guest-lddebug-libstdc-hardened.txt` - the hardened sections.

Working copies of everything (logs, evidence dirs, storage, state) stay under
`/home/dev/cang/disk/pi-native-addons-04/`.
