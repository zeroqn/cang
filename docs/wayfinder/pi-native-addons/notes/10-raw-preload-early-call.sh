#!/usr/bin/env bash
# Minimal reproduction of the preloaded-allocator fault that killed `zg` in the
# guest, and the matrix that separates its three factors.
#
#   glibc 2.42 (what the preloaded object was built against) + mimalloc 3.3.2 -> ok
#   glibc 2.44 (zg's own libc, from nixpkgs-unstable)      + mimalloc 3.3.2 -> SIGSEGV
#   glibc 2.44                                            + mimalloc 3.5.3 -> ok
#
# `t_free` (free(NULL) and nothing else) is fine in every arm: the fault needs
# libstdc++'s static initialiser, which is what runs in the window ld.so leaves
# between the binary's dependency initialisers and a preload's own constructor.
#
# Needs a C/C++ compiler (`nix develop --command bash <this script>`) and the
# paths below, which are from the 2026-10-16 session. Run inside
# `unshare -r -m` so /etc/ld-nix.so.preload can be masked.
set -u
B=${1:-/tmp/preload-early-call}
mkdir -p "$B"

cat > "$B/t_free.c" <<'EOF'
#include <stdlib.h>
int main(void) { free(0); return 0; }
EOF
cat > "$B/t_locale.cc" <<'EOF'
#include <locale>
int main(void) { std::locale l("C"); (void)l; return 0; }
EOF
cc -O0 -o "$B/t_free" "$B/t_free.c"
c++ -O0 -o "$B/t_locale" "$B/t_locale.cc"

mount --bind /dev/null /etc/ld-nix.so.preload || { echo "MASK FAILED" >&2; exit 9; }
G42=/nix/store/fsd4vbp9zjd9iaicq2fb9lpc1h5j973y-glibc-2.42-100
G44=/nix/store/h4wfwic161kxrr74jlzla5lsm28hgary-glibc-2.44-25
G16=/nix/store/j7qx4s4mr17j1wqgvqdzj33lmrnzb387-gcc-16.2.0-lib/lib
M332=/nix/store/kxmhr4lnrn66i7haivinf7pbk8ibgxc6-mimalloc-3.3.2/lib/libmimalloc.so
M353=/nix/store/45n260gs000cmi7fff67gy428yxi26dw-mimalloc-3.5.3/lib/libmimalloc.so
echo "preload file masked: [$(cat /etc/ld-nix.so.preload)]"
for glibc in 42 44; do
  case $glibc in
    42) LD=$G42/lib/ld-linux-x86-64.so.2; LIB=$G42/lib ;;
    44) LD=$G44/lib/ld-linux-x86-64.so.2; LIB=$G44/lib:$G16 ;;
  esac
  for pre in 332 353; do
    case $pre in 332) M=$M332 ;; 353) M=$M353 ;; esac
    for prog in t_free t_locale; do
      env -u LD_LIBRARY_PATH LD_PRELOAD=$M $LD --library-path "$LIB" "$B/$prog" >/dev/null 2>&1
      printf 'glibc=%s  preload=mimalloc-%s  %-9s rc=%s\n' "$glibc" "$pre" "$prog" "$?"
    done
  done
done
