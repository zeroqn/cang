#!/bin/sh
# In-guest half of the /nix block-lower smoke. Measures the two things the
# block lower promises (speed and no lost pages) and records them as files the
# host scores; it never decides PASS/FAIL itself.
set -u
E=/workspace/evidence
mkdir -p "$E"
: > "$E/trace.txt"
log() { echo "[nix-lower] $*" >> "$E/trace.txt"; }

echo 8 > /proc/sys/kernel/printk 2>/dev/null || true
uname -r > "$E/kernel.txt"
grep -E ' /nix | /run/cang/nix-lower ' /proc/mounts > "$E/mounts.txt" 2>/dev/null
for attribute in /sys/block/vd*/queue/max_sectors_kb; do
  [ -f "$attribute" ] || continue
  echo "$attribute $(cat "$attribute")" >> "$E/queue.txt"
done
ls /dev/vd* > "$E/devices.txt" 2>/dev/null

# Engaged: a block lower is mounted and /nix is an overlay over it. The guest
# composes that overlay in the background prep worker, which runs beside this
# script, so poll rather than sample once - sampling early reads the
# host-composed fallback and would score a working launch as "not engaged".
engaged=no
waited=0
while [ "$waited" -lt 60 ]; do
  if [ -e /dev/vdc ] && grep -q 'lowerdir=/run/cang/nix-lower' /proc/mounts; then
    engaged=yes
    break
  fi
  sleep 1
  waited=$((waited + 1))
done
echo "engaged=$engaged waited_s=$waited" > "$E/engaged.txt"
grep -E ' /nix | /run/cang/nix-lower ' /proc/mounts > "$E/mounts.txt" 2>/dev/null
log "engaged=$engaged waited_s=$waited"
if [ -r /run/cang/nix-prep.log ] && [ "$engaged" = no ]; then
  tail -20 /run/cang/nix-prep.log > "$E/nix-prep.log" 2>/dev/null || true
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
log "find_ms=$(cat "$E/find-ms.txt") du_ms=$(cat "$E/du-ms.txt")"

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
echo "files=$(wc -l < "$E/files.txt") csum_before=$before csum_after=$after" > "$E/csum.txt"
log "csum_before=$before csum_after=$after files=$(wc -l < "$E/files.txt")"

# An I/O error in a sweep means the same loss, reported differently.
grep -c 'Input/output error' "$E/sweep-1.txt" "$E/sweep-2.txt" > "$E/eio.txt" 2>/dev/null || true
dmesg 2>/dev/null | grep 'csum failed' > "$E/csum-lines.txt" || true
echo DONE >> "$E/trace.txt"
