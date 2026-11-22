#!/usr/bin/env bash
# Live smoke for the two /nix modes a launch can get. `--workspace-disk` derives
# the block lower (the read-only btrfs image on virtio-blk, with the guest
# composing /nix as an overlay over it), and a launch with no workspace disk keeps
# the host-composed overlay on virtio-fs. Two cold boots, scored on which mode was
# actually engaged, lost pages, and the speed the block lower exists for.
#
# The failure this exists for: btrfs on the 7.x guest kernel merges reads into
# multi-extent bios whose device LBA can belong to another file, so a guest read
# returns EIO (or SIGBUS for a mapping) on pages that are fine on disk. The guest
# reads the store through it; if the block lower silently regresses, this smoke
# fails instead of a user's chromium dying on SIGBUS. The second failure is the
# quiet one: a launch that cannot build the lower keeps the host-composed overlay
# and names the reason on stderr instead of failing, so an unasserted run looks
# clean and is simply the slow path - hence the two boots and the ratio between
# them.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool_dir="$repo_root/tools/nix-lower-cang-smoke"
cang_bin="${CANG_BIN:-}"
guest_init="${CANG_GUEST_INIT:-}"
container_ref=""
out_dir=""
mem_gib=4
timeout_seconds=600
skip_host_arm=0

die() { echo "FATAL: $*" >&2; exit 1; }
say() { echo "== $*"; }

usage() {
  cat <<'USAGE'
Usage: nix-lower-smoke.sh [OPTIONS]

Boots one guest with a --workspace-disk (which derives the /nix block lower) and
one with no workspace disk (which keeps the host-composed overlay on virtio-fs),
and scores which mode was engaged, lost pages, and the speed difference.

Options:
  --cang <path>         cang binary, wrapper script, or package prefix
                        (default: $CANG_BIN or nix build .#cang)
  --guest-init <path>   cang-guest-init to run (default: .#cang-musl's)
  --container <path>    OCI archive to load, or an already-loaded image
                        reference such as localhost/cang:latest
  --out-dir <path>      output root (default: .smoke/nix-lower-cang/<ts>).
                        Must be on btrfs.
  --mem <gib>           guest memory (default: 4)
  --timeout <secs>      per-VM timeout (default: 600)
  --no-host-arm         skip the no-workspace-disk boot (the block arm alone)
  -h, --help            show this help

Prerequisites: /dev/kvm, podman/buildah, script (util-linux), nix, and a btrfs
--out-dir. Each missing prerequisite is a hard failure naming the build command,
never a silent skip.
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --cang) cang_bin="${2:?missing value for --cang}"; shift 2 ;;
    --guest-init) guest_init="${2:?missing value for --guest-init}"; shift 2 ;;
    --container) container_ref="${2:?missing value for --container}"; shift 2 ;;
    --out-dir) out_dir="${2:?missing value for --out-dir}"; shift 2 ;;
    --mem) mem_gib="${2:?missing value for --mem}"; shift 2 ;;
    --timeout) timeout_seconds="${2:?missing value for --timeout}"; shift 2 ;;
    --no-host-arm) skip_host_arm=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# ---- resolve the build under test -----------------------------------------
# Anything this smoke builds is rooted in the output directory before it is used:
# a run is several boots long, and a store GC (an automatic one under disk
# pressure, say) must not pull the binary out from under the last arm.
[ -z "$out_dir" ] && out_dir="$repo_root/.smoke/nix-lower-cang/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out_dir/logs" "$out_dir/roots"
root_build() { # root_build <attr> -> the store path, rooted in the output directory
  local attr="$1" name="${1//\//-}" path
  path="$(nix build "$repo_root#$attr" -o "$out_dir/roots/$name" --print-out-paths 2>/dev/null || true)"
  printf '%s\n' "$path"
}
[ -n "$cang_bin" ] || cang_bin="$(root_build cang)"
[ -n "$cang_bin" ] || die "could not resolve a cang binary; pass --cang <path> or set CANG_BIN"
if [ -d "$cang_bin" ]; then
  [ -x "$cang_bin/bin/cang" ] || die "$cang_bin is neither a cang binary nor a package prefix"
  cang_bin="$cang_bin/bin/cang"
