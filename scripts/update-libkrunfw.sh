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
  --tag     newest permanent v<version>-cang.<n> / v<version>-cang-lts.<n>
            release that contains the system's asset. Rolling cang-<sha>
            prereleases are pruned by the fork's CI, so only permanent releases
            should be pinned.

The fork publishes two kernel lines: `cang` (newest kernel, x86_64 assets) and
`cang-lts` (LTS kernel, all architectures). nix/pins.nix records a primary
top-level `tag` - the line cang's own x86_64 asset comes from, and the one the
cang release gate checks - and a system may override it with its own `tag` when
its asset is published on the other line. This script keeps that invariant: a
run for x86_64-linux rewrites the primary tag, a run for another system records
that system's tag only when it differs from the primary.

Because each run rewrites only the selected system, a versioned re-pin has to be
run once per system.

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
  # builds are prereleases, so choose the newest permanent release with the
  # asset from the releases list instead.
  release_tag="$(
    curl -fsSL "https://api.github.com/repos/$owner/$repo/releases?per_page=100" |
      jq -r --arg asset_name "$asset_name" '
        [
          .[]
          | select(.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+-cang(-lts)?\\.[0-9]+$"))
          | select(any(.assets[]?; .name == $asset_name))
        ]
        | sort_by(.published_at // .created_at)
        | last
        | .tag_name // empty
      '
  )"

  if [ -z "$release_tag" ]; then
    echo "failed to determine latest permanent libkrunfw release tag containing $asset_name" >&2
    exit 1
  fi
fi

if ! printf '%s' "$release_tag" | grep -Eq '^(cang-[0-9a-f]{12}|cang-lts-[0-9a-f]{12}|v[0-9]+\.[0-9]+\.[0-9]+-cang\.[0-9]+|v[0-9]+\.[0-9]+\.[0-9]+-cang-lts\.[0-9]+)$'; then
  echo "unsupported libkrunfw release tag: $release_tag (expected cang-<sha> or v<version>-cang.<n>, or their cang-lts counterparts)" >&2
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

lines = block_match.group("body").split("\n")

primary_index = None
for index, line in enumerate(lines):
    if re.fullmatch(r'    tag = "[^"]+";', line):
        primary_index = index
        break
if primary_index is None:
    raise SystemExit("failed to locate the primary libkrunfw release tag in nix/pins.nix")

if system == "x86_64-linux":
    # The primary tag is the line cang's own x86_64 asset comes from (and the
    # one the cang release gate checks), so an x86_64 run moves it.
    lines[primary_index] = f'    tag = "{release_tag}";'
    primary_tag = release_tag
else:
    primary_tag = re.match(r'    tag = "([^"]+)";', lines[primary_index]).group(1)

block_start = None
for index, line in enumerate(lines):
    if line == f"      {system} = {{":
        block_start = index
        break
if block_start is None:
    raise SystemExit(f"failed to locate the {system} block in nix/pins.nix")

block_end = None
for index in range(block_start + 1, len(lines)):
    if lines[index] == "      };":
        block_end = index
        break
if block_end is None:
    raise SystemExit(f"failed to find the end of the {system} block in nix/pins.nix")

system_lines = lines[block_start + 1 : block_end]

asset_count = 0
hash_count = 0
system_tag_lines = []
for line in system_lines:
    if re.fullmatch(r'        tag = "[^"]+";', line):
        continue
    if re.fullmatch(r'        asset = "[^"]+";', line):
        line = f'        asset = "{asset_name}";'
        asset_count += 1
    elif re.fullmatch(r'        hash = "[^"]+";', line):
        line = f'        hash = "{asset_hash}";'
        hash_count += 1
    system_tag_lines.append(line)
if asset_count != 1 or hash_count != 1:
    raise SystemExit(f"failed to rewrite the asset metadata for {system} in nix/pins.nix")

# A system only spells out its own tag when it differs from the primary one;
# equality means "inherit", which keeps a single-line pin compact.
if release_tag != primary_tag:
    system_tag_lines = [f'        tag = "{release_tag}";'] + system_tag_lines

lines = lines[: block_start + 1] + system_tag_lines + lines[block_end:]

# Normalize the other systems against the (possibly new) primary tag.
index = 0
while index < len(lines):
    if re.fullmatch(r"      [a-z0-9_]+ = \{", lines[index]) and lines[index] != f"      {system} = {{":
        end = index + 1
        while end < len(lines) and lines[end] != "      };":
            end += 1
        for inner in range(index + 1, end):
            match = re.fullmatch(r'        tag = "([^"]+)";', lines[inner])
            if match and match.group(1) == primary_tag:
                lines = lines[:inner] + lines[inner + 1 :]
                end -= 1
                break
        index = end
    index += 1

updated_body = "\n".join(lines)
updated = text[: block_match.start("body")] + updated_body + text[block_match.end("body") :]
pins_path.write_text(updated)
PY_EOF

cat <<REPORT_EOF
updated nix/pins.nix:
  system = "$system"
  tag = "$release_tag"
  asset = "$asset_name"
  hash = "$asset_hash"
REPORT_EOF
