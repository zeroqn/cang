#!/usr/bin/env bash
# rust-compile-bench: three-arm rustc build benchmark for cang's /workspace.
#
#   arm host            the same tree built natively on the host (the floor no
#                       tuning can beat), with the devshell's toolchain
#   arm guest-virtiofs   the tree built inside a cang guest as /workspace, so
#                       every source read and target write crosses virtio-fs
#   arm guest-local      the tree copied to the guest's virtio-blk container
#                       disk and built there: the VM without virtio-fs
#
# guest-virtiofs / guest-local is the virtio-fs penalty; host / guest-local is
# the VM penalty no virtio-fs change can touch.
#
# Usage: run.sh [--arms host,guest-virtiofs,guest-local] [--runs N]
#               [--workload synthetic|cang] [--rebuild-workload] [--bench-root DIR]
#
# Artifacts under $BENCH_ROOT (default /home/dev/cang-bench):
#   workload-<kind>/   the tree: `synthetic` (gen-workload.sh) or `cang`
#                      (a copy of this repo at HEAD, submodule trees included)
#   <arm>/             that arm's private copy (rebuilt per arm, never reused)
#   logs/              one log per run, plus a per-thread CPU capture per guest run
#   summary.tsv        one row per completed run (appended)
#
# Result protocol: every arm writes `bench-result-<tag>.txt` with a unique tag
# into the shared workspace and the harness reads BENCH_MS from it. The harness
# does not scrape the guest command's stdout for it: the arm needs a structured
# record per run, and a file in the workspace survives an arm whose cang CLI is
# killed early.
#
#   --report           print the ratio table for the recorded runs and exit
#
# Env: CANG (default <repo>/result/bin/cang), IMAGE, BENCH_ROOT, RUNS, WORKLOAD,
#      LEAVES/MODULES/LINES (synthetic size), JOBS (default 26 = a cang guest's
#      vcpu count on a 28-CPU host), GUEST_TIMEOUT (default 3000 s).
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
bench_root=${BENCH_ROOT:-/home/dev/cang-bench}
cang=${CANG:-$repo/result/bin/cang}
image=${IMAGE:-ghcr.io/zeroqn/cang:latest}
runs=${RUNS:-2}
jobs=${JOBS:-26}
guest_timeout=${GUEST_TIMEOUT:-3000}
linger=${LINGER:-10}
workload_kind=${WORKLOAD:-synthetic}
arms=host,guest-virtiofs,guest-local
rebuild_workload=0
report=0

while [ $# -gt 0 ]; do
    case $1 in
        --arms) arms=$2; shift 2 ;;
        --arms=*) arms=${1#*=}; shift ;;
        --runs) runs=$2; shift 2 ;;
        --runs=*) runs=${1#*=}; shift ;;
        --workload) workload_kind=$2; shift 2 ;;
        --workload=*) workload_kind=${1#*=}; shift ;;
        --rebuild-workload) rebuild_workload=1; shift ;;
        --bench-root) bench_root=$2; shift 2 ;;
        --bench-root=*) bench_root=${1#*=}; shift ;;
        --report) report=1; shift ;;
        -h|--help) sed -n '2,42p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "run.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

workload=$bench_root/workload-$workload_kind
tarball=$bench_root/workload-$workload_kind.tar
summary=$bench_root/summary-$workload_kind.tsv
mkdir -p "$bench_root/logs"
if [ ! -s "$summary" ]; then
    printf 'arm\trun\tbench_ms\touter_ms\tworkload\tfs_worker_pct\trustc\tcargo\n' >"$summary"
fi

# Extra read-only host paths to bind-mount into a guest arm at the same absolute
# path. The cang workload needs `virglrenderer`, which the image's /nix closure
# does not ship (ticket 11); mounting the host's store path keeps the measurement
# possible without an image rebuild, and is exactly what a `cang -v` is for.
guest_volumes=()
if [ "$workload_kind" = cang ]; then
    for pc in /nix/store/*virglrenderer-*/lib/pkgconfig; do
        [ -e "$pc" ] || continue
        dir=${pc%/lib/pkgconfig}
        guest_volumes+=(-v "$dir:$dir:ro")
        break
    done
