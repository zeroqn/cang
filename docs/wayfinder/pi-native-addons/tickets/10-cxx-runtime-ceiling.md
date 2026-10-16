---
label: wayfinder:task
title: Keep the injected C++ runtime above every image consumer, and the preloaded allocator safe before initialisation
status: closed
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: pi session (2026-10-16)
---

## Question

Ticket 05 fixes the *missing* soname. It does not bound the *version* of the one
file the wrapper injects, and the same file is a ceiling for the whole process
tree, because `LD_LIBRARY_PATH` is searched ahead of every binary's own
`DT_RUNPATH`. The preloaded allocator has a second, independent hazard: it is
called before its own initialisation - ld.so runs a preload's constructor after
the binary's dependency initialisers - and `zg`'s libstdc++ static initialiser
runs in that window.

Both surfaced together on 2026-10-16 as "`zg` does not start in the guest":

- `zg` (zvec-grep 0.3.0, built with nixpkgs-unstable) links an ONNX Runtime built
  against `GLIBCXX_3.4.35/36`, while the runtime directory carried
  `pkgs.stdenv.cc.cc.lib` - the 26.05 input's gcc-15, whose libstdc++ stops at
  `GLIBCXX_3.4.34`. The loader failed with `version 'GLIBCXX_3.4.35' not found
  (required by libonnxruntime.so.1)`; under `fish` the same fault reads
  `'zg --version' terminated by signal SIGSEGV (Address boundary error)`.
- With the loader satisfied, `zg` still died with a SIGSEGV before `main` while
  the image preloaded mimalloc 3.3.2: under the glibc 2.44 that `zg` links (the
  unstable input's, newer than the 2.42 the preloaded object was built against)
  the 3.3.2 object faults inside libstdc++'s static initialiser, before the
  preload's own constructor has run.

## Resolution (2026-10-16)

Two changes, both mechanisms rather than symptoms:

- `flake.nix` passes nixpkgs-unstable's gcc
  (`zgPkgs.stdenv.cc.cc.lib`) as `nativeAddonCxxRuntime` into
  `nix/pkgs/pi-coding-agent.nix`, so the injected file is the newest C++ runtime
  any consumer in the image needs. libstdc++ keeps its ABI backward compatible,
  so the newer file still serves an addon built against an older one.
- `nix/image/layers.nix` builds the preloaded mimalloc from the same nixpkgs at
  3.5.3, which does not fault in that window, keeping the preloaded object on this
  image's glibc all the same.

Evidence: the host matrix in `notes/10-raw-host-zg-matrix.txt`, run in a mount
namespace with `/etc/ld-nix.so.preload` masked over `/dev/null`, is decisive on
both halves - only the fixed combination starts. The allocator half has a
two-line reproduction of its own in `notes/10-raw-preload-early-call.md`
(`t_free` = `free(NULL)`, `t_locale` = `std::locale` only): only the
glibc-2.44+libstdc+++mimalloc-3.3.2 arm faults, and 3.5.3 is clean in it:

| allocator preload | injected libstdc++ | `zg --version` |
| --- | --- | --- |
| none | none | rc=0 `0.3.0` |
| mimalloc 3.5.3 | none | rc=0 `0.3.0` |
| mimalloc 3.3.2 | none | **rc=139 SIGSEGV** |
| none | 26.05 gcc-15 | rc=1 `GLIBCXX_3.4.35 not found` |
| none | unstable gcc-16 | rc=0 `0.3.0` |
| 3.3.2 | 26.05 gcc-15 | rc=1 (loader fails first) |
| 3.5.3 | unstable gcc-16 | **rc=0 (the fixed image)** |
| 3.5.3 | 26.05 gcc-15 | rc=1 (ceiling still too low) |
| 3.3.2 | unstable gcc-16 | rc=139 SIGSEGV |

The same four conditions were then reproduced **live in a guest** booted from the
fixed image (one bounded VM per arm): the guest's preload resolves to mimalloc
3.5.3 and `zg --version` prints `0.3.0`, `zg` run from the wrapper's own
environment prints `0.3.0`, `pi --version` still starts, and in that same guest
the pre-fix allocator alone gives `Segmentation fault` (`D1_rc=139`) while the
pre-fix gcc-15 libstdc++ alone gives `GLIBCXX_3.4.35/36 not found` (`D2_rc=1`)
and the fixed combination and both single-arm controls give `0.3.0`. Raw logs
and the runner: `notes/10-live-guest-arms.md`.

Guards (both in `container-wrapper-contracts`): run the image's own `zg` under
`LD_PRELOAD=${layers.mimallocLib}`, and under
`LD_LIBRARY_PATH=${nativeAddonRuntimeDir}/lib`. A repository test
(`image_preloads_a_mimalloc_that_survives_early_initialisation`) pins the
override's version, tag and hash, and ADR 0009 records the ceiling rule.
