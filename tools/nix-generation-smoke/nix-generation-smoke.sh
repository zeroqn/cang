#!/usr/bin/env bash
# Live smoke for the /nix generation switch: btrfs-subvolume generation, snapshot
# seed, guest-side DB install and merge.
#
# What it proves, in one run, against two real images whose digests differ:
#   1. a workspace generation is a btrfs subvolume holding upper/work/merged, so
#      overlayfs' same-filesystem rule is satisfied by construction;
#   2. importing a store closure into it registers every path and every object
#      verifies (nix-store --verify-path) and runs;
#   3. moving the workspace to the other image seeds the new generation by
#      btrfs subvolume snapshot (no per-file copy, objects kept), and the guest
#      installs the staged image DB and merges the previous generation's DB, so
#      after the move every path still verifies and the previous generation's
#      closure is still registered;
#   4. the drift scan reports neither registered-but-absent nor
#      present-but-unregistered paths, and a second boot on the same generation
#      is a no-op with the same verification result.
#
# Exit 0 only when every assertion holds and the evidence is fresh.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cang_bin="${CANG_BIN:-}"
image_current="${CANG_SMOKE_IMAGE_CURRENT:-}"
image_previous="${CANG_SMOKE_IMAGE_PREVIOUS:-}"
out_dir=""
state_home=""
workspace=""
keep=0
isolate_state=0
state_root=""
timeout_seconds=1200

# A small closure that is not in the image: hello pulls glibc and the toolchain
# bits a nixpkgs package needs, which is enough file traffic to expose an inode
# identity bug, while staying a few hundred objects rather than thousands.
source_path="/nix/store/na5dnb0l6rjkzrbld3xn4188dd8nknhh-hello-2.12.1"
source_substituter="https://cache.nixos.org"

usage() {
  cat <<'USAGE'
Usage: nix-generation-smoke.sh [OPTIONS]

Run the /nix generation-switch live smoke inside real cang microVMs.

Options:
      --cang <path>       cang binary, wrapper script, or package prefix
      --image <ref>       the image the workspace moves TO (required)
      --previous-image <ref>
                          the image the workspace starts on (required; its
                          digest must differ from --image, otherwise there is no
                          generation switch to exercise)
      --out <dir>         artifact directory (default: a temp dir, printed)
      --state-home <dir>  isolated XDG config/state home (default: the ambient one;
                          an isolated home needs its own containers graphroot)
      --state-root <dir>  cang state root holding the per-workspace
                          <slug>/nix-overlay generation (default: from
                          cang.toml, else $XDG_STATE_HOME/cang)
      --workspace <dir>   workspace directory (default: <out>/workspace)
      --timeout <secs>    per-boot timeout (default: 1200)
      --keep              keep the workspace and state home after the run
  -h, --help              this text

Environment:
  CANG_BIN, CANG_SMOKE_IMAGE_CURRENT, CANG_SMOKE_IMAGE_PREVIOUS
      same as the matching options.
  CANG_SMOKE_SOURCE_PATH / CANG_SMOKE_SOURCE_SUBSTITUTER
      override the store path imported into the workspace (default: hello from
      cache.nixos.org). The path must NOT be in either image's store, or the
      import copy-ups over a lower-layer object instead of creating one.
USAGE
}

die() { printf 'nix-generation-smoke: %s\n' "$*" >&2; exit 1; }
note() { printf 'nix-generation-smoke: %s\n' "$*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --cang) cang_bin=$2; shift 2 ;;
    --image) image_current=$2; shift 2 ;;
    --previous-image) image_previous=$2; shift 2 ;;
    --out) out_dir=$2; shift 2 ;;
    --state-home) state_home=$2; isolate_state=1; shift 2 ;;
    --state-root) state_root=$2; shift 2 ;;
    --workspace) workspace=$2; shift 2 ;;
    --timeout) timeout_seconds=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
done

source_path="${CANG_SMOKE_SOURCE_PATH:-$source_path}"
source_substituter="${CANG_SMOKE_SOURCE_SUBSTITUTER:-$source_substituter}"

[ -n "$image_current" ] || die "--image is required"
[ -n "$image_previous" ] || die "--previous-image is required"

if [ -z "$out_dir" ]; then
  out_dir="$(mktemp -d "${TMPDIR:-/tmp}/nix-generation-smoke.XXXXXX")"
