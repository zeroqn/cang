#!/usr/bin/env bash
# Live smoke for `zg` on the guest GPU.
#
# What it proves. That the image's `zg` indexes a workspace with its GGUF
# embedding model **on the guest's Vulkan device** - mesa's venus through
# virtio-gpu - rather than silently on the CPU, and that the index it built is
# usable. Three things make that "rather than" checkable instead of plausible:
#
#   * the positive signal: ggml enumerates a device (`ggml_vulkan: 0 = <name>`)
#     and zg's own backend choice is a line in the log
#     (`... GPU embedding backend selected backend=Vulkan gpu=Vulkan0`);
#   * the negative one: `falling back to CPU`, `vulkan GPU backend is
#     unavailable`, a failed `Device memory allocation` and
#     `VK_ERROR_OUT_OF_DEVICE_MEMORY` all fail the smoke, which is the failure
#     mode the venus memory window produces when it is too small
#     (docs/graphics-audio.md, "The venus memory window");
#   * the throughput floor: the corpus is generated here, so its cost is fixed,
#     and an index that took an order of magnitude longer than the GPU takes is
#     refused even when every log line looks right.
#
# The model is the image's pre-installed `local/qwen3-embedding-0.6b`; the guest
# has no network, so a run that decides to download it is a failure too
# (`zg-model-local`).
#
# Attribution. Before the run the cang package's render-server helper must
# resolve inside .#virglrenderer, and while the VM is up every
# /proc/<pid>/maps mapping of libvirglrenderer.so.1 is sampled and must resolve
# into that same store path - so a cang linked against some other virglrenderer
# fails the smoke even when the index succeeds.
#
# Exit 0 only when every check passes. Evidence is under <out-dir>/zg/evidence,
# logs under <out-dir>/logs.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cang_bin="${CANG_BIN:-}"
guest_init="${CANG_GUEST_INIT:-}"
container_ref=""
out_dir=""
mem_gib=4
timeout_seconds=1800
# The guest bounds its own index with this; a CPU fallback needs far longer, so
# the bound is what turns "fell back to the CPU" into a scored failure instead of
# a VM timeout that leaves no evidence.
index_timeout=900
# Throughput floor, in embedding tokens per second over the whole run. The default
# corpus is 64 KiB of this repository's Rust sources - ~16 k Qwen3 tokens, measured
# with the Qwen3 BPE tokenizer at 4.10 chars per token, which CHARS_PER_TOKEN_100
# records as 410 to stay in integer arithmetic. A venus run embeds that in ~45 s, of
# which ~15 s is model load plus the SPIR-V pipeline compilation a fresh process
# cannot cache (ggml keeps no pipeline cache on disk), so a whole-run figure lands in
# the hundreds of t/s. A CPU fallback takes minutes for the same text, which is the
# regime the floor is there to refuse.
min_tokens_per_second=100
CHARS_PER_TOKEN_100=410
# The corpus budget, in bytes of this repository's Rust sources. 64 KiB is ~16 k
# Qwen3 tokens: enough that the fixed per-process cost (model load plus the SPIR-V
# pipeline compilation ggml cannot cache) does not dominate the throughput figure,
# and small enough to index in well under a minute on venus.
corpus_bytes=65536
corpus_path="crates/cang/src"
query_max_chars=160
model="local/qwen3-embedding-0.6b"
device="vulkan"

