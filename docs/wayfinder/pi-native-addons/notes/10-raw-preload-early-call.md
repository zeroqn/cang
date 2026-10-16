# Raw evidence: the preloaded allocator is called before its own initialisation

Session: pi, 2026-10-16, host `dev` (NixOS, glibc 2.42 runtime via the 26.05
input; the unstable input brings glibc 2.44, which `zg` links).
Harness: `notes/10-raw-preload-early-call.sh` (self-contained; write the two
sources, compile, mask the preload file, run the matrix). Store paths in it are
from that session.

## 1. Minimal reproduction (3 factors: glibc, preload, whether libstdc++ runs)

`t_free` calls `free(NULL)` in `main`; `t_locale` only constructs a `std::locale`
(so libstdc++'s static initialiser runs). Both are 2-line programs linked by the
devshell's gcc-15/c++ against glibc 2.42, run under a chosen ld.so via
`<ld.so> --library-path ...`. `/etc/ld-nix.so.preload` masked over `/dev/null`,
`LD_LIBRARY_PATH` unset.

```
preload file masked: []
glibc=42  preload=mimalloc-332  t_free    rc=0
glibc=42  preload=mimalloc-332  t_locale  rc=0
glibc=42  preload=mimalloc-353  t_free    rc=0
glibc=42  preload=mimalloc-353  t_locale  rc=0
glibc=44  preload=mimalloc-332  t_free    rc=0
glibc=44  preload=mimalloc-332  t_locale  rc=139        <-- SIGSEGV
glibc=44  preload=mimalloc-353  t_free    rc=0
glibc=44  preload=mimalloc-353  t_locale  rc=0
```

So: `free(NULL)` on its own is safe in every arm. The fault needs libstdc++'s
static initialiser *and* glibc 2.44 (newer than the 2.42 the preloaded object was
built against) *and* mimalloc 3.3.2. The same nixpkgs' 3.5.3 - built by the same
26.05 recipe, same glibc 2.42 - does not fault in that arm, so the fix is the
allocator version, not the alignment of its glibc.

## 2. Where the fault lands in `zg`

`zg` (`/nix/store/...-zvec-grep-0.3.0/bin/zg`) under `LD_PRELOAD=<mimalloc 3.3.2>`,
`LD_LIBRARY_PATH` unset, `LD_DEBUG=libs`:

```
620: calling init: .../glibc-2.44-25/lib/ld-linux-x86-64.so.2
623: calling init: .../glibc-2.44-25/lib/libc.so.6
626: calling init: .../glibc-2.44-25/lib/libm.so.6
629: calling init: .../gcc-16.2.0-lib/lib/libgcc_s.so.1
632: calling init: .../gcc-16.2.0-lib/lib/libstdc++.so.6
<-- SIGSEGV, nothing further; rc=139
```

Same command with mimalloc 3.5.3: the log continues past libstdc++ to
`libabsl_*`, `libonnxruntime.so.1`, `libzvec_c_api.so`, ... and the preload's own
constructor appears only near the end:

```
890: calling init: .../onnxruntime-1.27.1/lib/libonnxruntime.so.1
893: calling init: .../libzvec_c_api.so
896: calling init: .../malloc-provider-graphene-hardened/lib/libhardened_malloc.so
899: calling init: .../mimalloc-3.5.3/lib/libmimalloc.so   <-- the preload's own init
902: calling init: .../libsigcatch.so
```

So ld.so initialises the preload's constructor *after* the binary's dependency
initialisers, and everything the binary's dependencies do before that runs on an
allocator that has not been through its own initialisation yet. A signal handler
installed by a *preloaded* catcher (its own constructor would also run late)
never fires either, which is consistent: the fault happens in that window.

## 3. Consequence for the image

The guest preloads `pkgs.mimalloc` into every nix-linked process, so this window
is on every process's startup path there. The image now builds its preloaded
mimalloc from the same nixpkgs at 3.5.3 (`nix/image/layers.nix`), and
`container-wrapper-contracts` runs the image's own `zg` under that preload.
