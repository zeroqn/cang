#!/bin/sh
# In-guest half of the /nix smoke. Measures what a launch's /nix promises -
# where it is served from, whether the block pair is engaged, whether prep
# finished, the speed the block lower exists for, and lost pages - and prints
# them as `RESULT key=value` lines on stdout. It never decides PASS/FAIL itself.
#
# The evidence goes to stdout, not to /workspace: a --workspace-disk launch's
# /workspace IS the disk image (a btrfs tree the guest owns), so a file written
# there is invisible to the host, and the host is what scores the run.
set -u
E=/tmp/nix-lower-smoke
rm -rf "$E"
mkdir -p "$E"
: > "$E/trace.txt"
log() { echo "[nix-smoke] $*" >> "$E/trace.txt"; }

echo 8 > /proc/sys/kernel/printk 2>/dev/null || true
uname -r > "$E/kernel.txt"
ls /dev/vd* > "$E/devices.txt" 2>/dev/null || true

# Engaged: /nix is an overlay whose lower is the read-only block image. The
# guest composes that overlay in the background prep worker, which runs beside
# this script, so poll rather than sample once - sampling early reads whatever
# was there before the worker ran and would score a working launch as "not
# engaged".
engaged=no
waited=0
while [ "$waited" -lt 90 ]; do
  if grep -q 'lowerdir=/run/cang/nix-lower' /proc/mounts; then
    engaged=yes
    break
  fi
  sleep 1
  waited=$((waited + 1))
done
log "engaged=$engaged waited_s=$waited"

# The overlay mounted is not the same as prep having succeeded: the worker can
# compose /nix and then fail while swapping the image's Nix DB in, and a smoke
# that only reads mounts would score that green. The prep worker only exists for
# a block pair (a host-composed /nix has none), so only wait for a verdict when
# there is one to wait for.
prep=none
prep_wait=0
if [ "$engaged" = yes ]; then
  while [ "$prep_wait" -lt 300 ]; do
    prep="$(sed -n '1p' /run/cang/nix-prep.status 2>/dev/null)"
    case "$prep" in
      state=ready | state=failed) break ;;
    esac
    sleep 1
    prep_wait=$((prep_wait + 1))
  done
fi

# Speed: the numbers the feature exists for, on the store the image ships.
start=$(date +%s%N)
find /nix/store -name '*.nix' 2>/dev/null | wc -l > "$E/find-count.txt"
end=$(date +%s%N)
echo $(( (end - start) / 1000000 )) > "$E/find-ms.txt"
start=$(date +%s%N)
du -s /nix/store 2>/dev/null | cut -f1 > "$E/du-kb.txt"
end=$(date +%s%N)
echo $(( (end - start) / 1000000 )) > "$E/du-ms.txt"

# The data path: read the largest store files with the page cache dropped
# first, which is what the block lower exists for and is not dominated by how
# many entries the store happens to carry.
sync
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true
find /nix/store -maxdepth 3 -type f -size +8M 2>/dev/null | head -20 > "$E/read-files.txt"
start=$(date +%s%N)
while read -r file; do
  [ -n "$file" ] || continue
  dd if="$file" of=/dev/null bs=1M 2>/dev/null
done < "$E/read-files.txt"
end=$(date +%s%N)
echo $(( (end - start) / 1000000 )) > "$E/read-ms.txt"
wc -l < "$E/read-files.txt" > "$E/read-count.txt"

# Loss: cold reads of the largest store files, twice, with every csum warning
# the guest kernel logs in between. A lost page shows up here (the 7.x btrfs
# read-merge bug) even when nothing else notices.
csum() { dmesg 2>/dev/null | grep -c 'csum failed' || true; }
before=$(csum)
find /nix/store -maxdepth 3 -type f -size +8M 2>/dev/null | head -40 > "$E/files.txt"
sweep() { # sweep <n>
  while read -r file; do
    [ -n "$file" ] || continue
    sha256sum "$file" 2>&1
  done < "$E/files.txt" > "$E/sweep-$1.txt"
}
sweep 1
sync
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true
sweep 2
after=$(csum)
eio1="$(grep -c 'Input/output error' "$E/sweep-1.txt" 2>/dev/null || true)"
eio2="$(grep -c 'Input/output error' "$E/sweep-2.txt" 2>/dev/null || true)"

# ---- the results the host scores -------------------------------------------
# One key per line, so the host can read each of them with a single grep and no
# field splitting: a mount entry carries spaces, and a key=value pair packed
# beside it would not survive that.
nix_mount="$(grep ' /nix ' /proc/mounts 2>/dev/null | tr '\n' '|')"
lower_mount="$(grep ' /run/cang/nix-lower ' /proc/mounts 2>/dev/null | tr '\n' '|')"
echo "RESULT kernel=$(cat "$E/kernel.txt")"
echo "RESULT engaged=$engaged"
echo "RESULT waited_s=$waited"
echo "RESULT prep=$prep"
echo "RESULT prep_wait_s=$prep_wait"
echo "RESULT nix_mount=$nix_mount"
echo "RESULT lower_mount=$lower_mount"
echo "RESULT find_ms=$(cat "$E/find-ms.txt")"
echo "RESULT find_count=$(cat "$E/find-count.txt")"
echo "RESULT du_ms=$(cat "$E/du-ms.txt")"
echo "RESULT du_kb=$(cat "$E/du-kb.txt")"
echo "RESULT read_ms=$(cat "$E/read-ms.txt")"
echo "RESULT read_count=$(cat "$E/read-count.txt")"
echo "RESULT files=$(wc -l < "$E/files.txt")"
echo "RESULT csum_before=$before"
echo "RESULT csum_after=$after"
echo "RESULT eio1=$eio1"
echo "RESULT eio2=$eio2"
echo "RESULT prep_log=$(tail -5 /run/cang/nix-prep.log 2>/dev/null | tr '\n' '|')"
dmesg 2>/dev/null | grep 'csum failed' | sed 's/^/CSUM: /' || true
echo DONE