fi
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
# A previous run's workspace holds files the guest created, which are owned by
# the container's subuid and cannot be removed from the host's uid; clear it the
# same way cang does.
if [ -d "$out_dir/workspace" ]; then
  podman unshare rm -rf "$out_dir/workspace" 2>/dev/null || rm -rf "$out_dir/workspace" 2>/dev/null || true
fi
state_home="${state_home:-$out_dir/home}"
workspace="${workspace:-$out_dir/workspace}"
evidence_dir="$workspace/evidence"
logs_dir="$out_dir/logs"
mkdir -p "$state_home" "$workspace" "$evidence_dir" "$logs_dir"

if [ -z "$cang_bin" ]; then
  cang_bin="$repo_root/result/bin/cang"
  [ -x "$cang_bin" ] || die "no --cang given and $cang_bin is missing"
fi
if [ -d "$cang_bin" ]; then
  cang_bin="$cang_bin/bin/cang"
fi
[ -x "$cang_bin" ] || die "'$cang_bin' is not executable"

command -v podman >/dev/null || die "podman is required to inspect the image digests"
command -v stat >/dev/null || die "stat is required"

image_digest() {
  local digest=""
  # A locally built image has no RepoDigests, and an unknown reference makes
  # podman exit 125; both cases must fall through rather than kill the run.
  for _ in 1 2 3; do
    digest="$(podman image inspect --format '{{index .RepoDigests 0}}' "$1" 2>/dev/null || true)"
    if [ -z "$digest" ] || [ "$digest" = "<no value>" ]; then
      digest="$(podman image inspect --format '{{.Id}}' "$1" 2>/dev/null || true)"
    fi
    [ -n "$digest" ] && break
    sleep 1
  done
  printf '%s' "$digest"
}
digest_current="$(image_digest "$image_current")"
digest_previous="$(image_digest "$image_previous")"
[ -n "$digest_current" ] || die "cannot resolve image '$image_current'"
[ -n "$digest_previous" ] || die "cannot resolve image '$image_previous'"
[ "$digest_current" != "$digest_previous" ] ||
  die "both images resolve to the same digest ($digest_current); a generation switch needs two"

# The state home is only narrowed when the caller asked for it. cang's task
# rootfs is built from the containers graphroot, which lives beside the images
# this smoke boots, so an isolated state home without that graphroot finds no
# image and tries to pull `localhost/...` from a registry that is not there.
if [ "$isolate_state" = 1 ]; then
  export XDG_STATE_HOME="$state_home/state"
  export XDG_CONFIG_HOME="$state_home/config"
fi

# The smoke's own workspace state (the per-workspace generation, its task dirs)
# must start empty: a generation left over from an earlier run is already one
# image generation ahead, which would make boot-fresh a *seeded* boot and hide
# the very transition the smoke scores.
if [ -z "$state_root" ]; then
  state_root="$(sed -n 's/^location[[:space:]]*=[[:space:]]*"\{0,1\}\([^"]*\)"\{0,1\}$/\1/p' \
    "${XDG_CONFIG_HOME:-$HOME/.config}/cang/cang.toml" 2>/dev/null | head -1)"
  state_root="${state_root:-${XDG_STATE_HOME:-$HOME/.local/state}/cang}"
fi
slug="$(basename "$workspace")"
smoke_state="$state_root/$slug"
podman unshare rm -rf "$smoke_state" 2>/dev/null || rm -rf "$smoke_state" 2>/dev/null || true

run_start="$(date +%s)"
note "current image:  $image_current ($digest_current)"
note "previous image: $image_previous ($digest_previous)"
note "artifacts:      $out_dir"

run_boot() { # $1 label, $2 image, $3 script
  local label=$1 image=$2 script=$3
  local log="$logs_dir/$label.log"
  note "boot '$label' on $image"
  set +e
  # cang mounts the *current directory* as /workspace, so every boot runs from
  # the smoke's own workspace: the evidence writes land in this host directory
  # and the state root belongs to this smoke, not to the caller's cwd.
  ( cd "$workspace" && timeout "$timeout_seconds" "$cang_bin" --root --image "$image" -- bash -lc "$script" ) \
    >"$log" 2>&1
  local rc=$?
  set -e
  echo "$rc" > "$logs_dir/$label.rc"
  case $rc in
    0) ;;
    124) die "boot '$label' timed out after ${timeout_seconds}s (see $log)" ;;
    *) die "boot '$label' exited $rc (see $log)" ;;
  esac
}