usage() {
  cat <<'USAGE'
Usage: zg-vulkan-smoke.sh [OPTIONS]

Run the live smoke for `zg` on the guest GPU: a --gpu=drm guest indexes a staged
Rust corpus with the image's qwen3 GGUF model on venus, and the run is scored on
the device it used, the absence of a CPU fallback, the index's duration, the
index's readiness and one query's hits, with host-side attribution that
.#virglrenderer served it.

The client is the image's own `zg` and `vulkaninfo` (agent layer). They cannot be
host tools: the guest's /nix/store is the image's closure, so a host store path
does not exist in the guest.

Options:
      --cang <path>          cang binary, wrapper script, or package prefix
                             (default: $CANG_BIN or nix build .#cang)
      --guest-init <path>    guest-init override (default: $CANG_GUEST_INIT, else
                             nix build .#cang-musl -> bin/cang-guest-init)
      --container <path>     OCI archive to load, or an already-loaded image
                             reference (default: nix build .#container)
      --out-dir <path>       output root (default: .smoke/zg-vulkan-cang/<ts>).
                             Must be on btrfs: cang's only implemented task-rootfs
                             backend snapshots the Buildah graphroot.
      --mem <gib>            guest memory (default: 4)
      --timeout <secs>       VM timeout (default: 1800)
      --index-timeout <secs> guest-side bound on the index run (default: 900)
      --min-tokens-per-second <n>
                             throughput floor: the fixed corpus must be embedded
                             at no less than <n> tokens/s (default: 100)
      --corpus-bytes <n>     corpus size: bytes of the repository's Rust sources to
                             stage (default: 65536)
      --corpus-path <path>   source directory the corpus is cut from, relative to
                             the repository root (default: crates/cang/src)
      --embedding <model>    embedding model (default: local/qwen3-embedding-0.6b)
      --device <device>      zg device (default: vulkan)
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
    --out-dir) out_dir="${2:?missing value for --out-dir}"; shift 2 ;;
    --mem) mem_gib="${2:?missing value for --mem}"; shift 2 ;;
    --timeout) timeout_seconds="${2:?missing value for --timeout}"; shift 2 ;;
    --index-timeout) index_timeout="${2:?missing value for --index-timeout}"; shift 2 ;;
    --min-tokens-per-second) min_tokens_per_second="${2:?missing value for --min-tokens-per-second}"; shift 2 ;;
    --corpus-bytes) corpus_bytes="${2:?missing value for --corpus-bytes}"; shift 2 ;;
    --corpus-path) corpus_path="${2:?missing value for --corpus-path}"; shift 2 ;;
    --embedding) model="${2:?missing value for --embedding}"; shift 2 ;;
    --device) device="${2:?missing value for --device}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

die() { echo "FATAL: $*" >&2; exit 2; }
# `set -e` exits silently; say where, so an abrupt end of the log is attributable
# to a line instead of looking like a truncated run.
trap 'echo "FATAL: ${BASH_SOURCE[0]}:${LINENO}: command failed (exit $?)" >&2' ERR

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
# environment; .#cang-prebuilt's wrapper sets it itself. Same rule as the other
# GPU smokes: source the repo-published values, an exported value winning.
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

if [ -z "$out_dir" ]; then
  out_dir="$repo_root/.smoke/zg-vulkan-cang/$(date -u +%Y%m%dT%H%M%SZ)"
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
    echo "       live cang VM needs a btrfs filesystem for --out-dir." >&2
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

# ---- one hermetic store, image loaded once --------------------------------
# An OCI archive path is loaded once into a fresh btrfs-driver store under the
# out-dir: the ambient ~/.config/containers/storage.conf is never read, and the
# btrfs driver is required because btrfs-snapshot needs one subvolume per layer.
#
# An already-loaded image reference keeps the caller's store instead, so the
# (slow, and on a full filesystem fragile) load can be done once by hand and
# reused across runs.
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

# ---- workspace: staged guest script, generated corpus ----------------------
zg_ws="$out_dir/zg/workspace"
# A re-run into the same out-dir must not inherit the previous run's index: `zg`
# would report every file unchanged and the duration would score nothing.
rm -rf "$zg_ws/corpus" "$zg_ws/evidence"
mkdir -p "$zg_ws/smoke" "$zg_ws/corpus/src" "$zg_ws/evidence"
cp "$tool_dir/smoke/run-guest-zg-vulkan.sh" "$zg_ws/smoke/run-guest-zg-vulkan.sh"
chmod +x "$zg_ws/smoke/run-guest-zg-vulkan.sh"

# The corpus is a fixed-size slice of this repository's own Rust sources, taken
# from the *working tree* (not a tag or a release asset) and cut to the same byte
# budget on every run. That is the point: the code under test and the text it
# embeds are the same codebase, and the budget keeps the throughput figure
# comparable from one commit to the next - the sources change, the amount of them
# does not. Each source file contributes at most `budget / 8` bytes (whole lines up
# to a line boundary), so the slice is a spread of files rather than the first big
# one, and the total lands on the budget to within a line.
corpus_src_dir="$repo_root/$corpus_path"
[ -d "$corpus_src_dir" ] || die "corpus path is not a directory: $corpus_src_dir"
per_file=$(( corpus_bytes / 8 ))
[ "$per_file" -ge 4096 ] || per_file=4096
corpus_used=0
corpus_index=0
: > "$zg_ws/corpus-files.txt"
while IFS= read -r src_file; do
  [ "$corpus_used" -lt "$corpus_bytes" ] || break
  remaining=$(( corpus_bytes - corpus_used ))
  cap="$per_file"
  [ "$remaining" -lt "$cap" ] && cap="$remaining"
  name="$(printf '%02d' "$corpus_index")-$(basename "$src_file")"
  dest="$zg_ws/corpus/src/$name"
  awk -v budget="$cap" '
    { line = $0 "\n"; if (used + length(line) > budget) exit; used += length(line); printf "%s", line }
  ' "$src_file" > "$dest"
  produced="$(wc -c < "$dest" | tr -d ' ')"
  if [ "$produced" -gt 0 ]; then
    corpus_used=$(( corpus_used + produced ))
    printf '%s\n' "$name" >> "$zg_ws/corpus-files.txt"
  else
    rm -f "$dest"
  fi
  corpus_index=$(( corpus_index + 1 ))
