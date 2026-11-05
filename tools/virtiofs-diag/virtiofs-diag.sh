#!/bin/sh
# virtiofs-diag: one self-contained diagnostic for "a build in a cang guest is
# slow". It decides how much of the wall clock the virtio-fs `/workspace` share
# owns, and what a second of that wall clock is made of.
#
# RUN IT INSIDE THE GUEST, from the workspace root (cang maps it to /workspace).
# Add CANG_FS_STATS=1 on the host side or the request histogram - the decisive
# number - is lost with the task directory:
#
#   CANG_FS_STATS=1 cang -- sh /workspace/tools/virtiofs-diag/virtiofs-diag.sh all
#   # in a host shell while the guest runs, once a second:
#   R=$(awk -F'"' '/^location/{print $2}' ~/.config/cang/cang.toml)
#   for f in "$R"/cang/<workspace-dirname>/tasks/*/helper.stderr.log; do
#       [ -f "$f" ] && cp "$f" /tmp/hist-$(date +%s).log; done
#
# Modes (`all` runs every mode):
#   probe    per-op latency on the share against a control, in microseconds. The
#            batch is timed as a whole so timing never forks; ops the shell cannot
#            do as a builtin (mv, rm, dd) are marked "+exec" and a process-spawn
#            baseline is printed separately, because starting a binary from the
#            share is itself FUSE traffic
#   tree     what the build reads and writes: source and target file counts and
#            MiB, and the target subdirectories that hold the files
#   iterate  a no-op rebuild (cargo deciding every unit is fresh) and one touched
#            file - the metadata half of a compile-test loop
#   cold     a full cold build into a throwaway target dir on the share, and the
#            same build from /dev/shm: the difference between the arms IS the
#            share's cost
#
# It writes only `$WS/virtiofs-diag-report.md`, `$WS/.virtiofs-diag-share-target`
# and a source copy under /dev/shm, and never touches the project's own target/.
# `iterate` is the cheap mode; `cold` writes about the size of target/ into the
# share and is skipped when free space is short.
#
# Env: DIAG_JOBS (default nproc), DIAG_RUNS (default 2, arms interleaved),
#      DIAG_SKIP_COLD=1, DIAG_ALLOW_COLD=1, DIAG_TOUCH=<workspace-relative file>,
#      DIAG_WORKSPACE, DIAG_SHM, DIAG_CARGO, DIAG_REPORT.
set -u

WS=${DIAG_WORKSPACE:-/workspace}
SHM=${DIAG_SHM:-/dev/shm}
REPORT=${DIAG_REPORT:-$WS/virtiofs-diag-report.md}
JOBS=${DIAG_JOBS:-$(nproc 2>/dev/null || echo 4)}
RUNS=${DIAG_RUNS:-2}
CARGO=${DIAG_CARGO:-cargo}
TOUCH=${DIAG_TOUCH:-}
TMP=$SHM/virtiofs-diag-tmp-$$
mkdir -p "$TMP" || { echo "virtiofs-diag: cannot write $TMP" >&2; exit 1; }
trap 'rm -rf "$TMP"' EXIT

now_ns() { date +%s%N 2>/dev/null || echo 0; }
say() { printf '%s\n' "$*" >>"$REPORT"; printf '%s\n' "$*"; }
kv() { printf '%s=%s\n' "$1" "$2" >>"$REPORT"; }
section() { say ""; say "== $1 =="; }
ns_to_s() { awk -v t="${1:-0}" 'BEGIN { printf "%.3f", t / 1000000000 }'; }

# batch <count> <label> <snippet>: eval one op per iteration, time the batch.
# `eval` and the redirections are builtins, so the loop forks nothing.
batch() {
    b_n=$1; b_label=$2; b_op=$3
    b_i=0
    b_a=$(now_ns)
    while [ "$b_i" -lt "$b_n" ]; do
        eval "$b_op"
        b_i=$((b_i + 1))
    done
    b_b=$(now_ns)
    awk -v t=$((b_b - b_a)) -v n="$b_n" -v l="$b_label" 'BEGIN {
        printf "  %-32s %6d ops  mean %9.1f us  %8.1f k ops/s\n", l, n, t / n / 1000, n / (t / 1e9) / 1000 }'
}