fi
if [ -n "${EXTRA_VOLUMES:-}" ]; then
    for v in $EXTRA_VOLUMES; do guest_volumes+=(-v "$v"); done
fi

report_table() {
    [ -s "$summary" ] || { echo "run.sh: no recorded runs in $summary" >&2; return 1; }
    awk -F'\t' '
        NR > 1 {
            arm = $1
            n[arm]++
            sum[arm] += $3
            if ($6 != "" && $6 != "-") { ws[arm] += $6; wsn[arm]++ }
        }
        END {
            for (a in sum) {
                mean[a] = sum[a] / n[a] / 1000
                if (a == "host") host = mean[a]
                if (a == "guest-local") local = mean[a]
            }
            printf "%-14s %5s %9s %9s %15s\n", "arm", "runs", "mean s", "vs host", "vs guest-local"
            split("host guest-virtiofs guest-local", order, " ")
            for (i = 1; i <= 3; i++) {
                a = order[i]
                if (!(a in mean)) continue
                printf "%-14s %5d %9.1f %9s %15s", a, n[a], mean[a],
                    (host ? sprintf("%.2fx", mean[a] / host) : "-"),
                    (local ? sprintf("%.2fx", mean[a] / local) : "-")
                if (a in wsn) printf "   fs_worker %.0f%% of a core", ws[a] / wsn[a]
                printf "\n"
            }
        }' "$summary"
    echo
    echo "guest-virtiofs / guest-local is the virtio-fs penalty (the number the share costs);"
    echo "host / guest-local is the VM penalty (CPU and scheduling, which no share change touches)."
}

start_sampler() { # start_sampler <capture-file>
    "$repo/tools/rust-compile-bench/mon-threads.sh" "$1" 1 >/dev/null 2>&1 &
    sampler_pid=$!
}

stop_sampler() { # stop_sampler <capture-file>; prints mean fs_worker CPU% or nothing
    local capture=$1
    if [ -n "${sampler_pid:-}" ]; then
        kill "$sampler_pid" 2>/dev/null || true
        wait "$sampler_pid" 2>/dev/null || true
        sampler_pid=
    fi
    if [ -s "$capture" ]; then
        "$repo/tools/rust-compile-bench/summarise-threads.sh" "$capture" '^fs_worker$' \
            | awk 'NR == 1 { sub(/%/, "", $1); printf "%s", $1 }'
    fi
}

if [ "$report" = 1 ]; then
    report_table
    exit 0
fi

prepare_workload() {
    case $workload_kind in
    synthetic)
        if [ ! -d "$workload" ] || [ "$rebuild_workload" = 1 ]; then
            sh "$repo/tools/rust-compile-bench/gen-workload.sh" "$workload"
        fi
        build_cmd='cargo build --offline'
        ;;
    cang)
        rm -rf "$workload"
        mkdir -p "$workload"
        # This repo as cargo sees it in-tree: submodule contents included (they
        # are workspace crates), build output and VCS metadata excluded.
        tar -C "$repo" \
            --exclude=./target --exclude='*/target' --exclude=./.git \
            --exclude=./.cang --exclude=./.pi --exclude=./result \
            -cf - . | tar -C "$workload" -xf -
        cp "$repo/tools/rust-compile-bench/cang-workload-env.sh" "$workload/cang-workload-env.sh"
        build_cmd='cargo build --offline -p cang'
        ;;
    *) echo "run.sh: unknown workload '$workload_kind' (synthetic|cang)" >&2; exit 2 ;;
    esac
}
prepare_workload

if [ ! -f "$tarball" ] || [ "$rebuild_workload" = 1 ] || [ "$workload_kind" = cang ]; then
    tar -C "$workload" -cf "$tarball" .
fi