done < <(find "$corpus_src_dir" -name '*.rs' -type f | sort)
corpus_files_staged="$(wc -l < "$zg_ws/corpus-files.txt" | tr -d ' ')"
corpus_lines_total="$(cat "$zg_ws"/corpus/src/*.rs | wc -l | tr -d ' ')"
[ "$corpus_used" -gt 0 ] || die "staged an empty corpus from $corpus_src_dir"
echo "corpus:          $corpus_src_dir -> $corpus_files_staged files, $corpus_lines_total lines, $corpus_used bytes"

# The query: identifiers that occur in exactly one staged file, so what it should
# retrieve is a fact about the corpus rather than a matter of taste. Every staged
# file's identifier set is dumped, an identifier present in exactly one of those
# sets is unique to that file, and the longest few of the first staged file's
# unique identifiers become the query. The file itself is what the host scores.
ids_dir="$zg_ws/query-ids"
rm -rf "$ids_dir"
mkdir -p "$ids_dir"
staged_index=0
while IFS= read -r staged; do
  # A staged file can hold no identifier this long; that is not a failure.
  grep -oE '[A-Za-z_][A-Za-z0-9_]{5,}' "$zg_ws/corpus/src/$staged" | sort -u > "$ids_dir/$staged_index" || true
  staged_index=$(( staged_index + 1 ))
done < "$zg_ws/corpus-files.txt"
query_file_name="$(head -1 "$zg_ws/corpus-files.txt")"
query_text="$(cat "$ids_dir"/* | sort | uniq -u | comm -12 - <(sort "$ids_dir/0") \
  | awk '{ print length($0) " " $0 }' | sort -rn | head -4 | cut -d' ' -f2- | tr '\n' ' ' | sed 's/ *$//')"
[ -n "$query_text" ] || die "could not derive a unique-identifier query from $query_file_name"
printf '%s' "$query_text" > "$zg_ws/smoke/zg-query"
echo "query:           \"$query_text\" -> $query_file_name"

# The guest has no environment channel (cang passes PATH plus its allowlist), so
# every value it needs is a staged file.
printf '%s %s %s' "$index_timeout" "$model" "$device" > "$zg_ws/smoke/zg-params"
rm -rf "$zg_ws/evidence"
mkdir -p "$zg_ws/evidence"

cang_args=(--mem "$mem_gib" --gpu=drm --alloc hardened --seccomp=off --landlock=off)
[ -n "$guest_init" ] && cang_args+=(--guest-init "$guest_init")

# ---- attribution: sample libvirglrenderer mappings while the VM is up ------
attribution="$out_dir/logs/attribution.txt"
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

echo
echo "== zg on the guest GPU: $corpus_files_staged files / $corpus_lines_total lines / $corpus_used bytes, $model on $device"
launch_s="$(date +%s)"
if ( cd "$zg_ws" && env XDG_CONFIG_HOME="$out_dir/config" XDG_STATE_HOME="$out_dir/state" \
      CANG_IMAGE="$container_ref" \
      timeout "$timeout_seconds" \
      script -q -e -c "$cang_bin ${cang_args[*]} -- sh /workspace/smoke/run-guest-zg-vulkan.sh" /dev/null ) \
    >"$out_dir/logs/cang-zg.console" 2>&1; then
  vm_rc=0
else
  vm_rc=$?
fi
kill "$attribution_pid" 2>/dev/null || true
attribution_pid=""
sort -u "$attribution" > "$attribution.uniq"
mv "$attribution.uniq" "$attribution"

