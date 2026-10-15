#!/usr/bin/env bash
# Live smoke for cang's packaged virglrenderer.
#
# What it proves. That the virglrenderer cang ships - nix/lib/virglrenderer-patched.nix,
# published as .#virglrenderer - is the one doing real work, in both halves of the
# vrend video path and in the GPU path underneath them:
#
#   arm 1  chromium over waypipe: the frozen tools/chromium-cang-smoke --waypipe
#          design, delegated to unchanged. Chromium renders on venus (ANGLE on
#          Vulkan, never SwiftShader) and its frame reaches a host compositor.
#   arm 2  VA-API encode and decode in a --gpu=drm guest: the guest's vainfo must
#          advertise H.264 VLD (decode) and EncSlice (encode); a hardware decode
#          of a software-encoded clip, a hardware encode, and a hardware encode ->
#          hardware decode round trip must all produce exactly the expected frame
#          bytes and match the source per plane (luma AND chroma) above a floor.
#          Its client is the image's own ffmpeg/vainfo (agent layer): the guest's
#          /nix/store is the image's closure, so a host store path cannot run
#          there. The smoke therefore scores what the *shipped image* can do.
#
# Attribution. Arm 2 does not merely run the work and trust that the patched
# build served it. Before the run the cang package's render-server helper is
# resolved and must land inside .#virglrenderer, and during the run every
# /proc/<pid>/maps mapping of libvirglrenderer.so.1 is sampled and must resolve
# into that same store path. A cang built against any other virglrenderer fails
# the smoke even if the encode and decode succeed.
#
# Why the floors are per plane. The 2026-10 chroma bug produced streams whose
# luma was fine and whose chroma was wrong, so luma-only (or size-only) evidence
# would have passed it. The psnr filter reports y/u/v separately and all three
# are scored.
#
# Exit 0 only when every check passes. Evidence is under <out-dir>/; the two arms
# write their own evidence trees and this runner scores both.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
chromium_smoke="$repo_root/tools/chromium-cang-smoke/chromium-smoke.sh"

cang_bin="${CANG_BIN:-}"
guest_init="${CANG_GUEST_INIT:-}"
container_ref=""
weston_bin="${WESTON_BIN:-}"
waypipe_bin="${WAYPIPE_BIN:-}"
python_bin="${PYTHON:-}"
out_dir=""
mem_gib=4
timeout_seconds=900
# 640x360 is the geometry the VA-API investigation used; the guest script reads
# it from a staged file (environment variables do not reach the guest).
geom_w=640
geom_h=360
frames=60
# Floors for the per-plane PSNR of a qp-26 encode/decode against the source.
# Measured values on a working stack are ~40 dB luma and ~44 dB chroma; the
# floors are set well below that so the smoke reports a broken path rather than
# encoder tuning, and high enough that the chroma bug (chroma ~10-15 dB) fails.
psnr_floor_y=30
psnr_floor_chroma=25
skip_chromium=0

