#!/usr/bin/env bash
# Ticket 06 host runner: boot a real cang microVM from the CURRENT tree and run
# the in-guest proof that the ticket-05 pi wrapper carries libstdc++.so.6.
# Hermetic container storage + isolated XDG config/state, all on the btrfs disk.
set -uo pipefail
D=/home/dev/cang/disk/pi-native-addons-06
CANG=/nix/store/22i0vnx2672phbgvgwjp11rkc76x1z69-cang-0.10.1/bin/cang
GUEST_INIT=/nix/store/1j99c1xhp2jffvi5xphzric6708skzqq-cang-static-x86_64-unknown-linux-musl-0.10.1/bin/cang-guest-init
IMAGE_TAR=/nix/store/l28k4y2fqhhm68l7hhp0gzj0wvv5khy3-cang.tar.gz
# shellcheck disable=SC1091
. "$D/env.sh"
cd "$D/workspace"
DIGEST=$(podman images --digests --format '{{.Digest}}' localhost/cang:latest 2>/dev/null | head -1)
ID=$(podman images --format '{{.ID}}' localhost/cang:latest 2>/dev/null | head -1)
START=$(date +%s)
{
  echo "date-utc: $(date -u +%FT%TZ)"
  echo "repo-head: $(cd /home/dev/cang/cang && git rev-parse HEAD)"
  echo "repo-dirty: $(cd /home/dev/cang/cang && git status --porcelain | tr '\n' ';')"
  echo "cang: $CANG"
  echo "guest-init: $GUEST_INIT"
  echo "image-ref: $CANG_IMAGE"
  echo "image-store-tar: $IMAGE_TAR"
  echo "image-digest: $DIGEST"
  echo "image-id: $ID"
  echo "alloc: <default, no --alloc flag>"
  echo "cwd: $D/workspace"
  echo "launch: $CANG --mem 4 --seccomp=off --landlock=off --guest-init $GUEST_INIT -- sh /workspace/probe.sh"
} > "$D/logs/launch.txt"
timeout 1500 script -q -e -c "$CANG --mem 4 --seccomp=off --landlock=off --guest-init $GUEST_INIT -- sh /workspace/probe.sh" /dev/null > "$D/logs/console.log" 2>&1
rc=$?
echo "vm-exit=$rc" >> "$D/logs/launch.txt"
echo "vm-exit=$rc"
exit 0