probe_path() { # probe_path <dir> <label>
    p_dir=$1; p_label=$2; p_d=$1/.virtiofs-diag-probe
    rm -rf "$p_d"; mkdir -p "$p_d" || { say "  $p_label: cannot create $p_d"; return; }
    say "  path $p_label ($p_dir)"
    batch 100 "$p_label exec (a binary on this path)" 'mv --version >/dev/null 2>&1'
    batch 300 "$p_label create (open+close)" ': >"$p_d/f$b_i"'
    batch 300 "$p_label write 4 KiB + close" 'printf "%4096s" "" >"$p_d/w$b_i" 2>/dev/null'
    batch 300 "$p_label read 4 KiB (one read)" 'IFS= read -r _l <"$p_d/w$b_i"'
    batch 300 "$p_label stat" '[ -f "$p_d/w$b_i" ]'
    batch 100 "$p_label readdir (300 names)" 'set -- "$p_d"/*'
    batch 300 "$p_label rename (+1 exec)" 'mv "$p_d/f$b_i" "$p_d/f$b_i.r"'
    batch 300 "$p_label unlink (+1 exec)" 'rm -f "$p_d/f$b_i.r"'
    batch  20 "$p_label write 4 KiB + fsync (+1 exec)" 'dd if=/dev/zero of="$p_d/fs" bs=4096 count=1 conv=fsync 2>/dev/null'
    p_a=$(now_ns); dd if=/dev/zero of="$p_d/big" bs=32768 count=1024 2>/dev/null; p_b=$(now_ns)
    [ $((p_b - p_a)) -gt 0 ] && say "$(awk -v t=$((p_b - p_a)) -v l="$p_label" 'BEGIN { printf "  %-32s        32 MiB %8.1f MiB/s write", l, 32 / (t / 1e9) }')"
    p_a=$(now_ns); dd if="$p_d/big" of=/dev/null bs=32768 2>/dev/null; p_b=$(now_ns)
    [ $((p_b - p_a)) -gt 0 ] && say "$(awk -v t=$((p_b - p_a)) -v l="$p_label" 'BEGIN { printf "  %-32s        32 MiB %8.1f MiB/s read", l, 32 / (t / 1e9) }')"
    rm -rf "$p_d"
}

count_files() { find "$1" -type f 2>/dev/null | wc -l; }
sum_bytes() { find "$1" -type f -printf '%s\n' 2>/dev/null | awk '{ s += $1 } END { printf "%.0f", s + 0 }'; }

time_build() { # time_build <dir> <target-dir> <label>: echoes seconds, or nothing
    t_dir=$1; t_tgt=$2; t_label=$3
    t_a=$(now_ns)
    ( cd "$t_dir" && CARGO_TARGET_DIR="$t_tgt" CARGO_BUILD_JOBS="$JOBS" RUSTC_WRAPPER= \
        "$CARGO" build --offline ) >"$TMP/build-$t_label.log" 2>&1
    t_rc=$?
    t_b=$(now_ns)
    if [ "$t_rc" != 0 ]; then
        { printf '  build %s FAILED (rc=%s), last lines:\n' "$t_label" "$t_rc"
          tail -5 "$TMP/build-$t_label.log" | sed 's/^/    /'; } >>"$REPORT"
        printf '  build %s FAILED (rc=%s), log %s\n' "$t_label" "$t_rc" "$TMP/build-$t_label.log" >&2
        return 0
    fi
    ns_to_s $((t_b - t_a))
}