usage() {
  cat <<'USAGE'
Usage: virglrenderer-smoke.sh [OPTIONS]

Run the live smoke for cang's packaged virglrenderer: Chromium hardware
acceleration over waypipe (arm 1, delegated to tools/chromium-cang-smoke
--waypipe) and VA-API encode/decode (arm 2, a --gpu=drm guest), with host-side
attribution that the .#virglrenderer build served both.

Arm 2's ffmpeg and vainfo are the image's (agent layer: pkgs.ffmpeg-headless,
pkgs.libva-utils), because the guest's /nix/store is the image's closure and a
host store path does not exist in the guest.

Options:
      --cang <path>          cang binary, wrapper script, or package prefix
                             (default: $CANG_BIN or nix build .#cang)
      --guest-init <path>    guest-init override (default: $CANG_GUEST_INIT, else
                             nix build .#cang-musl -> bin/cang-guest-init)
      --container <path>     OCI archive to load, or an already-loaded image
                             reference (default: nix build .#container)
      --weston <path>        compositor for arm 1 (default: $WESTON_BIN, else
                             `nix build nixpkgs#weston` -> bin/weston)
      --waypipe-bin <path>   waypipe for arm 1 (default: $WAYPIPE_BIN, else PATH)
      --python <path>        python3 for arm 1's pixel check (default: $PYTHON,
                             else `nix build nixpkgs#python3` -> bin/python3)
      --out-dir <path>       output root (default: .smoke/virglrenderer-cang/<ts>).
                             Must be on btrfs: cang's only implemented task-rootfs
                             backend snapshots the Buildah graphroot.
      --mem <gib>            guest memory (default: 4)
      --timeout <secs>       per-VM timeout (default: 900)
      --frames <n>           frames per VA-API arm (default: 60)
      --psnr-floor-y <db>    luma floor for the per-plane PSNR checks (default: 30)
      --psnr-floor-chroma <db>
                             chroma floor (default: 25)
      --skip-chromium        run only the VA-API arm (arm 1 needs weston/waypipe)
  -h, --help                 show this help

Prerequisites: /dev/kvm, a /dev/dri/renderD* node, podman/buildah, script
(util-linux), nix, and a btrfs --out-dir. Each missing prerequisite is a hard
failure naming the build command, never a silent skip.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --cang) cang_bin="${2:?missing value for --cang}"; shift 2 ;;
    --guest-init) guest_init="${2:?missing value for --guest-init}"; shift 2 ;;
    --container) container_ref="${2:?missing value for --container}"; shift 2 ;;
    --weston) weston_bin="${2:?missing value for --weston}"; shift 2 ;;
    --waypipe-bin) waypipe_bin="${2:?missing value for --waypipe-bin}"; shift 2 ;;
    --python) python_bin="${2:?missing value for --python}"; shift 2 ;;
    --out-dir) out_dir="${2:?missing value for --out-dir}"; shift 2 ;;
    --mem) mem_gib="${2:?missing value for --mem}"; shift 2 ;;
    --timeout) timeout_seconds="${2:?missing value for --timeout}"; shift 2 ;;
    --frames) frames="${2:?missing value for --frames}"; shift 2 ;;
    --psnr-floor-y) psnr_floor_y="${2:?missing value for --psnr-floor-y}"; shift 2 ;;
    --psnr-floor-chroma) psnr_floor_chroma="${2:?missing value for --psnr-floor-chroma}"; shift 2 ;;
    --skip-chromium) skip_chromium=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

die() { echo "FATAL: $*" >&2; exit 2; }

# Resolve one binary out of a nix build. `nix build <attr> --print-out-paths` can
# print several outputs (ffmpeg-headless prints its bin and man outputs), so pick
# the one that actually holds the binary.
resolve_bin() { # resolve_bin <installable> <bin-relative-path>
  local installable="$1" rel="$2" out line
  out="$(nix build "$installable" --no-link --print-out-paths 2>/dev/null || true)"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if [ -x "$line/$rel" ]; then
      printf '%s\n' "$line/$rel"
      return 0
    fi
  done <<EOF
$out
EOF
  return 1
}

# ---- cang, guest-init, image, virglrenderer --------------------------------
if [ -z "$cang_bin" ]; then
  cang_root="$(nix build "$repo_root#cang" --no-link --print-out-paths 2>/dev/null || true)"
  [ -n "$cang_root" ] && cang_bin="$cang_root/bin/cang"
fi
if [ -n "$cang_bin" ] && [ -d "$cang_bin" ]; then
  cang_root="$cang_bin"
  cang_bin="$cang_bin/bin/cang"
else
  cang_root="$(dirname "$(dirname "$cang_bin")")"
fi
[ -n "$cang_bin" ] && [ -x "$cang_bin" ] || die "cang binary not resolved or not executable: ${cang_bin:-<none>}
       pass --cang <package prefix | .../bin/cang | wrapper script>"

if [ -z "$guest_init" ]; then
  musl="$(nix build "$repo_root#cang-musl" --no-link --print-out-paths 2>/dev/null || true)"
  [ -n "$musl" ] && [ -x "$musl/bin/cang-guest-init" ] && guest_init="$musl/bin/cang-guest-init"
fi
[ -z "$guest_init" ] || [ -x "$guest_init" ] || die "guest-init not executable: $guest_init"

# The build whose work this smoke attributes. Resolved before anything runs so a
# run against a cang that does not carry it fails here, not three checks later.
virglrenderer_path="$(nix build "$repo_root#virglrenderer" --no-link --print-out-paths 2>/dev/null || true)"
[ -n "$virglrenderer_path" ] || die "could not resolve .#virglrenderer (nix build $repo_root#virglrenderer)"

