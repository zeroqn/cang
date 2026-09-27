#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="cang"
system="x86_64-linux"
release_tag=""

usage() {
  cat <<'USAGE'
Usage: update-cang-prebuilt.sh [--tag <release-tag>] [--system <system>]

Refresh the pinned cang prebuilt release metadata in nix/pins.nix by querying
GitHub Releases, rejecting legacy/concrete-store-referencing payloads, and
recomputing the binary SRI hash.

Defaults:
  --tag     newest rolling sha-<revision> prerelease containing the selected
            cang asset. Pass a permanent v<version> tag (for example
            --tag v0.7.1) to pin an artifact from a versioned release: those
            releases are never pruned, while the release workflow deletes all
            but the 20 newest sha-<revision> prereleases.
  --system  x86_64-linux
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tag)
      release_tag="${2:?missing value for --tag}"
      shift 2
      ;;
    --system)
      system="${2:?missing value for --system}"
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

# Published zeroqn/cang asset names; pre-rename releases still carry the
# historical `loftd-*` prefix and stay resolvable through that name.
case "$system" in
  x86_64-linux)
    asset_arch="x86_64"
    ;;
  aarch64-linux)
    asset_arch="aarch64"
    ;;
  *)
    echo "unsupported system: $system" >&2
    exit 1
    ;;
esac

releases_api="https://api.github.com/repos/$owner/$repo/releases?per_page=100"

# A permanent v<version> release publishes its neutral asset under the versioned
# name cang-v<version>-<arch>-unknown-linux-gnu, while a rolling sha-<revision>
# prerelease made from a branch push carries the unversioned
# cang-<arch>-unknown-linux-gnu name.
if [ -n "$release_tag" ]; then
  case "$release_tag" in
    sha-*)
      asset_name="cang-$asset_arch-unknown-linux-gnu"
      ;;
    v[0-9]*)
      asset_name="cang-$release_tag-$asset_arch-unknown-linux-gnu"
      ;;
    *)
      echo "unsupported release tag: $release_tag" >&2
      echo "expected a rolling sha-<revision> or a permanent v<version> release" >&2
      exit 1
      ;;
  esac
else
  asset_name="cang-$asset_arch-unknown-linux-gnu"
fi

if [[ "$asset_name" == *-linux-flake-locked ]]; then
  echo "internal error: refusing legacy cang flake-locked asset name: $asset_name" >&2
  exit 1
fi

if [ -z "$release_tag" ]; then
  release_tag="$(
    curl -fsSL "$releases_api" |
      jq -r --arg asset_name "$asset_name" '
        map(
          select(.tag_name | startswith("sha-"))
          | select(any(.assets[]?; .name == $asset_name))
        )
        | sort_by(.published_at // .created_at)
        | reverse
        | .[0].tag_name // empty
      '
  )"
fi

if [ -z "$release_tag" ]; then
  echo "failed to determine a sha-* release tag containing $asset_name; pass --tag explicitly after publishing one" >&2
  exit 1
fi

download_url="https://github.com/$owner/$repo/releases/download/$release_tag/$asset_name"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
asset_path="$tmp_dir/$asset_name"

python3 - "$download_url" "$asset_path" "$asset_name" "$release_tag" <<'PY'
import pathlib
import sys
import urllib.request

url = sys.argv[1]
path = pathlib.Path(sys.argv[2])
asset_name = sys.argv[3]
release_tag = sys.argv[4]
try:
    with urllib.request.urlopen(url) as response:
        path.write_bytes(response.read())
except Exception as error:
    raise SystemExit(f"failed to download {asset_name} from {release_tag}: {error}") from error
PY

python3 - "$asset_path" "$asset_name" "$release_tag" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
asset_name = sys.argv[2]
release_tag = sys.argv[3]
data = path.read_bytes()
if data.startswith(b"#!"):
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} is a wrapper script, not raw ELF"
    )
if data[:4] != b"\x7fELF":
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} is not an ELF payload"
    )
if re.search(rb"/nix/store/[0-9a-df-np-sv-z]{32}-", data):
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} contains concrete /nix/store references; publish a neutral cang asset"
    )
PY

asset_hash="$(
  python3 - "$asset_path" <<'PY'
import base64
import hashlib
import pathlib
import sys

digest = hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).digest()
print("sha256-" + base64.b64encode(digest).decode())
PY
)"

python3 - "$pins_file" "$release_tag" "$system" "$asset_name" "$asset_hash" <<'PY'
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
    r'cangPrebuiltRelease = \{\n(?P<body>.*?)\n  \};',
    text,
    re.S,
)
if block_match is None:
    raise SystemExit("failed to locate cangPrebuiltRelease block in nix/pins.nix")

body = block_match.group("body")
body, tag_count = re.subn(r'tag = "[^"]+";', f'tag = "{release_tag}";', body, count=1)
if tag_count != 1:
    raise SystemExit("failed to update cang prebuilt release tag in nix/pins.nix")

system_entry = (
    f'      {system} = {{\n'
    f'        asset = "{asset_name}";\n'
    f'        hash = "{asset_hash}";\n'
    f'      }};'
)

# Match the whole per-system entry so comments inside it (which the
# asset/hash regexes below must not trip over) are preserved.
system_pattern = re.compile(
    rf'(      {re.escape(system)} = \{{\n)(?P<entry>.*?)(\n      \}};)',
    re.S,
)
system_match = system_pattern.search(body)
system_count = 0
if system_match is not None:
    entry = system_match.group("entry")
    entry, asset_count = re.subn(r'asset = "[^"]+";', f'asset = "{asset_name}";', entry, count=1)
    entry, hash_count = re.subn(r'hash = "[^"]+";', f'hash = "{asset_hash}";', entry, count=1)
    if (asset_count, hash_count) != (1, 1):
        raise SystemExit(
            f"failed to update the {system} asset/hash in nix/pins.nix"
        )
    body = body[: system_match.start("entry")] + entry + body[system_match.end("entry") :]
    system_count = 1

if system_count == 0:
    empty_systems = 'systems = { };'
    if empty_systems in body:
        body = body.replace(empty_systems, f'systems = {{\n{system_entry}\n    }};', 1)
    else:
        systems_match = re.search(r'(systems = \{\n)(?P<systems>.*?)(    \};)', body, re.S)
        if systems_match is None:
            raise SystemExit("failed to locate cangPrebuiltRelease.systems block in nix/pins.nix")
        existing = systems_match.group("systems")
        insertion = existing
        if insertion and not insertion.endswith("\n"):
            insertion += "\n"
        insertion += system_entry + "\n"
        body = body[: systems_match.start("systems")] + insertion + body[systems_match.end("systems") :]

updated = text[: block_match.start("body")] + body + text[block_match.end("body") :]
pins_path.write_text(updated)
PY

cat <<EOF_OUT
updated nix/pins.nix:
  tag = "$release_tag";
  $system.asset = "$asset_name";
  $system.hash = "$asset_hash";
EOF_OUT
