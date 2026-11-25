#!/bin/sh
# Guest-side sampler for a responsiveness / memory-pressure arm.
#
# One block per tick: a marker, then verbatim /proc/meminfo, /proc/vmstat,
# /proc/pressure/memory, the zram device's own counters, /proc/swaps, and a census
# of every resident process (comm, oom_score_adj, size, resident pages) - so a
# victim that disappears between two ticks is a visible gap and the oom_score_adj
# readback is a measurement rather than an assumption.
#
# Provenance: this is the sampler that measured the wall in
# docs/wayfinder/guest-responsiveness/notes/01-memory-failure-ladder.md, after
# two fixes its first probe run needed: `printf` writes a leading `--` argument
# under dash, and the per-field zram sysfs attributes (mem_limit, mem_used_max,
# mem_used_total) are root-only, while mm_stat is world-readable - so only
# mm_stat is read here and its first six fields carry the rest.
#
# usage: sample-guest.sh OUT_FILE [INTERVAL_S] [CANDIDATE_MAX_S]
set -u
out=${1:?usage: sample-guest.sh OUT_FILE [INTERVAL_S] [CANDIDATE_MAX_S]}
interval=${2:-0.5}
max_s=${3:-7200}
start=$(date +%s)
while :; do
    [ $(( $(date +%s) - start )) -ge "$max_s" ] && break
    printf '%s\n' "--- SAMPLE $(date -u +%s.%N)" >>"$out"
    cat /proc/meminfo >>"$out"
    cat /proc/vmstat >>"$out"
    printf 'pressure: ' >>"$out"
    cat /proc/pressure/memory >>"$out"
    printf 'zram0:mm_stat %s\n' "$(cat /sys/block/zram0/mm_stat 2>&1)" >>"$out"
    printf 'zram0:disksize %s\n' "$(cat /sys/block/zram0/disksize 2>&1)" >>"$out"
    printf 'zram0:comp_algorithm %s\n' "$(cat /sys/block/zram0/comp_algorithm 2>&1)" >>"$out"
    printf 'zram0:stat %s\n' "$(cat /sys/block/zram0/stat 2>&1)" >>"$out"
    printf 'zram0:io_stat failed_reads failed_writes notify_free = %s\n' "$(cat /sys/block/zram0/io_stat 2>&1)" >>"$out"
    printf 'swaps: %s\n' "$(sed -n 2p /proc/swaps 2>&1)" >>"$out"
    printf '%s\n' '--- PROCS' >>"$out"
    for p in /proc/[0-9]*; do
        pid=${p#/proc/}
        comm= adj= rss=
        # A process that vanishes between the glob and the read is normal on a
        # busy guest; silence each read so the session log is not 428 lines of
        # "No such file or directory".
        IFS= read -r comm <"$p/comm" 2>/dev/null || continue
        IFS= read -r adj <"$p/oom_score_adj" 2>/dev/null || adj=?
        IFS= read -r rss <"$p/statm" 2>/dev/null || rss='0 0'
        set -- $rss
        printf 'proc %s %s %s %s %s\n' "$pid" "$comm" "$adj" "$1" "${2:-0}" >>"$out"
    done
    sleep "$interval"
done