# The tree-built .#cang is a raw ELF (nix/pkgs/cang-rust.nix), so it finds the
# host mesa/Vulkan loader for virgl_render_server only through the render-server
# environment; .#cang-prebuilt's wrapper sets it itself. Same rule as the
# chromium smoke: source the repo-published values, an exported value winning.
cang_is_wrapper=0
if head -c2 "$cang_bin" | grep -q '#!'; then cang_is_wrapper=1; fi
if [ "$cang_is_wrapper" -eq 0 ]; then
  render_server_env="$(nix build "$repo_root#cang-render-server-env" --no-link --print-out-paths 2>/dev/null || true)"
  if [ -n "$render_server_env" ] && [ -r "$render_server_env/render-server-env.sh" ]; then
    # shellcheck source=/dev/null
    . "$render_server_env/render-server-env.sh"
  fi
fi
if [ "$cang_is_wrapper" -eq 0 ] && [ -z "${CANG_MESA_LIBDIR:-}" ]; then
  die "$cang_bin is a raw ELF and the render-server environment is unset
       nix build $repo_root#cang-render-server-env produced no render-server-env.sh;
       pass a wrapper such as .#cang-prebuilt/bin/cang, or export
       CANG_MESA_LIBDIR/CANG_MESA_ICD/CANG_VULKAN_LOADER_LIBDIR"
fi

# Static attribution: the helper cang execs for the render server has to be the
# one .#virglrenderer installed.
helper="$cang_root/libexec/cang-helpers/virgl_render_server"
helper_resolved=""
if [ -e "$helper" ]; then
  helper_resolved="$(readlink -f "$helper" 2>/dev/null || true)"