fi
[ -x "$cang_bin" ] || die "cang is not executable: $cang_bin"

if [ -z "$guest_init" ]; then
  musl="$(root_build cang-musl)"
  [ -n "$musl" ] && [ -x "$musl/bin/cang-guest-init" ] && guest_init="$musl/bin/cang-guest-init"
fi
[ -z "$guest_init" ] || [ -x "$guest_init" ] || die "guest-init not executable: $guest_init"

# ---- preflight -------------------------------------------------------------
[ -e /dev/kvm ] || die "/dev/kvm is not accessible"
command -v podman >/dev/null 2>&1 || die "podman is not on PATH (nix develop provides it)"
command -v script >/dev/null 2>&1 || die "script(1) is not on PATH (util-linux)"

mkdir -p "$out_dir/logs" "$out_dir/config/cang"
if [ -n "$container_ref" ] && [ -f "$container_ref" ]; then
  : "${CONTAINERS_STORAGE_CONF:=}"
  if [ -z "$CONTAINERS_STORAGE_CONF" ]; then
    container_storage_dir="$out_dir/container-storage"
    mkdir -p "$container_storage_dir/graph" "$container_storage_dir/run" "$container_storage_dir/tmp"
    cat > "$container_storage_dir/storage.conf" <<EOF
[storage]
driver = "btrfs"
graphroot = "$container_storage_dir/graph"
runroot = "$container_storage_dir/run"
EOF
    export CONTAINERS_STORAGE_CONF="$container_storage_dir/storage.conf"
    export TMPDIR="$container_storage_dir/tmp"
  fi
  say "loading image archive $container_ref"
  podman load -i "$container_ref" >"$out_dir/logs/podman-load.log" 2>&1 || {
    cat "$out_dir/logs/podman-load.log" >&2
    die "podman load failed (log: $out_dir/logs/podman-load.log)"
  }
  container_ref="localhost/cang:latest"
elif [ -n "$container_ref" ]; then
  podman image exists "$container_ref" ||
    die "image $container_ref is not in the current containers storage"
else
  container_ref="localhost/cang:latest"
  podman image exists "$container_ref" ||
    die "no --container given and $container_ref is not loaded;
       load it first: podman load -i \$(nix build $repo_root#container --no-link --print-out-paths)"
fi

fs_type="$(stat -f -c %T "$out_dir" 2>/dev/null || echo unknown)"
case "$fs_type" in
  btrfs) ;;
  *) die "$out_dir is on $fs_type; cang's only task-rootfs backend is btrfs-snapshot and it needs btrfs" ;;
esac

cat > "$out_dir/config/cang/cang.toml" <<EOF
[state]
location = "$out_dir/state"

[task-rootfs]
backend = "btrfs-snapshot"
EOF

# ---- the two boots --------------------------------------------------------
# The two modes a launch can end up in, and the only two the CLI still offers:
#
#   block  a --workspace-disk launch. `/nix` is derived from the workspace disk
#          (34533096: the --nix axis is gone), so the host builds the read-only
#          btrfs lower, attaches it on virtio-blk and the guest composes
#          `/nix` as an overlay over it with its upper inside the tree.
#   host   a launch with no workspace disk: the host composes the overlay itself
#          and shares it, so the guest sees `/nix` on virtio-fs. This is the
#          path every launch without a workspace image still takes.
#
# The guest probe is passed inline rather than as a file in /workspace, because
# a block launch's /workspace is the disk image - a file beside the launch on the
# host is not in it - and the block arm is the one under test. It prints its
# results on stdout and the host scores the console; nothing is read back off the
# guest's filesystem.
probe_b64="$(base64 -w0 "$tool_dir/smoke/run-guest-nix-lower.sh")"
probe_cmd="echo $probe_b64 | base64 -d | sh"

