# Ticket 01 - what resolves a pi extension's glibc addon libraries today

Resolved 2026-09-29 by the charting session, building on raw evidence left by a
research child that was killed mid-run (the host disk filled) and on direct
re-measurement. Evidence directories:

- `/home/dev/cang/disk/pi-addon-probe/raw/` - the killed child's host
  `LD_DEBUG` captures (node, sharp, onnxruntime).
- `/home/dev/cang/disk/pi-addon-probe/bwrap/` - this session's sandbox probes
  (the decisive ones).

## The loader condition, measured

ELF `DT_NEEDED` / `RUNPATH` (parsed with a small in-repo reader; `readelf` is not
installed on this host):

| object | NEEDED | RUNPATH |
|---|---|---|
| `pi` (`pi-coding-agent-0.85.1/lib/pi-coding-agent/pi`) | libc, ld-linux, libpthread, libdl, libm | **none** |
| `bun` (nixpkgs 1.4.2) | libc, ld-linux, libpthread, libdl, libm | **none** |
| `node` (nixpkgs slim 24) | ... libstdc++.so.6, libm, libgcc_s.so.1, ... | long, its own build inputs |
| `sharp-linux-x64-0.35.5.node` | libvips-cpp.so.8.18.7, **libstdc++.so.6**, libm, libgcc_s.so.1, libpthread, libc, ld-linux | `$ORIGIN/../../sharp-libvips-linux-x64/lib:...` |
| `libvips-cpp.so.8.18.7` | libresolv, libdl, **libstdc++.so.6**, libpthread, libm, libgcc_s.so.1, libc, ld-linux | `$ORIGIN/` |
| `onnxruntime_binding.node` | libonnxruntime.so.1, **libstdc++.so.6**, libm, libgcc_s.so.1, libc | `$ORIGIN/` |
| `libonnxruntime.so.1` | libdl, librt, libpthread, **libstdc++.so.6**, libm, libgcc_s.so.1, libc, ld-linux | `$ORIGIN` |
| host preload `/etc/ld-nix.so.preload` -> `malloc-provider-graphene-hardened/lib/libhardened_malloc.so` | **libstdc++.so.6, libgcc_s.so.1**, libc | glibc/lib : gcc-15.3.0-lib/lib |
| guest default preload (`nix/image/container.nix:99`) -> `mimalloc-3.3.2/lib/libmimalloc.so` | libpthread, librt, libatomic, libc | glibc/lib : gcc-15.3.0-lib/lib |
| cang `--alloc=hardened` lib (`nix/image/layers.nix` override of `graphene-hardened-malloc`, tag 14) | **libstdc++.so.6, libgcc_s.so.1**, libc | glibc/lib : gcc-15.3.0-lib/lib |

`libvips-cpp` and `libonnxruntime.so.1` resolve through the addons' own
`$ORIGIN` RUNPATHs. `libgcc_s.so.1` resolves from the loader's own default
search path (measured: `calling init: /nix/store/m07syxhld8hpprrdmzq565ziia6vlw9l-xgcc-15.3.0-libgcc/lib/libgcc_s.so.1`
while loading monty's addon, which NEEDs it). **The one library nothing in the
guest supplies is `libstdc++.so.6`.**

## Why the host works and the guest does not

The host's NixOS malloc provider, preloaded through
`/etc/ld-nix.so.preload`, itself has `DT_NEEDED libstdc++.so.6` and a RUNPATH
containing `gcc-15.3.0-lib/lib`. So every nix-linked process on the host loads
libstdc++ into the global scope at startup, and the addons' `NEEDED` is then
satisfied by an already-loaded soname. `LD_DEBUG=libs` shows exactly that
resolution (`raw/host-sharp-lddebug.txt`).

The guest's default preload is cang's `pkgs.mimalloc`, whose `DT_NEEDED` is
libpthread/librt/libatomic/libc - no libstdc++. Its RUNPATH does contain
`gcc-15.3.0-lib/lib`, but a preloaded object's RUNPATH is not what satisfies a
later `dlopen`'s NEEDED. The image Env sets no `LD_LIBRARY_PATH`
(`nix/image/config.nix`), and the `pi` binary has neither libstdc++ in NEEDED
nor a RUNPATH of its own.

## Reproduction in a host sandbox (the guest's loader conditions)

`bwrap --dev-bind / / --bind <empty-dir> /etc ...` drops the NixOS preload file
(`/etc/ld-nix.so.preload` is a symlink on NixOS, so it cannot be shadowed
directly), then `bun -e "require(MODULE)"` from
`~/.pi/agent/git/github.com/zeroqn/pi`:

| condition | sharp | onnxruntime-node |
|---|---|---|
| no preload (guest default library situation) | `Could not load the "sharp" module using the linux-x64 runtime` | `libstdc++.so.6: cannot open shared object file: No such file or directory` |
| `LD_PRELOAD=<mimalloc>` (exactly the guest default) | same failure | same failure |
| `LD_PRELOAD=<cang hardened_malloc-14>` (the guest's `--alloc=hardened`) | **LOAD_OK** | **LOAD_OK** |
| `LD_LIBRARY_PATH=<gcc-15.3.0-lib/lib>` | **LOAD_OK** | **LOAD_OK** |
| `LD_PRELOAD=<mimalloc> LD_LIBRARY_PATH=<gcc lib>` | **LOAD_OK** | **LOAD_OK** |

`node` is *not* a valid stand-in for `pi` here: nixpkgs' node links libstdc++
itself and carries a rich RUNPATH, so it loads both addons even with `/etc`
masked. Only `bun` (and therefore `pi`) exposes the failure.

Caveat, stated plainly: this is the guest's *loader condition* reproduced on the
host, with the guest's own allocator library, not the guest itself. A live guest
run is still owed (see the map's "Not yet specified").

## Other addons in the tree

Of the 8 linux `.node` addons under `~/.pi`, only sharp needs libstdc++:
`@mariozechner/clipboard-linux-*`, `@earendil-works/pi-tui .../linux-platform-x11.node`,
`@pydantic/monty-linux-x64-gnu` and `@yuuang/ffi-rs-linux-x64-gnu` are libgcc_s-only
(they load fine - monty verified LOAD_OK under the guest condition). The
onnxruntime binding is outside that glob but is the second libstdc++ consumer.

## Symptom

`require('onnxruntime-node')` throws at import time with the loader error above;
`sharp` throws its own generic module-load error. Whether magic-context catches
this and degrades quietly (both packages are `optionalDependencies`) was **not
established** - the bundle's `require`/`import` sites are not greppable in the
generated `index*.js`.

## What this means for the mechanism ticket

The thing to supply is the C++ runtime - one directory, `gcc-*-lib/lib`, or the
single soname `libstdc++.so.6`. Two mechanisms are already proven by the table
above: an environment path (`LD_LIBRARY_PATH`) and a preloaded library that
carries libstdc++ as its own dependency (which is what the host does, and what
`--alloc=hardened` would do in the guest).