fi
helper_ok=1
case "$helper_resolved" in
  "$virglrenderer_path"/*) helper_ok=0 ;;
esac


if [ "$skip_chromium" -eq 0 ]; then
  [ -x "$chromium_smoke" ] || die "arm 1 needs $chromium_smoke"
  [ -n "$weston_bin" ] || weston_bin="$(resolve_bin nixpkgs#weston bin/weston || true)"
  [ -n "$weston_bin" ] && [ -x "$weston_bin" ] || die "arm 1 needs weston; nix build nixpkgs#weston, or pass --weston <path>
       (run with --skip-chromium for the VA-API arm only)"
  if [ -z "$waypipe_bin" ]; then waypipe_bin="$(command -v waypipe || true)"; fi
  [ -n "$waypipe_bin" ] && [ -x "$waypipe_bin" ] || die "arm 1 needs waypipe; nix build .#waypipe-bin (the build the image ships), or pass --waypipe-bin <path>"
  if [ -z "$python_bin" ]; then
    python_bin="$(resolve_bin nixpkgs#python3 bin/python3 || true)"
    [ -n "$python_bin" ] || python_bin="$(command -v python3 || true)"
  fi
  [ -n "$python_bin" ] && [ -x "$python_bin" ] || die "arm 1 needs python3 to read screenshot pixels; pass --python <path>"
fi

if [ -z "$out_dir" ]; then
  out_dir="$repo_root/.smoke/virglrenderer-cang/$(date -u +%Y%m%dT%H%M%SZ)"
fi

# ---- preflight -------------------------------------------------------------
[ -e /dev/kvm ] || die "/dev/kvm is not accessible"
ls /dev/dri/renderD* >/dev/null 2>&1 || die "no /dev/dri/renderD* node; --gpu=drm needs a DRM render node"

mkdir -p "$out_dir/logs" "$out_dir/config/cang" "$out_dir/state" "$out_dir/container-storage/graph"
# cang's only implemented task-rootfs backend is btrfs-snapshot and it snapshots
# the Buildah graphroot, so both the graphroot and the cang state home must be on
# btrfs. Check after mkdir: findmnt --target fails on a path that does not exist.
for required_btrfs_dir in "$out_dir/container-storage/graph" "$out_dir/state"; do
  if ! findmnt -t btrfs --target "$required_btrfs_dir" >/dev/null 2>&1; then
    echo "FATAL: $required_btrfs_dir must be on btrfs; the btrfs-snapshot task-rootfs backend" >&2
    echo "       snapshots the Buildah graphroot and fuse-overlay is not implemented, so a" >&2
    echo "       live cang VM needs a btrfs filesystem for --out-dir (and --state-home)." >&2
    echo "       Mount one first, e.g. the host's btrfs image:" >&2
    echo "         sudo mount -o loop /home/dev/cang/btrfs.img <mountpoint>" >&2
    exit 2
  fi
done

free_kib="$(df -Pk "$out_dir" | awk 'NR==2 {print $4}')"
needed_kib=$((12 * 1024 * 1024))
if [ "${free_kib:-0}" -lt "$needed_kib" ]; then
  die "not enough free space on $out_dir (free ${free_kib:-0} KiB, need ${needed_kib} KiB): the image unpacks to ~7.7 GiB per store"
fi

echo "cang:           $cang_bin"
echo "guest-init:     ${guest_init:-<image default>}"
echo "virglrenderer:  $virglrenderer_path"
echo "render helper:  ${helper_resolved:-<missing>}"
echo "out-dir:        $out_dir"

# ---- one hermetic store, loaded once, shared by both arms ------------------
# An OCI archive path is loaded once into a fresh btrfs-driver store under the
# out-dir, and both arms run against it: the ambient
# ~/.config/containers/storage.conf is never read, and the btrfs driver is
# required because btrfs-snapshot needs one subvolume per layer.
#
# An already-loaded image reference keeps the caller's store instead, so the
# (slow, and on a full filesystem fragile) load can be done once by hand and
# reused across runs:
#   CONTAINERS_STORAGE_CONF=<store>/storage.conf podman load -i .#container
#   CONTAINERS_STORAGE_CONF=<store>/storage.conf virglrenderer-smoke.sh \
#     --container localhost/cang:latest ...
if [ -z "$container_ref" ]; then
  container_ref="$(nix build "$repo_root#container" --no-link --print-out-paths)"
fi
if [[ "$container_ref" == /* ]]; then
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
  echo "loading image archive $container_ref into $container_storage_dir"
  podman load -i "$container_ref" >"$out_dir/logs/podman-load.log" 2>&1 || {
    cat "$out_dir/logs/podman-load.log" >&2
    die "podman load failed (log: $out_dir/logs/podman-load.log)"
  }
  container_ref="localhost/cang:latest"
else
  # A reference means the caller already loaded it: check it is actually there,
  # so a missing image fails here instead of inside the guest.
  echo "using already-loaded image $container_ref (storage: ${CONTAINERS_STORAGE_CONF:-<ambient>})"
  if ! podman image exists "$container_ref"; then
    die "image $container_ref is not in the current containers storage
       load it first, e.g.:
         CONTAINERS_STORAGE_CONF=<store>/storage.conf podman load -i .#container"
  fi
fi

cat > "$out_dir/config/cang/cang.toml" <<EOF
[state]
location = "$out_dir/state"

[task-rootfs]
backend = "btrfs-snapshot"
EOF

cang_args=(--mem "$mem_gib" --gpu=drm --alloc hardened --seccomp=off --landlock=off)
[ -n "$guest_init" ] && cang_args+=(--guest-init "$guest_init")

# ---- arm 1: chromium over waypipe ------------------------------------------
chromium_rc="skipped"
if [ "$skip_chromium" -eq 0 ]; then
  echo
  echo "== arm 1: chromium over waypipe (tools/chromium-cang-smoke --waypipe)"
  chromium_args=(
    --waypipe
    --container "$container_ref"
    --cang "$cang_bin"
    --out-dir "$out_dir/chromium"
    --mem "$mem_gib"
    --timeout "$timeout_seconds"
    --weston "$weston_bin"
    --waypipe-bin "$waypipe_bin"
    --python "$python_bin"
  )
  [ -n "$guest_init" ] && chromium_args+=(--guest-init "$guest_init")
  if "$chromium_smoke" "${chromium_args[@]}" >"$out_dir/logs/chromium-smoke.log" 2>&1; then
    chromium_rc=0
  else
    chromium_rc=$?
  fi
  tail -n 20 "$out_dir/logs/chromium-smoke.log" || true
  echo "arm 1 exit: $chromium_rc (log: $out_dir/logs/chromium-smoke.log)"
fi

# ---- arm 2: VA-API encode/decode in a --gpu=drm guest ----------------------
echo
echo "== arm 2: VA-API encode/decode in a --gpu=drm guest"
vaapi_ws="$out_dir/vaapi/workspace"
mkdir -p "$vaapi_ws/smoke" "$out_dir/logs"
cp "$tool_dir/smoke/run-guest-vaapi.sh" "$vaapi_ws/smoke/run-guest-vaapi.sh"
chmod +x "$vaapi_ws/smoke/run-guest-vaapi.sh"
# The guest has no environment channel (cang passes PATH plus its allowlist), so
# every value it needs is a staged file. The ffmpeg/vainfo clients are not
# staged: the guest's /nix/store is the image's closure, so they have to come
# from the image (its agent layer) - a host store path does not exist in there.
printf '%s %s %s' "$geom_w" "$geom_h" "$frames" > "$vaapi_ws/smoke/vaapi-params"
rm -rf "$vaapi_ws/evidence"
mkdir -p "$vaapi_ws/evidence"

# Attribution: sample every libvirglrenderer.so.1 mapping while the VM is up.
attribution="$out_dir/logs/vaapi-attribution.txt"
: > "$attribution"
sample_attribution() {
  local deadline
  deadline=$(( $(date +%s) + timeout_seconds + 30 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    local maps pid comm
    for maps in /proc/[0-9]*/maps; do
      [ -r "$maps" ] || continue
      grep -q 'libvirglrenderer' "$maps" 2>/dev/null || continue
      pid="${maps#/proc/}"
      pid="${pid%/maps}"
      comm="$(cat "/proc/$pid/comm" 2>/dev/null || echo '?')"
      grep -o '[^ ]*/libvirglrenderer\.so[^ ]*' "$maps" 2>/dev/null | sort -u | while IFS= read -r lib; do
        printf '%s\t%s\t%s\n' "$pid" "$comm" "$lib"
      done
    done >> "$attribution" 2>/dev/null
    sleep 2
  done
}
attribution_pid=""
cleanup() {
  [ -n "$attribution_pid" ] && kill "$attribution_pid" 2>/dev/null || true
  return 0
}
trap cleanup EXIT