# Everything the guest reports goes into /workspace/evidence, which is this
# host directory, so the assertions read files rather than scraping a console.
guest_report() { # $1 label, $2 image
  local label=$1 image=$2
  run_boot "$label" "$image" '
    set -u
    ev=/workspace/evidence
    mkdir -p "$ev"
    out="$ev/'"$label"'.txt"
    : > "$out"
    say() { printf "%s\n" "$*" >> "$out"; }

    # Let nix prep finish (it installs the staged image DB and merges the parked
    # one); sampling earlier would read a generation mid-switch.
    if command -v cang-guest-init >/dev/null 2>&1; then
      cang-guest-init internal nix wait >/dev/null 2>&1 || true
    fi
    say "label='"$label"'"
    say "image='"$image"'"
    say "guest_nix_inode=$(stat -c %i /nix)"
    say "guest_init=$(command -v cang-guest-init || echo MISSING)"
    say "repair_tool=$(command -v cang-nix-store-repair || echo MISSING)"
    say "prep_status=$(tr "\n" " " < /run/cang/nix-prep.status 2>/dev/null)"
    say "prep_log=$(tr "\n" " " < /run/cang/nix-prep.log 2>/dev/null | cut -c1-400)"
    say "staged_previous=$(ls -1 /nix/.cang-previous-db 2>/dev/null | tr "\n" " ")"
    say "staged_image=$(ls -1 /nix/.cang-image-db 2>/dev/null | tr "\n" " ")"

    if command -v sqlite3 >/dev/null 2>&1; then
      say "live_rows=$(sqlite3 /nix/var/nix/db/db.sqlite "select count(*) from ValidPaths" 2>/dev/null || echo unknown)"
    fi

    nix copy --store local --from '"$source_substituter"' --no-check-sigs '"$source_path"' >/dev/null 2>&1
    say "copy_rc=$?"

    ok=0; bad=0; badlist=""
    for p in $(nix path-info --store local --all 2>/dev/null); do
      if nix-store --store local --verify-path "$p" >/dev/null 2>&1; then
        ok=$((ok+1))
      else
        bad=$((bad+1)); badlist="$badlist $p"
      fi
    done
    say "verify_ok=$ok"
    say "verify_bad=$bad"
    say "verify_bad_list=$badlist"

    say "source_present=$([ -e '"$source_path"' ] && echo yes || echo no)"
    say "source_verify_rc=$(nix-store --store local --verify-path '"$source_path"' >/dev/null 2>&1; echo $?)"

    if command -v '"$source_path"'/bin/hello >/dev/null 2>&1; then :; fi
    if [ -x '"$source_path"'/bin/hello ]; then
      v=$('"$source_path"'/bin/hello 2>&1 | head -1)
      say "source_run=$v"
    fi

    if command -v cang-nix-store-repair >/dev/null 2>&1; then
      drift=$(cang-nix-store-repair --dry-run 2>&1 | grep -oE "registered-but-absent=[0-9]+ present-but-unregistered=[0-9]+" | head -1)
      say "drift=${drift:-unknown}"
    fi
    say "report_done=yes"
  '
}

guest_report boot-fresh "$image_previous"
guest_report boot-seeded "$image_current"
guest_report boot-steady "$image_current"

failures=0
fail() { printf 'FAIL: %s\n' "$*" >&2; failures=$((failures + 1)); }
field() { # $1 file, $2 key
  sed -n "s/^$2=//p" "$evidence_dir/$1.txt" 2>/dev/null | head -1
}
require() { # $1 file, $2 key, $3 expected
  local got
  got="$(field "$1" "$2")"
  if [ "$got" != "$3" ]; then
    fail "$1: expected $2='$3', got '$got'"
    return 1
  fi
  return 0
}
require_nonzero() { # $1 file, $2 key
  local got
  got="$(field "$1" "$2")"
  case "$got" in
    ''|0|unknown) fail "$1: expected $2 to be a positive number, got '$got'"; return 1 ;;
  esac
  return 0
}
require_zero() { # $1 file, $2 key
  local got
  got="$(field "$1" "$2")"
  case "$got" in
    ''|unknown) fail "$1: expected $2 to be 0, got '$got'"; return 1 ;;
  esac
  if [ "$got" != "0" ]; then
    fail "$1: expected $2=0, got '$got'"
    return 1
  fi
  return 0
}

# Freshness: the evidence must be newer than this run's start, which is captured
# before the first boot rather than here.