mode_probe() {
    section "per-op latency (one batch, timed as a whole)"
    say "  guest $(uname -r)  nproc=$(nproc 2>/dev/null)  mem=$(awk '/MemTotal/{printf "%d MiB", $2/1024}' /proc/meminfo)"
    say "  mounts: $(awk '$2 == "/" || $2 == "'"$WS"'" { printf "%s %s %s; ", $2, $3, $4 }' /proc/mounts)"
    probe_path "$WS" share
    say ""
    probe_path "$SHM" tmpfs
    say ""
    say "  Reading: 'share' minus 'tmpfs' is the round trip; nothing here touches disk."
    say "  Ops marked (+1 exec) fork a binary, so they include one process start: the"
    say "  'exec' row is that start, and it is 3 ms on the share because the binary is"
    say "  also on the share. A create or 4 KiB write in the hundreds of microseconds is"
    say "  round trips; an fsync row in the milliseconds is the host's disk, and rustc"
    say "  does not fsync."
}

mode_tree() {
    section "what the build reads and writes"
    d_src_files=$(find "$WS" -path "$WS/target" -prune -o -type f -print 2>/dev/null | wc -l)
    d_src_mib=$(awk -v b="$(find "$WS" -path "$WS/target" -prune -o -type f -printf '%s\n' 2>/dev/null | awk '{ s += $1 } END { printf "%.0f", s + 0 }')" 'BEGIN { printf "%.1f", b / 1048576 }')
    say "$(printf '  %-26s %10s files %10s MiB' 'sources (excl target/)' "$d_src_files" "$d_src_mib")"
    kv DIAG_SOURCE_FILES "$d_src_files"
    if [ -d "$WS/target" ]; then
        d_tgt_files=$(count_files "$WS/target")
        d_tgt_mib=$(awk -v b="$(sum_bytes "$WS/target")" 'BEGIN { printf "%.1f", b / 1048576 }')
        say "$(printf '  %-26s %10s files %10s MiB' 'target/' "$d_tgt_files" "$d_tgt_mib")"
        kv DIAG_TARGET_FILES "$d_tgt_files"
        say "  target/ subdirectories:"
        for d in "$WS"/target/*/; do
            [ -d "$d" ] || continue
            say "$(printf '    %-24s %10s files' "$(basename "$d")" "$(count_files "$d")")"
        done
        say ""
        say "  requests per target file: the histogram's total request count divided by"
        say "  the target/ file count above. Reference (2401 crates, 7 200 target files):"
        say "  464 193 requests = 64 per file, and 10 000 requests bought one second."
    else
        say "  no target/ yet in $WS"
    fi
}

mode_iterate() {
    section "a compile-test iteration (no-op rebuild)"
    say "  jobs=$JOBS  cargo=$($CARGO --version 2>/dev/null || echo '?')"
    if [ ! -d "$WS/target" ]; then
        say "  target/ is absent; warming it (not timed)"
        time_build "$WS" "$WS/target" warm >/dev/null
    fi
    i=1
    while [ "$i" -le "$RUNS" ]; do
        a=$(now_ns)
        ( cd "$WS" && CARGO_BUILD_JOBS="$JOBS" RUSTC_WRAPPER= "$CARGO" build --offline ) >/dev/null 2>&1
        b=$(now_ns)
        s=$(ns_to_s $((b - a)))
        say "$(printf '  no-op rebuild, run %s%12s s' "$i" "$s")"
        kv "ITERATE_NOOP_S_$i" "$s"
        i=$((i + 1))
    done
    if [ -n "$TOUCH" ] && [ -f "$WS/$TOUCH" ]; then
        a=$(now_ns)
        ( cd "$WS" && touch "$TOUCH" && CARGO_BUILD_JOBS="$JOBS" RUSTC_WRAPPER= "$CARGO" build --offline ) >/dev/null 2>&1
        b=$(now_ns)
        s=$(ns_to_s $((b - a)))
        say "$(printf '  one touched file (%s)%8s s' "$TOUCH" "$s")"
        kv ITERATE_TOUCH_S "$s"
    else
        say "  (DIAG_TOUCH=<workspace-relative path> adds the touched-file rebuild arm)"
    fi
}

mode_cold() {
    section "a full cold build: share vs tmpfs (the difference is the share's cost)"
    if [ "${DIAG_SKIP_COLD:-0}" = 1 ]; then say "  skipped (DIAG_SKIP_COLD=1)"; return; fi
    c_need=0
    [ -d "$WS/target" ] && c_need=$(sum_bytes "$WS/target")
    c_have=$(df -k "$WS" | awk 'NR == 2 { print $4 * 1024 }')
    c_src=$(find "$WS" -path "$WS/target" -prune -o -type f -printf '%s\n' 2>/dev/null | awk '{ s += $1 } END { printf "%.0f", s + 0 }')
    if [ "${DIAG_ALLOW_COLD:-0}" != 1 ] && [ "$c_need" -gt 0 ] && [ "$c_have" -lt $((c_need * 12 / 10)) ]; then
        say "  skipped: target/ is $((c_need / 1048576)) MiB, the share has $((c_have / 1048576)) MiB free"
        say "  (DIAG_ALLOW_COLD=1 forces it; 'iterate' needs no space at all)"
        return
    fi
    c_tree=$SHM/virtiofs-diag-src
    rm -rf "$c_tree"; mkdir -p "$c_tree"
    say "  copying sources (excl target/) to $c_tree: $((c_src / 1048576)) MiB"
    if ! ( cd "$WS" && tar --exclude=./target --exclude=./.virtiofs-diag-share-target -cf - . ) | ( cd "$c_tree" && tar -xf - ); then
        say "  copy failed; skipping the tmpfs arm"
        rm -rf "$c_tree"
        return
    fi
    round=1
    while [ "$round" -le "$RUNS" ]; do
        c_w=$(time_build "$WS" "$WS/.virtiofs-diag-share-target" "share-$round")
        c_m=$(time_build "$c_tree" "$c_tree/target" "shm-$round")
        if [ -n "$c_w" ] && [ -n "$c_m" ]; then
            say "$(awk -v w="$c_w" -v m="$c_m" -v r="$round" 'BEGIN {
                printf "  round %s  share %7s s   tmpfs %7s s   share costs %7.3f s (%.2fx)", r, w, m, w - m, w / m }')"
            kv "COLD_SHARE_S_$round" "$c_w"
            kv "COLD_SHM_S_$round" "$c_m"
        fi
        round=$((round + 1))
        rm -rf "$WS/.virtiofs-diag-share-target" "$c_tree/target"
    done
    rm -rf "$c_tree"
    say ""
    say "  The tmpfs arm is the same VM without the share: subtract it and what is"
    say "  left is the share. Run both arms at two job counts (DIAG_JOBS=6 and 26) -"
    say "  the share's absolute cost does not move with jobs, only its ratio does."
}

main() {
    : >"$REPORT"
    section "virtiofs-diag $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    kv DIAG_KERNEL "$(uname -r)"
    kv DIAG_NPROC "$(nproc 2>/dev/null)"
    kv DIAG_JOBS "$JOBS"
    kv DIAG_WORKSPACE "$WS"
    kv DIAG_MEM_MIB "$(awk '/MemTotal/{printf "%d", $2/1024}' /proc/meminfo)"
    kv DIAG_SHM_MIB "$(df -m "$SHM" | awk 'NR == 2 { printf "%d", $2 }')"
    kv DIAG_WS_FREE_MIB "$(df -m "$WS" | awk 'NR == 2 { printf "%d", $4 }')"
    say "  paste this whole file back: $REPORT"
    say "  not collectable from inside the guest: the FUSE request histogram. Relaunch"
    say "  with CANG_FS_STATS=1 and harvest the task's helper.stderr.log (see header)."
    for mode in "$@"; do
        case $mode in
        probe) mode_probe ;;
        tree) mode_tree ;;
        iterate) mode_iterate ;;
        cold) mode_cold ;;
        all) mode_probe; mode_iterate; mode_tree; mode_cold ;;
        *) echo "virtiofs-diag: unknown mode '$mode' (probe|tree|iterate|cold|all)" >&2; exit 2 ;;
        esac
    done
    section done
}

main "$@"