if [ "$vm_rc" -eq 124 ]; then
  echo "FATAL: the zg guest timed out after ${timeout_seconds}s (log: $out_dir/logs/cang-zg.console)" >&2
fi
echo "vm exit: $vm_rc (console: $out_dir/logs/cang-zg.console)"

# ---- scoring ---------------------------------------------------------------
fail=0
E="$zg_ws/evidence"
fresh() { f="$1"; [ -s "$f" ] && [ "$(stat -c %Y "$f")" -ge "$launch_s" ]; }
require() { # require <name> <ok 0|1> <desc> [path]
  if [ "$2" -eq 0 ]; then
    echo "PASS  $1 ($3)"
  else
    echo "FAIL  $1 ($3)${4:+ -> $4}"
    fail=1
  fi
}
note() { echo "        $*"; }
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

require zg-vm-exit "$([ "$vm_rc" -eq 0 ] && echo 0 || echo 1)" \
  "the zg guest ran to completion" "$out_dir/logs/cang-zg.console"

# Every guest step's status, in one file. A missing line means the guest died
# before recording it, which is a failure and not a skip.
steps_ok() {
  local body name
  body="$(cat)"
  for name in vulkaninfo index status query; do
    printf '%s\n' "$body" | grep -qx "$name=0" || return 1
  done
  return 0
}
check zg-guest-steps "$E/zg-rc.txt" \
  "every guest step (vulkaninfo, index, status, query) exited 0" steps_ok

software_names="llvmpipe|lavapipe|swiftshader|softpipe"

# The guest's own answer to "is there a Vulkan device": a deviceName that is not
# a software implementation. Printed so the reader sees which device.
vulkan_device_ok() {
  local body line
  body="$(cat)"
  line="$(printf '%s\n' "$body" | grep -m1 'deviceName' || true)"
  [ -n "$line" ] || return 1
  ! printf '%s' "$line" | grep -Eqi "$software_names"
}
check zg-vulkan-device "$E/zg-vulkaninfo.txt" \
  "vulkaninfo reports a non-software Vulkan device in the guest" vulkan_device_ok
if [ -s "$E/zg-vulkaninfo.txt" ]; then
  grep -m2 -E 'deviceName|driverName' "$E/zg-vulkaninfo.txt" | sed 's/^/        /' || true
fi

# What ggml actually enumerated while indexing - the device the work ran on.
# `ggml_vulkan: 0 = <name> (<driver>) | uma: ...` is llama.cpp's own line.
ggml_device_ok() {
  local body line
  body="$(cat)"
  line="$(printf '%s\n' "$body" | grep -m1 -E '^ggml_vulkan: [0-9]+ = ' || true)"
  [ -n "$line" ] || return 1
  ! printf '%s' "$line" | grep -Eqi "$software_names"
}
check zg-ggml-device "$E/zg-index.log" \
  "ggml enumerated a non-software Vulkan device for the index run" ggml_device_ok
if [ -s "$E/zg-index.log" ]; then
  grep -m1 -E '^ggml_vulkan: [0-9]+ = ' "$E/zg-index.log" | sed 's/^/        /' || true
fi

# zg's own statement of the backend it selected (RUST_LOG=zg_engine=debug). Each
# grep gets the body again: three `grep -q`s sharing one stdin would only ever see
# what the first left unread.
backend_ok() {
  local body
  body="$(cat)"
  printf '%s\n' "$body" | grep -q 'GPU embedding backend selected' || return 1
  printf '%s\n' "$body" | grep -q 'backend=Vulkan' || return 1
  printf '%s\n' "$body" | grep -q 'gpu=Vulkan' || return 1
  return 0
}
check zg-backend-vulkan "$E/zg-index.log" \
  "zg selected the Vulkan embedding backend" backend_ok

# The whole point of the smoke: nothing fell back. These are the lines the venus
# memory window produces when it is too small, plus zg's own fallback warning.
no_fallback_ok() {
  local body marker
  body="$(cat)"
  for marker in 'falling back to CPU' 'vulkan GPU backend is unavailable' \
                'VK_ERROR_OUT_OF_DEVICE_MEMORY' 'Device memory allocation of size' \
                'Failed ggml_vk_create_buffer' 'Failed to allocate pinned memory'; do
    if printf '%s\n' "$body" | grep -qF "$marker"; then
      printf '%s\n' "$body" | grep -m1 -F "$marker" | sed 's/^/        /'
      return 1
    fi
  done
  return 0
}
check zg-no-cpu-fallback "$E/zg-index.log" \
  "no CPU fallback, out-of-device-memory or allocation failure in the index log" no_fallback_ok

