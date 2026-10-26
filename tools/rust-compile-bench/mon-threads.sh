#!/bin/bash
# 1 Hz per-thread CPU sampler for cang's VM worker, so "is the single FUSE worker
# thread the bottleneck?" has a number instead of an opinion. Appends
#   <ts>|<tid>|<comm>|<utime+stime ticks>|<schedstat runtime ns>
# per thread per second; summarise-threads.sh turns a capture into percentages.
#
# NOTE on parsing /proc/<tid>/stat: comm can contain SPACES (the FUSE worker's is
# literally "fs worker"), so the fields must be read from after the LAST ')'. A
# naive `awk '{print $14 + $15}'` then silently sums the wrong pair and reports
# roughly half the thread's CPU - that mistake produced a "12% of a core" figure
# for this very thread when the truth was ~26%.
#
# Usage: mon-threads.sh <outfile> [interval-seconds]
set -u
out=${1:?usage: mon-threads.sh <outfile> [interval]}
interval=${2:-1}
: >"$out"
while :; do
    # The VMM is the process with the most threads among cang's helpers; the
    # unshare wrapper matches the same cmdline but has one thread.
    pid=$(for p in $(pgrep -f 'libkrun-vm-worker-enter' 2>/dev/null); do
              echo "$(ls -d /proc/"$p"/task/* 2>/dev/null | wc -l) $p"
          done | sort -rn | head -1 | cut -d' ' -f2)
    if [ -n "${pid:-}" ]; then
        ts=$(date +%s)
        for t in /proc/"$pid"/task/*; do
            [ -r "$t/stat" ] || continue
            tid=${t##*/}
            name=$(tr ' ' '_' <"$t/comm" 2>/dev/null)
            tail_fields=$(sed 's/^.*) //' "$t/stat" 2>/dev/null) || continue
            ticks=$(printf '%s' "$tail_fields" | awk '{ print $12 + $13 }')
            sched=$(awk '{ print $1 }' "$t/schedstat" 2>/dev/null)
            printf '%s|%s|%s|%s|%s\n' "$ts" "$tid" "$name" "${ticks:-0}" "${sched:-0}" >>"$out"
        done
    fi
    sleep "$interval"
done