# run_boot <label> [extra env assignment] [extra cang arg]...
run_boot() {
  local label="$1" extra_env="${2:-}" ws rc
  shift 2
  ws="$out_dir/$label/workspace"
  rm -rf "$out_dir/$label"
  mkdir -p "$ws" "$out_dir/workspaces" "$out_dir/logs"

  local args=(--mem "$mem_gib" --alloc hardened --seccomp=off --landlock=off --root)
  [ -n "$guest_init" ] && args+=(--guest-init "$guest_init")
  # Extra args are appended one at a time: a block arm needs `--workspace-disk`
  # and its creation size as two words, and an image inside the launch directory
  # would be hidden by the reverse share, so every image lives beside it.
  while [ "$#" -gt 0 ]; do
    case "$1" in
      WSIMG) args+=(--workspace-disk "$out_dir/workspaces/$label.raw" --workspace-size 16G); shift ;;
      *) args+=("$1"); shift ;;
    esac
  done
  # `%q` so the command survives being joined into script(1)'s `-c` string:
  # without it the shell running that string would split the probe on its
  # own `|` characters and pipe cang's console into the host's base64.
  args+=(-- sh -c "$(printf %q "$probe_cmd")")

  say "boot $label"
  if ( cd "$ws" && env XDG_CONFIG_HOME="$out_dir/config" XDG_STATE_HOME="$out_dir/state" \
        CANG_IMAGE="$container_ref" $extra_env \
        timeout "$timeout_seconds" \
        script -q -e -c "$cang_bin ${args[*]}" /dev/null ) \
      >"$out_dir/logs/cang-$label.console" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  echo "$rc" > "$out_dir/logs/$label.rc"
  say "vm exit ($label): $rc (console: $out_dir/logs/cang-$label.console)"
  return 0
}

run_boot "block" "" WSIMG
if [ "$skip_host_arm" -eq 0 ]; then
  run_boot "host" ""
fi

# ---- scoring ---------------------------------------------------------------
fail=0
require() { # require <name> <ok 0|1> <desc> [detail]
  if [ "$2" -eq 0 ]; then
    echo "PASS  $1 ($3)"
  else
    echo "FAIL  $1 ($3)${4:+ -> $4}"
    fail=1
  fi
}
# Read one `RESULT key=value` line out of an arm's console, with the tty escapes
# script(1) records stripped first. The guest prints its own evidence because a
# block launch's /workspace is the disk image, not a host directory.
value() { # value <label> <key>
  sed 's/\x1b\[[0-9;?]*[a-zA-Z]/ /g' "$out_dir/logs/cang-$1.console" 2>/dev/null |
    tr -d '\r' | grep -m1 "^RESULT $2=" | sed "s|^RESULT $2=||" || true
}
arm_rc() { cat "$out_dir/logs/$1.rc" 2>/dev/null || echo 1; }
# A lost page is a defect in the runtime, not in the launch being scored, so
# CANG_LENIENT_NIX_LOWER_LOSS=1 records the loss as KNOWN instead of failing, for a
# caller that must not be blocked by it (see docs/images-and-storage.md). It never
# hides the count.
lost_check() { # lost_check <name> <label> <desc-prefix>
  local name="$1" label="$2" desc="$3" files csum_before csum_after eio1 eio2
  files="$(value "$label" files)"
  csum_before="$(value "$label" csum_before)"
  csum_after="$(value "$label" csum_after)"
  eio1="$(value "$label" eio1)"
  eio2="$(value "$label" eio2)"
  local summary="files=${files:-?} csum_before=${csum_before:-?} csum_after=${csum_after:-?} eio=${eio1:-?}/${eio2:-?}"
  if [ -n "$files" ] && [ "${files:-0}" -gt 8 ] &&
    [ "${csum_after:-1}" = "${csum_before:-0}" ] &&
    [ "${eio1:-1}" -eq 0 ] && [ "${eio2:-1}" -eq 0 ]; then
    require "$name" 0 "$desc ($summary)"
  elif [ "${CANG_LENIENT_NIX_LOWER_LOSS:-0}" = 1 ] && [ -n "$files" ] && [ "${files:-0}" -gt 8 ]; then
    echo "KNOWN $name: $desc lost a page or saw EIO ($summary);"
    echo "      recorded as KNOWN because CANG_LENIENT_NIX_LOWER_LOSS=1 (guest-kernel read-merge bug)"
  else
    require "$name" 1 "$desc ($summary)"
  fi
}