# The model has to come from the image's pre-installed cache: the guest has no
# network, and a run that downloads 610 MiB is not what this smoke measures.
local_model_ok() {
  grep -q '\.gguf -> /nix/store/' <<<"$(cat)"
}
check zg-model-local "$E/zg-model.txt" \
  "the GGUF embedding model resolves out of the image's store (no download needed)" local_model_ok
no_download_ok() {
  ! grep -q 'Downloading model' <<<"$(cat)"
}
check zg-no-download "$E/zg-index.log" \
  "the index run downloaded nothing" no_download_ok

# The index is ready and carries the GPU model.
status_ok() {
  local body
  body="$(cat)"
  printf '%s\n' "$body" | grep -q '100%' || return 1
  printf '%s\n' "$body" | grep -qF "$model" || return 1
  return 0
}
check zg-index-ready "$E/zg-status.txt" \
  "the index reports 100% coverage with the $model embedding" status_ok
if [ -s "$E/zg-status.txt" ]; then
  sed 's/^/        /' "$E/zg-status.txt" | head -12
fi

# One query, whose text is a verbatim line of the corpus: the top hit has to be the
# file that line came from. This is the only check that reads what the embeddings
# produced rather than that they were produced at all - an index whose stored model
# reference differs from the one a query resolves still builds and still exits 0.
query_top=""
if fresh "$E/zg-query.txt"; then
  query_top="$(grep -E '^#[1-3] ' "$E/zg-query.txt" || true)"
fi
if printf '%s\n' "$query_top" | grep -qF "src/$query_file_name:"; then
  echo "PASS  zg-query-hits (identifiers unique to $query_file_name retrieve it in the top 3)"
else
  echo "FAIL  zg-query-hits (identifiers unique to $query_file_name must retrieve it in the top 3, got: $(printf '%s' "${query_top:-<no hits>}" | tr '\n' ' ')) -> $E/zg-query.txt"
  fail=1
fi
if [ -s "$E/zg-query.txt" ]; then
  grep -E '^#[1-3] ' "$E/zg-query.txt" | sed 's/^/        /' || true
fi

# Throughput, in the unit the embedding work is actually done in. The summary's
# own duration line is the measured time; the token count is derived from the
# corpus's byte count at the ratio CHARS_PER_TOKEN_100 records, because nothing in
# zg's output counts tokens. A CPU fallback lands an order of magnitude lower.
index_ms() { grep -o '([0-9]*ms)' "$1" 2>/dev/null | head -1 | tr -dc '0-9'; }
corpus_staged_bytes="$(cat "$zg_ws"/corpus/src/*.rs | wc -c | tr -d ' ')"
corpus_tokens=$(( corpus_staged_bytes * 100 / CHARS_PER_TOKEN_100 ))
entities="$(grep -E '^entities' "$E/zg-index.log" 2>/dev/null | head -1 | tr -dc '0-9' || true)"
index_ms_value="$(index_ms "$E/zg-index.log")"
tokens_per_second=0
if [ -n "$index_ms_value" ] && [ "$index_ms_value" -gt 0 ]; then
  tokens_per_second=$(( corpus_tokens * 1000 / index_ms_value ))
fi
if fresh "$E/zg-index.log" && [ -n "$index_ms_value" ] && [ "$tokens_per_second" -ge "$min_tokens_per_second" ]; then
  echo "PASS  zg-throughput ($corpus_tokens tokens over $corpus_staged_bytes bytes indexed in $(( index_ms_value / 1000 ))s = ${tokens_per_second} t/s${entities:+, $entities entities}; floor ${min_tokens_per_second} t/s)"
else
  echo "FAIL  zg-throughput ($corpus_tokens tokens of corpus must embed at >= ${min_tokens_per_second} t/s, got ${tokens_per_second} t/s in ${index_ms_value:-<no duration line>}ms) -> $E/zg-index.log"
  fail=1
fi
if [ -s "$E/zg-index.log" ]; then
  grep -E '^(files|entities|duration)' "$E/zg-index.log" | sed 's/^/        /' || true
fi

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

echo
echo "evidence:"
echo "  guest:  $E"
echo "  logs:   $out_dir/logs"
if [ "$fail" -eq 0 ]; then
  echo "VERDICT: PASS"
else
  echo "VERDICT: FAIL"
fi
exit "$fail"
