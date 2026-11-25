#!/usr/bin/env bash
# Host-side driver for one arm: gate the host, boot one guest, capture what cang
# deletes, tear the arm down, leave the raw files behind.
#
# It decides nothing: it launches, copies and records, and the note that cites it
# owns the verdict. Provenance: the driver that ran the seven boots behind
# docs/wayfinder/guest-responsiveness/notes/01-memory-failure-ladder.md and 12-exec-oom-inheritance.md.
#
# Usage (from the repo root):
#   ARM=arm1 MODE=session PAYLOAD=storm.sh MEM=4 ./tools/responsive-arm/run-arm.sh
#   ARM=arm2 MODE=exec    PAYLOAD=exstorm.sh MEM=4 ./tools/responsive-arm/run-arm.sh
#
# Env:
#   ARM            workspace/out-dir name, unique per arm            (required)
#   MODE           session (payload runs in the managed session) or
#                  exec (payload runs through `cang exec`)           (required)
#   PAYLOAD        guest-side script name under /workspace           (required)
#   MEM            --mem in GiB, always explicit: a guest defaults to
#                  80% of host RAM, and two such guests do not fit   (default 4)
#   IMAGE          image reference (default localhost/cang:nixone)
#   CANG           host CLI (default ./result/bin/cang)
#   GUEST_INIT     cang-guest-init executable; default is the repo's
#                  `nix build .#cang-musl` result, resolved fresh each run
#   WORKLOAD_SRC   host directory copied to <dir>/workload first (optional)
#   OUT_DIR        root for per-arm directories (default .scratch/responsive-arms)
#   WATCH_S        how long the console copier runs (default 1800)
#   DONE_TIMEOUT   how long to wait for the guest's done sentinel (default 600)
#
# One arm per boot, one arm at a time. The gate refuses to start while any other
# microVM is running, because a co-tenant invalidates every timing number.
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
: "${ARM:?set ARM}"
: "${MODE:?set MODE=session|exec}"
: "${PAYLOAD:?set PAYLOAD (a guest script under /workspace)}"
mem=${MEM:-4}
: "${IMAGE:=localhost/cang:nixone}"
: "${OUT_DIR:=$root/.scratch/responsive-arms}"
watch_s=${WATCH_S:-1800}
done_timeout=${DONE_TIMEOUT:-600}
cang=${CANG:-$root/result/bin/cang}
workload_src=${WORKLOAD_SRC:-}

