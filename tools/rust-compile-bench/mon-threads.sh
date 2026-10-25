#!/bin/bash
# 1 Hz per-thread CPU sampler for cang's VM worker, so "is the single FUSE worker
# thread the bottleneck?" has a number instead of an opinion. Appends
#   <ts>|<tid>|<comm>|<utime+stime ticks>
# per thread per second; summarise-threads.sh turns a capture into percentages.
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
            ticks=$(awk '{ print $14 + $15 }' "$t/stat" 2>/dev/null)
            printf '%s|%s|%s|%s\n' "$ts" "$tid" "$name" "$ticks" >>"$out"
        done
    fi
    sleep "$interval"
done
