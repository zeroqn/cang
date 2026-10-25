#!/bin/bash
# Summarise a mon-threads.sh capture: mean CPU% per thread name over the sampled
# window. Idle time before the VM exists drops out because each thread is measured
# between its own first and last sample.
#
# Usage: summarise-threads.sh <capture> [thread-name-regex]
set -u
f=${1:?usage: summarise-threads.sh <capture> [filter]}
filter=${2:-}
awk -F'|' -v filter="$filter" '
    { key = $2
      if (!(key in first)) { first[key] = $4; tfirst[key] = $1; name[key] = $3 }
      last[key] = $4; tlast[key] = $1 }
    END {
        for (k in first) {
            if (filter != "" && name[k] !~ filter) continue
            span = tlast[k] - tfirst[k]
            if (span <= 0) continue
            printf "%6.1f%%  %-22s (span %ds)\n", (last[k] - first[k]) * 100 / span / 100, name[k], span
        }
    }' "$f" | sort -rn