sample_attribution &
attribution_pid=$!

launch_s="$(date +%s)"
if ( cd "$vaapi_ws" && env XDG_CONFIG_HOME="$out_dir/config" XDG_STATE_HOME="$out_dir/state" \
      CANG_IMAGE="$container_ref" \
      timeout "$timeout_seconds" \
      script -q -e -c "$cang_bin ${cang_args[*]} -- sh /workspace/smoke/run-guest-vaapi.sh" /dev/null ) \
    >"$out_dir/logs/cang-vaapi.console" 2>&1; then
  vm_rc=0
else
  vm_rc=$?
fi
kill "$attribution_pid" 2>/dev/null || true
attribution_pid=""
sort -u "$attribution" > "$attribution.uniq"
mv "$attribution.uniq" "$attribution"

if [ "$vm_rc" -eq 124 ]; then
  echo "FATAL: the VA-API guest timed out after ${timeout_seconds}s (log: $out_dir/logs/cang-vaapi.console)" >&2
fi
echo "arm 2 exit: $vm_rc (console: $out_dir/logs/cang-vaapi.console)"

# ---- scoring ---------------------------------------------------------------
fail=0
E="$vaapi_ws/evidence"
fresh() { f="$1"; [ -s "$f" ] && [ "$(stat -c %Y "$f")" -ge "$launch_s" ]; }
require() { # require <name> <ok 0|1> <desc> [path]
  if [ "$2" -eq 0 ]; then
    echo "PASS  $1 ($3)"
  else
    echo "FAIL  $1 ($3)${4:+ -> $4}"
    fail=1
  fi
}
check() { # check <name> <path> <desc> <predicate...>
  local name="$1" path="$2" desc="$3"
  shift 3
  if fresh "$path" && "$@" < "$path"; then
    echo "PASS  $name ($desc)"
  else
    echo "FAIL  $name ($desc) -> $path missing/stale/empty or predicate failed"
    fail=1
  fi
}

echo
echo "== checks"

require virglrenderer-helper "$helper_ok" \
  "the cang render-server helper resolves into .#virglrenderer" "$helper_resolved"

if [ "$skip_chromium" -eq 0 ]; then
  if [ "$chromium_rc" = "0" ]; then
    require chromium-waypipe 0 "chromium renders on venus and its frame reaches the host compositor (tools/chromium-cang-smoke --waypipe verdict)"
  else
    require chromium-waypipe 1 "chromium waypipe arm exited 0" "$out_dir/logs/chromium-smoke.log"
  fi
else
  echo "SKIP  chromium-waypipe (--skip-chromium)"
fi

