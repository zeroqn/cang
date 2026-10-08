#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="cang"
system="x86_64-linux"
release_tag="alpha"
version=""
revision=""
asset_name=""
asset_path=""
asset_hash=""

usage() {
  cat <<'USAGE'
Usage: update-mesa-prebuilt.sh [--tag <release-tag>] [--system <system>]
                               [--version <mesa-version>] [--revision <revision>]
                               [--asset <asset-name>] [--asset-path <file>]
                               [--hash <sri>]

Refresh the prebuilt mesa pin in nix/pins.nix. cang publishes the patched mesa
(nix/lib/mesa-patched.nix) as mesa-<version>-<system>.tar.gz from
.github/workflows/build-mesa.yml, on cang's rolling `alpha` prerelease.

By default the asset is downloaded from the release and its SRI hash is
recomputed, the way the other update-*-prebuilt.sh scripts do. Pass --asset-path
to hash a local tarball instead (build-mesa.yml does this right after uploading
one), or --hash to skip hashing entirely.

Defaults:
  --tag       alpha (cang's rolling prerelease, owned by publish_release.yml)
  --system    x86_64-linux
  --version   nix eval --raw .#mesa-release-build.version
  --revision  mesa-<version>
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
    --version)
      version="${2:?missing value for --version}"
      shift 2
      ;;
    --revision)
      revision="${2:?missing value for --revision}"
      shift 2
      ;;
    --asset)
      asset_name="${2:?missing value for --asset}"
      shift 2
      ;;
    --asset-path)
      asset_path="${2:?missing value for --asset-path}"
      shift 2
      ;;
    --hash)
      asset_hash="${2:?missing value for --hash}"
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

case "$system" in
  x86_64-linux|aarch64-linux) ;;
  *)
    echo "unsupported system: $system" >&2
    exit 1
    ;;
esac

for cmd in curl jq python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

if [ -z "$version" ]; then
  if ! command -v nix >/dev/null 2>&1; then
    echo "missing required command: nix (or pass --version explicitly)" >&2
    exit 1
  fi
  version="$(nix eval --raw ".#mesa-release-build.version")"
fi

if [ -z "$version" ]; then
  echo "failed to determine the mesa version; pass --version explicitly" >&2
  exit 1
fi

if [ -z "$revision" ]; then
  revision="mesa-$version"
fi

if [ -z "$asset_name" ]; then
  asset_name="mesa-$version-$system.tar.gz"
fi

if [ -z "$asset_hash" ]; then
  tmp_dir=""
  if [ -z "$asset_path" ]; then
    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT
    asset_path="$tmp_dir/$asset_name"
    download_url="https://github.com/$owner/$repo/releases/download/$release_tag/$asset_name"
    echo "downloading $download_url"
    curl --silent --show-error --fail --location --retry 3 --output "$asset_path" "$download_url"
  fi

  if [ ! -f "$asset_path" ]; then
    echo "asset path does not exist: $asset_path" >&2
    exit 1
  fi

  asset_hash="$(
    python3 - "$asset_path" <<'PY_HASH'
import base64
import hashlib
import pathlib
import sys

digest = hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).digest()
print("sha256-" + base64.b64encode(digest).decode())
PY_HASH
  )"
fi

python3 - "$pins_file" "$release_tag" "$version" "$revision" "$system" "$asset_name" "$asset_hash" <<'PY_EDIT'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
release_tag = sys.argv[2]
version = sys.argv[3]
revision = sys.argv[4]
system = sys.argv[5]
asset_name = sys.argv[6]
asset_hash = sys.argv[7]
text = pins_path.read_text()

block_match = re.search(
    r'(  mesaPrebuiltRelease = \{\n)(?P<body>.*?)(\n  \};)',
    text,
    re.S,
)
if block_match is None:
    raise SystemExit("failed to locate mesaPrebuiltRelease block in nix/pins.nix")

prefix = block_match.group(1)
body = block_match.group(2)
suffix = block_match.group(3)

body, tag_count = re.subn(r'tag = "[^"]*";', f'tag = "{release_tag}";', body, count=1)
if tag_count != 1:
    raise SystemExit("failed to update the mesa prebuilt release tag in nix/pins.nix")

body, version_count = re.subn(r'version = "[^"]*";', f'version = "{version}";', body, count=1)
if version_count != 1:
    raise SystemExit("failed to update the mesa prebuilt release version in nix/pins.nix")

body, revision_count = re.subn(r'revision = "[^"]*";', f'revision = "{revision}";', body, count=1)
if revision_count != 1:
    raise SystemExit("failed to update the mesa prebuilt release revision in nix/pins.nix")

system_entry = (
    f'      {system} = {{\n'
    f'        asset = "{asset_name}";\n'
    f'        hash = "{asset_hash}";\n'
    f'      }};'
)

# The `\n` before each closing-brace pattern anchors the match to a line start,
# so the 4-space `};` cannot match inside a 6-space `      };` system entry.
system_match = re.search(
    rf'(      {re.escape(system)} = \{{\n)(?P<entry>.*?)(\n      \}};)',
    body,
    re.S,
)
system_count = 0
if system_match is not None:
    entry = system_match.group(2)
    entry, asset_count = re.subn(r'asset = "[^"]+";', f'asset = "{asset_name}";', entry, count=1)
    entry, hash_count = re.subn(r'hash = "[^"]+";', f'hash = "{asset_hash}";', entry, count=1)
    if (asset_count, hash_count) != (1, 1):
        raise SystemExit(f"failed to update the {system} asset/hash in nix/pins.nix")
    body = body[: system_match.start()] + system_match.group(1) + entry + system_match.group(3) + body[system_match.end() :]
    system_count = 1

if system_count == 0:
    empty_systems = 'systems = { };'
    if empty_systems in body:
        body = body.replace(empty_systems, f'systems = {{\n{system_entry}\n    }};', 1)
    else:
        systems_match = re.search(r'(systems = \{\n)(?P<systems>.*?)(\n    \};)', body, re.S)
        if systems_match is None:
            raise SystemExit("failed to locate mesaPrebuiltRelease.systems block in nix/pins.nix")
        insertion = systems_match.group(2)
        if insertion and not insertion.endswith("\n"):
            insertion += "\n"
        insertion += system_entry
        body = body[: systems_match.start()] + systems_match.group(1) + insertion + systems_match.group(3) + body[systems_match.end() :]

updated = text[: block_match.start()] + prefix + body + suffix + text[block_match.end() :]
pins_path.write_text(updated)
PY_EDIT

cat <<EOF_OUT
updated nix/pins.nix:
  tag = "$release_tag";
  version = "$version";
  revision = "$revision";
  $system.asset = "$asset_name";
  $system.hash = "$asset_hash";
EOF_OUT
