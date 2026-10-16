
set -u
mount --bind /dev/null /etc/ld-nix.so.preload || { echo "MASK FAILED"; exit 9; }
Z=/nix/store/30swk6s3s01n03h9ikdv8ij9jnhwk061-zvec-grep-0.3.0/bin/zg
M332=/nix/store/kxmhr4lnrn66i7haivinf7pbk8ibgxc6-mimalloc-3.3.2/lib/libmimalloc.so
M353=/nix/store/45n260gs000cmi7fff67gy428yxi26dw-mimalloc-3.5.3/lib/libmimalloc.so
OLD=/nix/store/dqj57l1prad38vjrad4bs9d977i4q5zm-gcc-15.3.0-lib/lib
NEW=/nix/store/j7qx4s4mr17j1wqgvqdzj33lmrnzb387-gcc-16.2.0-lib/lib
echo "inherited LD_LIBRARY_PATH=[${LD_LIBRARY_PATH:-}]"
run() { label="$1"; shift; out=$( env -u LD_LIBRARY_PATH "$@" 2>&1 ); rc=$?; printf '%-46s rc=%-4s %s\n' "$label" "$rc" "$(printf '%s' "$out" | head -1 | cut -c1-110)"; }
run "A clean (no preload, no LD_LIBRARY_PATH)"        env LD_PRELOAD= $Z --version
run "B mimalloc 3.5.3 (fixed allocator)"              env LD_PRELOAD=$M353 $Z --version
run "C mimalloc 3.3.2 (pre-fix allocator)"            env LD_PRELOAD=$M332 $Z --version
run "D LD_LIBRARY_PATH=26.05 gcc (pre-fix wrapper)"   env LD_PRELOAD= LD_LIBRARY_PATH=$OLD $Z --version
run "E LD_LIBRARY_PATH=unstable gcc (fixed wrapper)"  env LD_PRELOAD= LD_LIBRARY_PATH=$NEW $Z --version
run "F pre-fix combo (332 + 26.05)"                   env LD_PRELOAD=$M332 LD_LIBRARY_PATH=$OLD $Z --version
run "G FIXED combo (3.5.3 + unstable)"                env LD_PRELOAD=$M353 LD_LIBRARY_PATH=$NEW $Z --version
run "H half-fixed (3.5.3 + 26.05)"                    env LD_PRELOAD=$M353 LD_LIBRARY_PATH=$OLD $Z --version
run "I half-fixed (3.3.2 + unstable)"                 env LD_PRELOAD=$M332 LD_LIBRARY_PATH=$NEW $Z --version