require vaapi-vm-exit "$([ "$vm_rc" -eq 0 ] && echo 0 || echo 1)" \
  "the VA-API guest ran to completion" "$out_dir/logs/cang-vaapi.console"

# The guest-half VA driver must be a real file, not a dangling symlink into a
# collected store path: with one, libva falls back to the unpatched mesa runtime
# and every hardware encode stalls at frame=0 - which is indistinguishable from a
# cang regression.
driver_ok() {
  grep -Eq '^driver=/nix/store/' <<<"$(cat)"
}
check vaapi-guest-driver "$E/vaapi-driver.txt" \
  "the guest's cang VA driver resolves into the store" driver_ok

# Each grep gets the body again: three `grep -q`s sharing one stdin would only
# ever see what the first left unread.
profiles_ok() {
  local body
  body="$(cat)"
  printf '%s\n' "$body" | grep -q VAProfileH264 || return 1
  printf '%s\n' "$body" | grep -q VAEntrypointVLD || return 1
  printf '%s\n' "$body" | grep -q VAEntrypointEncSlice || return 1
  return 0
}
check vaapi-profiles "$E/vaapi-vainfo.txt" \
  "vainfo advertises H.264 VLD (decode) and EncSlice (encode)" profiles_ok

# Every step exit status zero, and every step present (a missing line means the
# guest died before recording it).
rc_ok() {
  local body name
  body="$(cat)"
  for name in source reference decode_vaapi decode_vaapi_content decode_software encode_vaapi encoded_decode_software roundtrip_frames roundtrip_vaapi psnr_decode psnr_encode psnr_roundtrip psnr_decode_hw_sw; do
    printf '%s\n' "$body" | grep -qx "$name=0" || return 1
  done
  return 0
}
check vaapi-steps "$E/vaapi-rc.txt" \
  "every VA-API step exited 0" rc_ok

# Frame-exact byte counts: nv12 is w*h*3/2 per frame. A size mismatch means the
# arm silently produced fewer frames than asked for, whatever its exit status.
expected_bytes=$((geom_w * geom_h * 3 / 2 * frames))
sizes_ok() {
  local body line name got
  body="$(cat)"
  for name in source decoded_vaapi decoded_software encoded_decode_software roundtrip_vaapi; do
    line="$(printf '%s\n' "$body" | grep -x "$name=[0-9]*" || true)"
    [ -n "$line" ] || return 1
    got="${line#*=}"
    [ "$got" -eq "$expected_bytes" ] || return 1
  done
  # Non-empty bitstreams (their exact size is the encoder's business).
  for name in reference encoded_vaapi; do
    line="$(printf '%s\n' "$body" | grep -x "$name=[0-9]*" || true)"
    [ -n "$line" ] || return 1
    got="${line#*=}"
    [ "$got" -gt 0 ] || return 1
  done
  return 0
}
# The two hwframe decodes report their own frame counts; a decoder that silently
# stops early still writes a plausible file, so the count is scored directly.
frames_ok() {
  local body name line got
  body="$(cat)"
  for name in decode_vaapi roundtrip_frames; do
    line="$(printf '%s\n' "$body" | grep -x "$name=[0-9]*" || true)"
    [ -n "$line" ] || return 1
    got="${line#*=}"
    [ "$got" -eq "$frames" ] || return 1
  done
  return 0
}
check vaapi-frames "$E/vaapi-frames.txt" \
  "both hwframe decodes reported exactly $frames decoded frames" frames_ok

# The decoded picture has to equal the software decode of the *same clip*, frame
# for frame, on every plane; anything less means the copy out of the decode surface
# mangled it. Scored per frame (not as an average) because that is how the two
# defects this check was built for presented: a zeroed Cr plane, and an output that
# stopped advancing at the first GOP boundary. Its first thirty frames (a GOP at the
# settings the guest script uses) also catch a regression that the whole-clip
# average would dilute.
first_gop_ok() {
  local body line n fields my mu mv
  body="$(cat)"
  for n in $(seq 1 30); do
    line="$(printf '%s\n' "$body" | grep -m1 "^n:$n ")"
    [ -n "$line" ] || return 1
    my="$(printf '%s' "$line" | sed -n 's/.*mse_y:[\(]*\([-0-9.einf]*\)[)]*.*/\1/p')"
    mu="$(printf '%s' "$line" | sed -n 's/.*mse_u:[\(]*\([-0-9.einf]*\)[)]*.*/\1/p')"
    mv="$(printf '%s' "$line" | sed -n 's/.*mse_v:[\(]*\([-0-9.einf]*\)[)]*.*/\1/p')"
    [ -n "$my" ] && [ -n "$mu" ] && [ -n "$mv" ] || return 1
    awk -v y="$my" -v u="$mu" -v v="$mv" 'BEGIN { exit !(y <= 1 && u <= 1 && v <= 1) }' || return 1
  done
  return 0
}
check vaapi-first-gop "$E/vaapi-psnr-decode_hw_sw-perframe.log" \
  "the hardware decode equals the software decode of the same clip for the first GOP, every plane, every frame" \
  first_gop_ok