if [ -z "${GUEST_INIT:-}" ]; then
    gi_pkg=$(cd "$root" && nix build --no-link --print-out-paths .#cang-musl) || exit 1
    GUEST_INIT="$gi_pkg/bin/cang-guest-init"
fi
[ -x "$GUEST_INIT" ] || { echo "GUEST_INIT is not executable: $GUEST_INIT" >&2; exit 1; }

dir=$OUT_DIR/$ARM
hostdir=$dir/host
mkdir -p "$hostdir/vmm" "$dir/out"
: >"$hostdir/driver.log"
dlog() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" | tee -a "$hostdir/driver.log"; }

dlog "arm=$ARM mode=$MODE mem=${mem}GiB payload=$PAYLOAD"
dlog "cang=$(readlink -f "$cang")"
dlog "guest-init=$GUEST_INIT"
dlog "image=$IMAGE"

# --- gate: no other microVM on this host -----------------------------------
# Matched on the worker's own comm, not on a command line: a `pgrep -f` here also
# matches whoever typed the pattern (a shell running this driver from a command
# line that mentions it), which reports a co-tenant that is not there.
foreign=$(pgrep -x libkrun-vm-work | wc -l)
xcount=$(pgrep -x cang | wc -l)
dlog "gate foreign_workers=$foreign pgrep_x_cang=$xcount"
if [ "$foreign" -ne 0 ]; then
    for pid in $(pgrep -x libkrun-vm-work); do
        tr '\0' ' ' <"/proc/$pid/cmdline" | cut -c1-160 >>"$hostdir/driver.log"
    done
    dlog "ABORT: a foreign microVM is running; a co-tenant invalidates the numbers"
    exit 7
fi
busy=$($cang ps 2>/dev/null | awk -v ws="$ARM" '$NF == ws && $3 == "running" { printf "%s ", $1 }')
if [ -n "$busy" ]; then
    dlog "ABORT: workspace $ARM already has a running task: $busy"
    exit 8
fi

if [ -n "$workload_src" ]; then
    dlog "staging workload from $workload_src"
    rm -rf "$dir/workload"
    cp -a "$workload_src" "$dir/workload"
    dlog "workload files=$(find "$dir/workload" -type f | wc -l)"
fi

# --- console copier: cang deletes its task dir, so copy while the VM lives ---
( end=$(( $(date +%s) + watch_s ))
  while [ "$(date +%s)" -lt "$end" ]; do
      for f in "/home/$USER/disp/cang/$ARM"/tasks/*/guest-kernel-console.log \
               "/home/$USER/disp/cang/$ARM"/tasks/*/helper.stderr.log \
               "/home/$USER/disp/cang/$ARM"/tasks/*/helper.stdout.log; do
          [ -f "$f" ] || continue
          t=$(basename "$(dirname "$f")")
          cp -f "$f" "$hostdir/vmm/$t.$(basename "$f")" 2>/dev/null
      done
      sleep 1
  done ) &
cop=$!
dlog "console copier pid=$cop"

rm -f "$dir/out/done.txt"
if [ "$MODE" = exec ]; then
    session_payload="sh -lc 'mkdir -p /workspace/out; date -u > /workspace/out/session-up.txt; sleep 2400'"
    launch="$cang --image $IMAGE --guest-init $GUEST_INIT --mem $mem -- sh -lc 'mkdir -p /workspace/out; date -u > /workspace/out/session-up.txt; sleep 2400'"
else
    launch="$cang --image $IMAGE --guest-init $GUEST_INIT --mem $mem -- sh /workspace/$PAYLOAD"
fi
dlog "session command: $launch"
( cd "$dir" && script -q -e -c "$launch" /dev/null >"$hostdir/cang-$MODE.log" 2>&1 ) &
cpid=$!
dlog "cang launcher pid=$cpid"

task=
for _ in $(seq 1 180); do
    task=$($cang ps 2>/dev/null | awk -v ws="$ARM" '$NF == ws && $3 == "running" { print $2; exit }')
    [ -n "$task" ] && break
    kill -0 "$cpid" 2>/dev/null || { dlog "cang launcher exited before a task appeared"; break; }
    sleep 1
done
dlog "task=$task"

if [ "$MODE" = exec ]; then
    n=0
    while [ ! -s "$dir/out/session-up.txt" ]; do
        n=$(( n + 2 )); [ "$n" -gt 240 ] && { dlog "ABORT: session payload never came up"; break; }
        kill -0 "$cpid" 2>/dev/null || { dlog "cang session launcher exited"; break; }
        sleep 2
    done
    dlog "session-up: $(cat "$dir/out/session-up.txt" 2>/dev/null)"
    dlog "payload through the exec listener"
    ( cd "$dir" && "$cang" exec "$task" -- sh "/workspace/$PAYLOAD" >"$hostdir/cang-exec.log" 2>&1 )
    dlog "cang exec returned rc=$?"
fi

n=0
while [ ! -s "$dir/out/done.txt" ]; do
    [ "$n" -ge "$done_timeout" ] && break
    sleep 2; n=$(( n + 2 ))
    # Re-read the sentinel after the sleep before blaming the launcher: it exits on
    # the payload, so a liveness check first reports a failure that did not happen.
    [ -s "$dir/out/done.txt" ] && break
    kill -0 "$cpid" 2>/dev/null || break
done
if [ -s "$dir/out/done.txt" ]; then
    dlog "done sentinel: $(cat "$dir/out/done.txt")"
    dlog "cang launcher still alive=$(kill -0 "$cpid" 2>/dev/null && echo yes || echo no)"
else
    dlog "WARN: no done sentinel after ${n}s (launcher alive=$(kill -0 "$cpid" 2>/dev/null && echo yes || echo no))"
fi

# --- teardown: launcher first, then this workspace's own task ---------------
for _ in $(seq 1 60); do kill -0 "$cpid" 2>/dev/null || break; sleep 1; done
if kill -0 "$cpid" 2>/dev/null; then
    dlog "cang launcher still alive after 60 s: SIGTERM to its own pid, then this workspace's task"
    kill -TERM "$cpid" 2>/dev/null
    sleep 5
    kill -KILL "$cpid" 2>/dev/null
fi
wait "$cpid" 2>/dev/null
handle=$($cang ps 2>/dev/null | awk -v ws="$ARM" '$NF == ws && $3 == "running" { print $1; exit }')
if [ -n "$handle" ]; then
    dlog "killing leftover task handle=$handle"
    "$cang" kill "$handle" >>"$hostdir/driver.log" 2>&1
    sleep 3
fi
dlog "post cang ps: $($cang ps 2>/dev/null | awk -v ws="$ARM" '$NF == ws { printf "%s %s %s | ", $1, $2, $3 }')"
kill "$cop" 2>/dev/null
wait "$cop" 2>/dev/null

dlog "workspace evidence: $(ls -1 "$dir/out" 2>/dev/null | tr '\n' ' ')"
dlog "console copies: $(ls -1 "$hostdir/vmm" 2>/dev/null | tr '\n' ' ')"
# The task dir only survives with --preserve-debug, which this driver does not pass:
# the copier above is the capture. Run it with --preserve-debug by hand if you want
# launch.conf and the rootfs snapshot as well.
dlog "END arm=$ARM"