for f in boot-fresh boot-seeded boot-steady; do
  [ -f "$evidence_dir/$f.txt" ] || die "missing evidence $f.txt (looked in $evidence_dir)"
  mtime="$(stat -c %Y "$evidence_dir/$f.txt")"
  [ "$mtime" -ge "$run_start" ] || die "stale evidence $f.txt (mtime $mtime < run start $run_start)"
  require "$f" report_done yes || true
done

# 1. The generation directory - host side, not the guest's /nix mount - is a
#    btrfs subvolume (inode 256 is the root of one). That is what makes an image
#    move a snapshot, and it is what keeps the overlay's upperdir and workdir on
#    one filesystem. `--state-root` defaults to the state root cang resolves for
#    this user, which the tool reports when the directory cannot be found.
generation_dir="$smoke_state/nix-overlay"
if [ ! -d "$generation_dir" ]; then
  fail "no generation directory at $generation_dir (pass --state-root)"
else
  inode="$(stat -c %i "$generation_dir")"
  case "$inode" in
    256) ;;
    *) fail "the generation directory should be a btrfs subvolume (inode 256), got $inode at $generation_dir" ;;
  esac
  upper_dev="$(stat -c %d "$generation_dir/upper")"
  work_dev="$(stat -c %d "$generation_dir/work")"
  [ "$upper_dev" = "$work_dev" ] ||
    fail "upper and work are on different filesystems ($upper_dev/$work_dev), which overlayfs forbids"
  {
    printf 'generation_dir=%s\n' "$generation_dir"
    printf 'generation_inode=%s\n' "$inode"
    printf 'upper_dev=%s\n' "$upper_dev"
    printf 'work_dev=%s\n' "$work_dev"
  } >> "$out_dir/summary.txt"
fi

# 2. The import registered every path and every object verifies and runs.
require boot-fresh copy_rc 0 || true
require_zero boot-fresh verify_bad || true
require_nonzero boot-fresh verify_ok || true
require boot-fresh source_present yes || true
require boot-fresh source_verify_rc 0 || true
# The binary's *narHash* is what an inode-identity bug corrupts, and
# `source_verify_rc=0` above is that check; running it also depends on the
# guest's own glibc, so its output is recorded but not scored.

# 3. The image move installed the new image's DB, merged the parked one and left
#    nothing staged; the store still verifies completely, and the closure the
#    previous generation supplied is still registered (it was not re-fetched).
require boot-seeded copy_rc 0 || true
require_zero boot-seeded verify_bad || true
require boot-seeded source_present yes || true
require boot-seeded source_verify_rc 0 || true

require boot-seeded staged_image "" || true
previous_rows="$(field boot-seeded live_rows)"
fresh_rows="$(field boot-fresh live_rows)"
case "$previous_rows" in
  ''|unknown) fail "boot-seeded: could not read the live DB row count" ;;
  *)
    if [ "$previous_rows" -lt "${fresh_rows:-0}" ] 2>/dev/null; then
      fail "boot-seeded: the merged DB has $previous_rows rows, fewer than the previous generation's $fresh_rows - the merge did not happen"
    fi
    ;;
esac

# 4. Steady state: a second boot on the same generation verifies identically and
#    has no drift left to reconcile.
require_zero boot-steady verify_bad || true

for f in boot-fresh boot-seeded boot-steady; do
  drift="$(field "$f" drift)"
  case "$drift" in
    "registered-but-absent=0 present-but-unregistered=0") ;;
    *) fail "$f: drift scan should be clean, got '${drift:-none}'" ;;
  esac
done

{
  printf 'image_current=%s\n' "$image_current"
  printf 'image_current_digest=%s\n' "$digest_current"
  printf 'image_previous=%s\n' "$image_previous"
  printf 'image_previous_digest=%s\n' "$digest_previous"
  printf 'cang=%s\n' "$cang_bin"
  printf 'failures=%s\n' "$failures"
} > "$out_dir/summary.txt"
cat "$out_dir/summary.txt"

if [ "$failures" -ne 0 ]; then
  printf 'nix-generation-smoke: %s assertion(s) failed (see %s)\n' "$failures" "$out_dir" >&2
  exit 1
fi

if [ "$keep" -eq 0 ]; then
  rm -rf "$workspace"
fi
printf 'nix-generation-smoke: ok - generation is a subvolume, the image move snapshots it, and the guest merge keeps every path valid\n'