echo "== checks"
require block-vm-exit "$([ "$(arm_rc block)" = 0 ] && echo 0 || echo 1)" \
  "the --workspace-disk guest ran to completion" "exit $(arm_rc block), $out_dir/logs/cang-block.console"

block_nix="$(value block nix_mount)"
block_lower="$(value block lower_mount)"
require block-lower-engaged \
  "$(printf '%s' "$block_nix" | grep -q 'lowerdir=/run/cang/nix-lower' &&
     printf '%s' "$block_lower" | grep -q ' /run/cang/nix-lower ' && echo 0 || echo 1)" \
  "/nix is an overlay over the read-only block lower (engaged=$(value block engaged))" \
  "$block_lower $block_nix"
require block-prep-ready "$([ "$(value block prep)" = state=ready ] && echo 0 || echo 1)" \
  "/nix prep finished cleanly ($(value block prep) after $(value block prep_wait_s)s)" \
  "$(value block prep_log | tr '|' ' ')"

lost_check block-no-lost-pages block "cold reads of the block arm's store files"

if [ "$skip_host_arm" -eq 0 ]; then
  host_nix="$(value host nix_mount)"
  require host-vm-exit "$([ "$(arm_rc host)" = 0 ] && echo 0 || echo 1)" \
    "the launch without a workspace disk ran to completion" "exit $(arm_rc host), $out_dir/logs/cang-host.console"
  require host-keeps-host-overlay \
    "$(printf '%s' "$host_nix" | grep -q 'virtiofs' &&
       [ "$(value host engaged)" = no ] && echo 0 || echo 1)" \
    "with no workspace disk /nix is the host-composed overlay, and the block pair is not engaged" \
    "$host_nix"
  lost_check host-no-lost-pages host "cold reads of the host arm's store files"

  # The reason the block lower exists: the same store served off a block device
  # instead of virtio-fs. `du -s` walks the store's metadata and block counts -
  # the per-node cost virtio-fs pays a round trip for - and stays meaningful when
  # the host page cache is warm, where a cold data read does not. The bar is
  # 1.3x, not the 4-10x a cold read can show: a busy host shrinks the gap, and
  # this check only has to tell "on the block device" from "on virtio-fs".
  du_block="$(value block du_ms)"
  du_host="$(value host du_ms)"
  find_block="$(value block find_ms)"
  find_host="$(value host find_ms)"
  read_block="$(value block read_ms)"
  read_host="$(value host read_ms)"
  faster=1
  ratio="?"
  if [ "${du_block:-0}" -gt 0 ] && [ "${du_host:-0}" -gt 0 ]; then
    hundredths=$(( du_host * 100 / du_block ))
    ratio="$(( hundredths / 100 )).$(( (hundredths % 100) / 10 ))x"
    if [ $(( du_block * 13 )) -le $(( du_host * 10 )) ]; then
      faster=0
    fi
  fi
  require block-lower-is-faster "$faster" \
    "walking the store is faster on the block lower ($ratio: du ${du_block:-?} ms vs ${du_host:-?} ms; cold read ${read_block:-?} vs ${read_host:-?} ms; find ${find_block:-?} vs ${find_host:-?} ms)"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "VERDICT: PASS"
  echo "  evidence: $out_dir"
  exit 0
fi
echo "VERDICT: FAIL"
echo "  evidence: $out_dir"
echo "  logs: $out_dir/logs/cang-block.console${skip_host_arm:+ }$( [ "$skip_host_arm" -eq 0 ] && echo "$out_dir/logs/cang-host.console")"
exit 1