# The guest command's stdout is NOT forwarded to the host (`cang -- echo hi`
# prints nothing and exits 0) and its exit status is not propagated either, so an
# arm reports through files it writes into the shared workspace: its own log
# (everything, so a failure is readable afterwards) and the result file. Every
# arm runs through this same script, host and guest alike.
inner='set -eu
tag=$1
result=$2
log="$(dirname "$result")/bench-run-$tag.log"
# Append, and mark each start: a payload that exits non-zero can be re-run by the
# guest entrypoint, which would silently truncate the trace of an earlier attempt.
exec >>"$log" 2>&1
echo "=== inner start $(date -u +%H:%M:%S) tag=$tag"
on_exit() {
    rc=$?
    if [ "$rc" != 0 ]; then
        printf "BENCH_FAILED=1\nBENCH_RC=%s\n" "$rc" >"$result"
        echo "inner: failed with rc=$rc"
    fi
    # cang tears the guest down as soon as the payload exits, which can drop the
    # last writes (the result file included), so linger: see ticket 09.
    sleep '"$linger"'
}
trap on_exit EXIT
export RUSTC_WRAPPER=
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS='"$jobs"'
if [ -f cang-workload-env.sh ]; then . ./cang-workload-env.sh; fi
echo "inner: rustc=$(rustc --version 2>/dev/null || echo missing) jobs=$CARGO_BUILD_JOBS"
rm -rf target
start=$(date +%s%N)
if ! '"$build_cmd"'; then
    echo "inner: build failed"
    exit 1
fi
end=$(date +%s%N)
{
    printf "BENCH_MS=%s\n" "$(( (end - start) / 1000000 ))"
    printf "RUSTC=%s\n" "$(rustc --version 2>/dev/null || echo unknown)"
    printf "CARGO=%s\n" "$(cargo --version 2>/dev/null || echo unknown)"
} >"$result"
echo "inner: done"
'
now_ms() { date +%s%3N; }
secs() { awk -v ms="${1:-0}" 'BEGIN { printf "%.1f", ms / 1000 }'; }

# cang keys a task by workspace directory name and refuses a launch into a
# workspace that already has a running task, so a leftover VM from an interrupted
# run would make every later measurement fail instead of measuring.
require_workspace_free() {
    local ws busy
    ws=$(basename "$1")
    busy=$("$cang" ps 2>/dev/null | awk -v ws="$ws" '$NF == ws && $3 == "running" { printf "%s ", $1 }')
    if [ -n "$busy" ]; then
        echo "run.sh: workspace '$ws' already has a running cang task: $busy" >&2
        echo "run.sh: clear it first, e.g. $cang kill ${busy%% *}" >&2
        exit 2
    fi
}

