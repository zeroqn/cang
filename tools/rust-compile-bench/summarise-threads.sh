#!/bin/bash
# Summarise a mon-threads.sh capture: mean CPU% per thread name over its own
# sampled window, from utime+stime ticks (100 Hz clock) and, when present,
# schedstat runtime as a cross-check.
#
# Usage: summarise-threads.sh <capture> [thread-name-regex]
set -u
f=${1:?usage: summarise-threads.sh <capture> [filter]}
filter=${2:-}
awk -F'|' -v filter="$filter" '
    { key = $2
      if (!(key in first)) { first[key] = $4; sc0[key] = $5; tfirst[key] = $1; name[key] = $3 }
      last[key] = $4; sc1[key] = $5; tlast[key] = $1 }
    END {
        for (k in first) {
            if (filter != "" && name[k] !~ filter) continue
            span = tlast[k] - tfirst[k]
            if (span <= 0) continue
            pct = (last[k] - first[k]) * 100 / span / 100
            printf "%6.1f%%  %-22s (span %ds", pct, name[k], span
            if (sc0[k] > 0) printf ", schedstat %.1f%%", (sc1[k] - sc0[k]) * 100 / (span * 1e9)
            printf ")\n"
        }
    }' "$f" | sort -rn
