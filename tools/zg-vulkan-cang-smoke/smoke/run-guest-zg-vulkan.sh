#!/bin/sh
# Runs inside the cang microVM under --gpu=drm, from the host runner
# (tools/zg-vulkan-cang-smoke/zg-vulkan-smoke.sh).
#
# It indexes a staged corpus with the image's own `zg` and the pre-installed
# qwen3 GGUF model, explicitly on the Vulkan device, and writes the raw evidence
# the host runner scores: what Vulkan device the guest has, what device ggml
# picked, whether anything fell back to the CPU, whether the model came from the
# image's cache or the network, the index's own duration line, and one query's
# hits.
#
# The guest always exits 0: every step's status goes to evidence/zg-rc.txt, so a
# stall or a crash still leaves the evidence a human can read. Each step is
# bounded by `timeout` - a CPU fallback indexes this corpus in minutes, and that
# has to end as a scored failure rather than as a VM timeout that leaves nothing.
#
# stdin comes from /dev/null for every zg invocation: under the managed PTY a
# guest process that reads stdin stops on SIGTTOU, which looks exactly like a
# wedged GPU.
set -u

E=/workspace/evidence
mkdir -p "$E"
CORPUS=/workspace/corpus

# "INDEX_TIMEOUT MODEL DEVICE": environment variables do not reach the guest
# (cang passes PATH plus its own allowlist), so a staged file is the only channel.
PARAMS="$(cat /workspace/smoke/zg-params 2>/dev/null || echo '900 local/qwen3-embedding-0.6b vulkan')"
# shellcheck disable=SC2086
set -- $PARAMS
INDEX_TIMEOUT="${1:-900}"
MODEL="${2:-local/qwen3-embedding-0.6b}"
DEVICE="${3:-vulkan}"

# Resolved from PATH: the image's agent layer carries `zg` and `vulkan-tools`.
# Neither can be a host binary - the guest's /nix/store is the image's closure.
ZG="$(command -v zg || echo zg)"
VULKANINFO="$(command -v vulkaninfo || echo vulkaninfo)"
CACHE="$HOME/.zvec-grep/models"

: > "$E/zg-rc.txt"

# Unscored diagnostics: the first things to read when a check fails.
{
  echo "uname: $(uname -a)"
  echo "--- /dev/dri ---"
  ls -l /dev/dri 2>&1
  echo "--- vulkan/mesa env ---"
  env | grep -E '^VK_|^MESA_|^CANG_GPU|^ZVEC_GREP' 2>&1
  echo "--- icd manifests ---"
  ls -l /usr/share/vulkan/icd.d 2>&1
  echo "--- zg ---"
  echo "zg: $ZG"
  "$ZG" --version 2>&1
  echo "--- image gpu runtime ---"
  ls -l /usr/lib/cang-gpu-runtime 2>&1
} > "$E/zg-diag.txt" 2>&1

# The guest's own answer to "is there a Vulkan device?", the same probe
# guest-init runs to decide the model/device default.
timeout 60 "$VULKANINFO" --summary > "$E/zg-vulkaninfo.txt" 2>&1
printf 'vulkaninfo=%s\n' "$?" >> "$E/zg-rc.txt"

# The model has to already be in the cache guest-init seeded from the image: this
# guest has no network, and a `zg` that decides to download 610 MiB would fail
# after minutes instead of at this check.
{
  echo "cache=$CACHE"
  find "$CACHE" -name '*.gguf' -o -name 'model.safetensors' 2>/dev/null | while IFS= read -r f; do
    printf '%s -> %s\n' "$f" "$(readlink -f "$f" 2>/dev/null || echo '<unresolved>')"
  done
} > "$E/zg-model.txt" 2>&1

# The index run this smoke is about. RUST_LOG=zg_engine=debug is what makes the
# backend selection a line in the log (`backend=Vulkan gpu=Vulkan0`) instead of
# something the reader has to infer from timings.
timeout "$INDEX_TIMEOUT" env RUST_LOG=zg_engine=debug \
  "$ZG" --index "$CORPUS" --embedding "$MODEL" --device "$DEVICE" \
  < /dev/null > "$E/zg-index.log" 2>&1
printf 'index=%s\n' "$?" >> "$E/zg-rc.txt"

# --check-ready keeps the normal status output and exits non-zero unless the
# index is ready, so one file carries both the numbers and the verdict.
timeout 120 "$ZG" --status "$CORPUS" --check-ready \
  < /dev/null > "$E/zg-status.txt" 2>&1
printf 'status=%s\n' "$?" >> "$E/zg-rc.txt"

# A query always embeds its text through the workspace's model, so this is the
# end-to-end path: the index answers, and the model that answers it is the GPU one
# built from the same GGUF. The text is a verbatim line of the staged corpus
# (staged by the host runner), so the host can check that the file it came from
# ranks first instead of settling for "some hits came back".
QUERY="$(cat /workspace/smoke/zg-query 2>/dev/null)"
printf '%s\n' "$QUERY" > "$E/zg-query-text.txt"
( cd "$CORPUS" && timeout 300 "$ZG" "$QUERY" \
    < /dev/null > "$E/zg-query.txt" 2>&1 )
printf 'query=%s\n' "$?" >> "$E/zg-rc.txt"
