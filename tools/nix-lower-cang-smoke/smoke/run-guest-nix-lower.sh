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

# The overlay being mounted is not the same as nix prep having succeeded: a prep
# worker can compose /nix and then fail while swapping the image's Nix DB in, and
# a smoke that only reads mounts would score that green. Wait for the worker to
# finish and keep its own verdict, so the host can require it.
prep=0
while [ "$prep" -lt 300 ]; do
  grep -q '^state=ready' /run/cang/nix-prep.status 2>/dev/null && break
  grep -q '^state=failed' /run/cang/nix-prep.status 2>/dev/null && break
  sleep 1
  prep=$((prep + 1))
done
cp /run/cang/nix-prep.status "$E/nix-prep.status" 2>/dev/null || true
tail -20 /run/cang/nix-prep.log > "$E/nix-prep.log" 2>/dev/null || true
log "nix_prep=$(head -1 "$E/nix-prep.status" 2>/dev/null) waited=${prep}s"

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
# The data path: read the largest store files with the page cache dropped
# first. This is what the block lower exists for, and unlike a directory walk
# it is not dominated by how many entries the store happens to carry.
sync
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true
start=$(date +%s%N)
find /nix/store -maxdepth 3 -type f -size +8M 2>/dev/null | head -20 > "$E/read-files.txt"
while read -r file; do
  [ -n "$file" ] || continue
  dd if="$file" of=/dev/null bs=1M 2>/dev/null
done < "$E/read-files.txt"
end=$(date +%s%N)
echo $(( (end - start) / 1000000 )) > "$E/read-ms.txt"
wc -l < "$E/read-files.txt" > "$E/read-count.txt"
log "find_ms=$(cat "$E/find-ms.txt") du_ms=$(cat "$E/du-ms.txt") read_ms=$(cat "$E/read-ms.txt")"

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
