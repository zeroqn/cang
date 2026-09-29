# Pi extension C++ runtime delivery

Status: accepted

Cang ships a **native addon runtime directory** that exposes exactly
`libstdc++.so.6`, and installs the `pi` binary through a wrapper that prepends
that directory to `LD_LIBRARY_PATH`.

`nix/pkgs/pi-coding-agent.nix` keeps `$out/lib/pi-coding-agent/pi` as the raw
bun binary and turns `$out/bin/pi` into the wrapper, so the guest (which takes
pi from the image's agent layer) and the development host (which takes
`pi-coding-agent` from the cang flake) receive the same delivery.

The directory is scoped to the single soname. `LD_LIBRARY_PATH` is searched
ahead of a binary's own `DT_RUNPATH`, so a directory carrying gcc's other
libraries would pre-empt the run paths of unrelated nix binaries.

The guest keeps `mimalloc` as its default allocator. `cang --alloc=hardened`
incidentally makes the same addons load, because that allocator library carries
`libstdc++` in its own `DT_NEEDED`; it is documented as a stopgap, not promoted
to the mechanism and not made the default.

## Context

A pi extension may install a native addon at runtime: magic-context 0.44.1 pulls
in `@img/sharp-linux-x64` and `onnxruntime-node`, whose bindings are prebuilt
glibc ELF objects that NEED `libstdc++.so.6`. Those packages are installed by pi
into `~/.pi`, which cang grafts from the host into the guest, so one unpatched
tree is loaded in both environments.

The extension host is the bun-compiled `pi` binary. Its `DT_NEEDED` set is
libc, ld-linux, libpthread, libdl and libm, and it has no `RUNPATH` at all, so
nothing in its own chain can supply a dependency of a library it `dlopen`s
later. A Nix environment has no `/lib`, no `/usr/lib` and no `ld.so.cache`, and
the loader's only default search path is its read-only store `lib` directory -
which holds glibc, and holds `libgcc_s` through the loader's own path, but never
`libstdc++`.

The development host appears to work: `LD_DEBUG=libs` shows `libstdc++.so.6`
resolving through the run path of the library NixOS preloads from
`/etc/ld-nix.so.preload`. That library, the malloc provider, NEEDs `libstdc++`
itself, so the soname is loaded into the global scope before any addon is
opened. The cang image preloads `pkgs.mimalloc` instead, whose NEEDED set is
libpthread, librt, libatomic and libc - so the accident does not reproduce in
the guest, and `require('onnxruntime-node')` fails there with
`libstdc++.so.6: cannot open shared object file`.

The measurement work behind this decision is in
`docs/wayfinder/pi-native-addons/notes/01-glibc-addon-resolution.md` and
`notes/02-loader-visible-compat-libs.md`.

## Consequences

Delivery rides on `mimalloc` being irrelevant: `--alloc=glibc` removes
`/etc/ld-nix.so.preload` without touching the wrapper, and the allocator policy
stops being load bearing for a loader problem.

The wrapper becomes the entry point that matters. Anything that invokes
`$out/lib/pi-coding-agent/pi` directly bypasses the mechanism, and the image's
`versionCheckHook` must keep running through `$out/bin/pi`.

Coverage follows pi's process tree: extensions, subagents, and every
`node`/`npm`/`bun` command the agent launches inherit the directory. A dynamic
process started from the guest task shell outside pi's tree is a known gap; the
image environment variable that would close it is deliberately not used, because
it would shadow the C++ runtime of every process in the guest without covering
the host.

A future addon needing a different soname - `libxcb` for the pi-tui X11 prebuild,
say - is not covered by this decision and needs its own.

The mechanism is verified in a real guest by a scored probe, and guarded in the
repository by a wiring check, so undoing the wrapper fails loudly.

## Considered options

- **`LD_LIBRARY_PATH` in the image Env**: covers every dynamic process in the
  guest, including a shell-started `node`, but reaches the host not at all and
  leaves the development host on its accidental mechanism.
- **An env-free `/etc` hook**: `/etc/ld.so.preload`, or a shipped
  `/etc/ld.so.cache` over a compat directory, would cover the guest without an
  inherited variable, and the loader in this environment does read both paths.
  Rejected because the hook is unprobed, reaches the guest only, and
  `/etc/ld.so.preload` loads the library ahead of every binary's own run path.
- **Extending the allocator's preload file**: `/etc/ld-nix.so.preload` is
  already written by the image and rewritten per `--alloc`, so adding
  `libstdc++.so.6` there looks free. Rejected because the allocator owns that
  file and `--alloc=glibc` deletes it, which would break the addons again.
- **Patching the addon tree**: rewriting the addons' `RUNPATH` with `patchelf`
  fixes the files themselves, and because `~/.pi` is shared it would fix both
  environments at once. Rejected as a mechanism because the tree is installed at
  runtime and would have to be re-patched after every install, mutating user
  state as a side effect of starting pi.
- **Making `hardened_malloc` the default allocator**: fixes the whole class for
  free in the guest, at the cost of an allocator policy chosen for performance
  and hardening reasons, and with the same fragility.