# Launch a guest arm and wait for its result file rather than for `cang` to
# return: the CLI can come back while the guest is still working (or after the
# VM died), which would both lose the result and leave an orphan VM.
# The VM is always killed afterwards, and a VM that died without a result has its
# VMM log tail appended, because that is where a virtio-fs death is visible.
launch_guest_arm() {
    local dir=$1 log=$2 result=$3
    shift 3
    local cpid deadline handle ws vmm
    ws=$(basename "$dir")
    ( cd "$dir" && "$cang" --image "$image" "${guest_volumes[@]}" -- "$@" >>"$log" 2>&1 ) &
    cpid=$!
    deadline=$(( $(date +%s) + guest_timeout ))
    while [ ! -s "$result" ] && [ "$(date +%s)" -lt "$deadline" ]; do
        sleep 2
    done
    if [ ! -s "$result" ]; then
        echo "run.sh: guest arm '$ws' produced no result within ${guest_timeout}s" >&2
        for vmm in /home/dev/disp/cang/"$ws"/tasks/*/helper.stderr.log; do
            [ -f "$vmm" ] || continue
            echo "--- VMM log tail: $vmm" >>"$log"
            grep -iE 'error|panic|abort|exit code' "$vmm" | tail -10 >>"$log" 2>/dev/null || true
            tail -5 "$vmm" >>"$log" 2>/dev/null || true
        done
    fi
    kill "$cpid" 2>/dev/null || true
    wait "$cpid" 2>/dev/null || true
    handle=$("$cang" ps 2>/dev/null | awk -v ws="$ws" '$NF == ws && $3 == "running" { print $1; exit }')
    if [ -n "$handle" ]; then
        "$cang" kill "$handle" >>"$log" 2>&1 || true
        sleep 2
    fi
}

prepare_arm() {
    rm -rf "$bench_root/$1"
    cp -a "$workload" "$bench_root/$1"
    ln "$tarball" "$bench_root/$1/workload.tar" 2>/dev/null || cp "$tarball" "$bench_root/$1/workload.tar"
    printf '%s\n' "$inner" >"$bench_root/$1/bench-inner.sh"
}

result_of() { sed -n "s/^$2=//p" "$1" 2>/dev/null | tail -1; }

record() { # record <arm> <run> <resultfile> <outer_ms> <log> [fs_worker_pct]
    local resultfile=$3 outer_ms=$4 log=$5 fs_worker_pct=${6:--} bench_ms rustc_ver cargo_ver
    if [ ! -s "$resultfile" ]; then
        echo "run.sh: $1 run $2 wrote no result file ($resultfile); log $log" >&2
        return 1
    fi
    bench_ms=$(result_of "$resultfile" BENCH_MS)
    if ! [ "${bench_ms:-0}" -gt 0 ] 2>/dev/null; then
        echo "run.sh: $1 run $2 has no usable BENCH_MS in $resultfile; log $log" >&2
        return 1
    fi
    rustc_ver=$(result_of "$resultfile" RUSTC)
    cargo_ver=$(result_of "$resultfile" CARGO)
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$1" "$2" "$bench_ms" "$outer_ms" "$workload_kind" "$fs_worker_pct" \
        "${rustc_ver:-?}" "${cargo_ver:-?}" >>"$summary"
    printf '%-14s run %s  build %ss  outer %ss  fs_worker %s%%\n' \
        "$1" "$2" "$(secs "$bench_ms")" "$(secs "$outer_ms")" "$fs_worker_pct"
}

for arm in ${arms//,/ }; do
    case $arm in
    host|guest-virtiofs|guest-local) prepare_arm "$arm" ;;
    *) echo "run.sh: unknown arm $arm" >&2; exit 2 ;;
    esac
done

for arm in ${arms//,/ }; do
    dir=$bench_root/$arm
    case $arm in guest-*) require_workspace_free "$dir" ;; esac
    for n in $(seq 1 "$runs"); do
        log=$bench_root/logs/$arm-$n.log
        tag=$arm-$n-$(date +%s)
        result=$dir/bench-result-$tag.txt
        : >"$log"
        rm -f "$result"
        capture=$bench_root/logs/threads-$arm-$n.txt
        sampler_pid=
        case $arm in guest-*) start_sampler "$capture" ;; esac
        start=$(now_ms)
        case $arm in
        host)
            ( cd "$dir" && CARGO_BUILD_JOBS=$jobs nix develop "$repo" --command \
                bash bench-inner.sh "$tag" "$result" ) >>"$log" 2>&1
            ;;
        guest-virtiofs)
            launch_guest_arm "$dir" "$log" "$result" \
                sh bench-inner.sh "$tag" "/workspace/$(basename "$result")"
            ;;
        guest-local)
            launch_guest_arm "$dir" "$log" "$result" sh -c '
                set -eu
                # RAM-backed and FUSE-free: a plain cang guest mounts no
                # containers disk (/dev/vdb is absent unless podman runs) and
                # /home is the image root, so /dev/shm is the guest-local
                # location that actually exists. This is the CPU/IO floor of the
                # VM itself.
                local_dir=/dev/shm/cang-rust-bench
                rm -rf "$local_dir"
                mkdir -p "$local_dir"
                cp /workspace/workload.tar "$local_dir/workload.tar"
                tar -C "$local_dir" -xf "$local_dir/workload.tar"
                rm -f "$local_dir/workload.tar"
                cp /workspace/bench-inner.sh "$local_dir"/
                cd "$local_dir"
                exec sh bench-inner.sh "$1" "/workspace/$2"
            ' sh "$tag" "$(basename "$result")"
            ;;
        esac
        outer=$(( $(now_ms) - start ))
        fs_worker_pct=$(stop_sampler "$capture")
        record "$arm" "$n" "$result" "$outer" "$log" "${fs_worker_pct:--}"
    done
done

echo
echo "summary: $summary"
column -t -s $'\t' "$summary" 2>/dev/null || cat "$summary"
echo
report_table
