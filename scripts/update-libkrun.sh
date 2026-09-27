#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="libkrun"
release_prefix="cang-"
required_release_systems=("x86_64-linux" "aarch64-linux")

release_tag=""

# Keep both Linux assets pinned from the same release so cang and its images
# all share one coherent libkrun build profile.
declare -A release_assets=(
  ["x86_64-linux"]="libkrun-x86_64-linux-full.tgz"
  ["aarch64-linux"]="libkrun-aarch64-linux-full.tgz"
)

usage() {
  cat <<'USAGE_EOF'
Usage: update-libkrun.sh [--tag <release-tag>]

Refresh the pinned zeroqn/libkrun prebuilt release metadata in nix/pins.nix by
querying GitHub Releases and recomputing the selected release-asset SRI hashes.

Default:
  Select the newest rolling cang-<sha> release that contains every required Linux
  asset and update libkrunRelease.tag plus all asset hashes from that single
  release. Rolling cang-<sha> releases are pruned by the fork's CI.

Options:
  --tag <tag>          Pin a specific release tag instead of auto-selecting the
                       newest complete rolling release. Accepts either a rolling
                       cang-<sha> tag or a permanent v<libkrun version>-cang.<n>
                       release. Tagged cang releases should pin the permanent
                       v*-cang.* release because rolling tags are pruned.

Required release assets:
  libkrun-x86_64-linux-full.tgz
  libkrun-aarch64-linux-full.tgz
USAGE_EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tag|--release-tag)
      release_tag="${2:?missing value for $1}"
      shift 2
      ;;
    --source|--rev|--ref|--system|--prebuilt-release)
      echo "unsupported mode option for prebuilt-only root libkrun: $1" >&2
      usage >&2
      exit 1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

for cmd in curl jq python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

hash_url() {
  python3 - "$1" <<'PY_EOF'
import base64
import hashlib
import sys
import urllib.request

url = sys.argv[1]
with urllib.request.urlopen(url) as response:
    digest = hashlib.sha256(response.read()).digest()
print("sha256-" + base64.b64encode(digest).decode())
PY_EOF
}

required_assets_json="$(
  for required_system in "${required_release_systems[@]}"; do
    printf '%s\n' "${release_assets[$required_system]}"
  done |
    jq -R . |
    jq -s .
)"

if [ -z "$release_tag" ]; then
  release_tag="$(
    curl -fsSL "https://api.github.com/repos/$owner/$repo/releases?per_page=100" |
      jq -r --arg release_prefix "$release_prefix" --argjson required_assets "$required_assets_json" '
        [
          .[]
          | select(.tag_name | startswith($release_prefix))
          | select([.assets[]?.name] as $names | all($required_assets[]; . as $asset | $names | index($asset)))
        ]
        | sort_by(.published_at // .created_at)
        | last
        | .tag_name // empty
      '
  )"
fi

if [ -z "$release_tag" ]; then
  echo "failed to determine latest libkrun release tag containing all required assets:" >&2
  for required_system in "${required_release_systems[@]}"; do
    echo "  ${release_assets[$required_system]}" >&2
  done
  exit 1
fi

if ! printf '%s' "$release_tag" | grep -Eq '^(cang-[0-9a-f]{12}|v[0-9]+\.[0-9]+\.[0-9]+-cang\.[0-9]+)$'; then
  echo "unsupported libkrun release tag: $release_tag (expected cang-<sha> or v<version>-cang.<n>)" >&2
  exit 1
fi

release_json="$(curl -fsSL "https://api.github.com/repos/$owner/$repo/releases/tags/$release_tag")"

tmp_report="$(mktemp)"
trap 'rm -f "$tmp_report"' EXIT

for required_system in "${required_release_systems[@]}"; do
  asset_name="${release_assets[$required_system]}"
  download_url="$(
    printf '%s' "$release_json" |
      jq -r --arg asset_name "$asset_name" '
        .assets[]
        | select(.name == $asset_name)
        | .browser_download_url
      ' |
      head -n 1
  )"

  if [ -z "$download_url" ] || [ "$download_url" = "null" ]; then
    echo "release $release_tag did not contain required asset $asset_name" >&2
    exit 1
  fi

  asset_hash="$(hash_url "$download_url")"
  printf '%s\t%s\t%s\n' "$required_system" "$asset_name" "$asset_hash" >> "$tmp_report"
done

python3 - "$pins_file" "$release_tag" "$tmp_report" <<'PY_EOF'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
release_tag = sys.argv[2]
report_path = Path(sys.argv[3])
text = pins_path.read_text()

block_match = re.search(
    r'libkrunRelease = \{\n(?P<body>.*?)\n  \};',
    text,
    re.S,
)
if block_match is None:
    raise SystemExit("failed to locate libkrunRelease block in nix/pins.nix")

body = block_match.group("body")
body, tag_count = re.subn(r'tag = "[^"]+";', f'tag = "{release_tag}";', body, count=1)
if tag_count != 1:
    raise SystemExit("failed to update libkrun release tag in nix/pins.nix")

for line in report_path.read_text().splitlines():
    system, asset_name, asset_hash = line.split("\t")
    system_pattern = re.compile(
        rf'({re.escape(system)} = \{{\n\s+asset = ")[^"]+?(";\n\s+hash = ")[^"]+?(";)',
        re.S,
    )
    body, system_count = system_pattern.subn(rf'\1{asset_name}\2{asset_hash}\3', body, count=1)
    if system_count != 1:
        raise SystemExit(f"failed to update libkrun release asset metadata for {system}")

updated = text[: block_match.start("body")] + body + text[block_match.end("body") :]
pins_path.write_text(updated)
PY_EOF

cat <<REPORT_EOF
updated nix/pins.nix:
  libkrunRelease.tag = "$release_tag";
REPORT_EOF
while IFS=$'\t' read -r required_system asset_name asset_hash; do
  echo "  $required_system.asset = \"$asset_name\";"
  echo "  $required_system.hash = \"$asset_hash\";"
done < "$tmp_report"