check vaapi-frame-bytes "$E/vaapi-sizes.txt" \
  "every frame buffer holds exactly $frames frames of ${geom_w}x${geom_h} nv12 ($expected_bytes bytes) and both bitstreams are non-empty" \
  sizes_ok

# Per-plane PSNR against the source. Reported so a human sees the margin, scored
# against the floors.
psnr_line() { grep -o 'y:[0-9.]* u:[0-9.]* v:[0-9.]*' "$1" | tail -1; }
psnr_ok() {
  local line y u v
  line="$(psnr_line "$1")"
  [ -n "$line" ] || return 1
  y="$(printf '%s' "$line" | sed -n 's/.*y:\([0-9.]*\).*/\1/p')"
  u="$(printf '%s' "$line" | sed -n 's/.*u:\([0-9.]*\).*/\1/p')"
  v="$(printf '%s' "$line" | sed -n 's/.*v:\([0-9.]*\).*/\1/p')"
  awk -v y="$y" -v u="$u" -v v="$v" -v fy="$psnr_floor_y" -v fc="$psnr_floor_chroma" \
    'BEGIN { exit !(y >= fy && u >= fc && v >= fc) }'
}
psnr_check() { # psnr_check <name> <file> <desc>
  local name="$1" file="$2" desc="$3" line
  line="$(psnr_line "$file" 2>/dev/null || true)"
  if fresh "$file" && psnr_ok "$file"; then
    echo "PASS  $name ($desc: ${line:-no PSNR line})"
  else
    echo "FAIL  $name ($desc; need y >= $psnr_floor_y, u/v >= $psnr_floor_chroma, got ${line:-none}) -> $file"
    fail=1
  fi
}
psnr_check vaapi-psnr-decode "$E/vaapi-psnr-decode.txt" \
  "hardware decode of a software-encoded clip matches the source over the whole clip"
psnr_check vaapi-psnr-encode "$E/vaapi-psnr-encode.txt" \
  "the software decode of the hardware-encoded stream matches the source"
psnr_check vaapi-psnr-roundtrip "$E/vaapi-psnr-roundtrip.txt" \
  "the hardware encode -> hardware decode round trip matches the source over the whole clip"

# Live attribution: every libvirglrenderer mapping sampled during the run has to
# be the one .#virglrenderer installed.
attribution_ok() {
  [ -s "$1" ] || return 1
  local lib
  while IFS= read -r lib; do
    [ -n "$lib" ] || continue
    case "$lib" in
      "$virglrenderer_path"/*) ;;
      *) return 1 ;;
    esac
  done < <(cut -f3 "$1" | sort -u)
  return 0
}
if [ -s "$attribution" ] && attribution_ok "$attribution"; then
  echo "PASS  virglrenderer-loaded (every libvirglrenderer.so.1 mapped during the run is from .#virglrenderer)"
  sed 's/^/        /' "$attribution"
else
  echo "FAIL  virglrenderer-loaded (need every mapping under $virglrenderer_path) -> $attribution"
  sed 's/^/        /' "$attribution" 2>/dev/null || true
  fail=1
fi

if [ -s "$E/vaapi-hwdownload.txt" ]; then
  echo "INFO  hwdownload (unscored): $(cat "$E/vaapi-hwdownload.txt")"
fi

echo
echo "evidence:"
echo "  vaapi:     $E"
if [ "$skip_chromium" -eq 0 ]; then
  echo "  chromium:  $out_dir/chromium/workspace/evidence"
fi
echo "  logs:      $out_dir/logs"
if [ "$fail" -eq 0 ]; then
  echo "VERDICT: PASS"
else
  echo "VERDICT: FAIL"
fi
exit "$fail"
