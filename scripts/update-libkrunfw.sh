#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="libkrunfw"
system="x86_64-linux"
release_tag=""

usage() {
  cat <<'USAGE_EOF'
Usage: update-libkrunfw.sh [--system <system>] [--tag <release-tag>]

Refresh the pinned zeroqn/libkrunfw release metadata in nix/pins.nix by querying
GitHub Releases and recomputing the selected release-asset SRI hash.

Default:
  --system  x86_64-linux
  --tag     newest rolling cang-<sha> release that contains the system's asset.
            Rolling cang-<sha> releases are pruned by the fork's CI, so tagged
            cang releases should pin the permanent v<libkrunfw version>-cang.<n>
            release instead.

Because each run rewrites only the selected system, a versioned re-pin has to be
run once per system with the same --tag.

Supported systems:
  x86_64-linux, aarch64-linux, riscv64-linux
USAGE_EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --system)
      system="${2:?missing value for --system}"
      shift 2
      ;;
    --tag|--release-tag)
      release_tag="${2:?missing value for $1}"
      shift 2
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

case "$system" in
  x86_64-linux)
    asset_name="libkrunfw-x86_64-kvm-lto.tgz"
    ;;
  aarch64-linux)
    asset_name="libkrunfw-aarch64.tgz"
    ;;
  riscv64-linux)
    asset_name="libkrunfw-riscv64.tgz"
    ;;
  *)
    echo "unsupported system: $system" >&2
    exit 1
    ;;
esac

if [ -z "$release_tag" ]; then
  # GitHub's /releases/latest endpoint ignores prereleases. The cang libkrunfw
  # builds are prereleases, so choose the newest matching asset from the
  # releases list instead.
  release_tag="$(
    curl -fsSL "https://api.github.com/repos/$owner/$repo/releases?per_page=100" |
      jq -r --arg asset_name "$asset_name" '
        [
          .[]
          | select(.tag_name | startswith("cang-"))
          | select(any(.assets[]?; .name == $asset_name))
        ]
        | sort_by(.published_at // .created_at)
        | last
        | .tag_name // empty
      '
  )"

  if [ -z "$release_tag" ]; then
    echo "failed to determine latest libkrunfw release tag containing $asset_name" >&2
    exit 1
  fi
fi

if ! printf '%s' "$release_tag" | grep -Eq '^(cang-[0-9a-f]{12}|v[0-9]+\.[0-9]+\.[0-9]+-cang\.[0-9]+)$'; then
  echo "unsupported libkrunfw release tag: $release_tag (expected cang-<sha> or v<version>-cang.<n>)" >&2
  exit 1
fi

release_api="https://api.github.com/repos/$owner/$repo/releases/tags/$release_tag"
release_json="$(curl -fsSL "$release_api")"
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
  echo "failed to find asset $asset_name in release $release_tag" >&2
  exit 1
fi

asset_hash="$(
  python3 - "$download_url" <<'PY_EOF'
import base64
import hashlib
import sys
import urllib.request

url = sys.argv[1]
with urllib.request.urlopen(url) as response:
    digest = hashlib.sha256(response.read()).digest()
print("sha256-" + base64.b64encode(digest).decode())
PY_EOF
)"

python3 - "$pins_file" "$release_tag" "$system" "$asset_name" "$asset_hash" <<'PY_EOF'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
release_tag = sys.argv[2]
system = sys.argv[3]
asset_name = sys.argv[4]
asset_hash = sys.argv[5]
text = pins_path.read_text()

block_match = re.search(
    r'libkrunfwRelease = \{\n(?P<body>.*?)\n  \};',
    text,
    re.S,
)
if block_match is None:
    raise SystemExit("failed to locate libkrunfwRelease block in nix/pins.nix")

body = block_match.group("body")
body, tag_count = re.subn(r'tag = "[^"]+";', f'tag = "{release_tag}";', body, count=1)
if tag_count != 1:
    raise SystemExit("failed to update libkrunfw release tag in nix/pins.nix")

system_pattern = re.compile(
    rf'({re.escape(system)} = \{{\n\s+asset = ")[^"]+(";\n\s+hash = ")[^"]+(";)',
    re.S,
)
body, system_count = system_pattern.subn(rf'\1{asset_name}\2{asset_hash}\3', body, count=1)
if system_count != 1:
    raise SystemExit(f"failed to update libkrunfw asset metadata for {system} in nix/pins.nix")

updated = text[: block_match.start("body")] + body + text[block_match.end("body") :]
pins_path.write_text(updated)
PY_EOF

cat <<REPORT_EOF
updated nix/pins.nix:
  tag = "$release_tag";
  $system.asset = "$asset_name";
  $system.hash = "$asset_hash";
REPORT_EOF
