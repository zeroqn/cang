#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="headless"
ref="main"

usage() {
  cat <<'EOF'
Usage: update-waypipe-prebuilt.sh [--ref <git-ref>]

Refresh the pinned waypipe prebuilt release metadata in nix/pins.nix from
zeroqn/headless's `release-assets.json` - the file its own flake reads and its
`update-release-assets` app writes - which names the waypipe release asset on the
repository's rolling `main-build` prerelease together with the upstream revision
it was built from.

The asset's SRI hash is recomputed from the download rather than copied out of
that file; a disagreement is reported.

Defaults:
  --ref     main (the branch carrying release-assets.json)
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --ref)
      ref="${2:?missing value for --ref}"
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

for cmd in curl python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

metadata_url="https://raw.githubusercontent.com/$owner/$repo/$ref/release-assets.json"
metadata="$(curl --silent --show-error --fail --location --retry 3 "$metadata_url")"

METADATA="$metadata" python3 - "$pins_file" "$metadata_url" <<'PY'
import base64
import hashlib
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

pins_path = Path(sys.argv[1])
metadata_url = sys.argv[2]
metadata = json.loads(os.environ["METADATA"])

waypipe = metadata["packages"]["waypipe"]
release_tag = metadata["release"]["tag"]
text = pins_path.read_text()

def sri_hash(url: str) -> str:
    with urllib.request.urlopen(url) as response:
        digest = hashlib.sha256(response.read()).digest()
    return "sha256-" + base64.b64encode(digest).decode()

def asset_name(url: str) -> str:
    return url.rsplit("/", 1)[-1]

lines = [
    "  waypipePrebuiltRelease = {",
    f'    owner = "{metadata["owner"]}";',
    f'    repo = "{metadata["repo"]}";',
    f'    tag = "{release_tag}";',
    f'    version = "{waypipe["version"]}";',
    f'    revision = "{waypipe["revision"]}";',
    "    systems = {",
]

for system, asset in sorted(waypipe["assets"].items()):
    name = asset_name(asset["url"])
    digest = sri_hash(asset["url"])
    if digest != asset["hash"]:
        print(
            f"warning: {system} asset hash disagrees with {metadata_url}: "
            f"{asset['hash']} (published) vs {digest} (downloaded)",
            file=sys.stderr,
        )
    lines.extend(
        [
            f"      {system} = {{",
            f'        asset = "{name}";',
            f'        hash = "{digest}";',
            "      };",
        ]
    )

lines.extend(["    };", "  };"])
replacement = "\n".join(lines)
updated, count = re.subn(
    r"  waypipePrebuiltRelease = \{.*?\n  \};",
    replacement,
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit(
        "failed to replace waypipePrebuiltRelease block; expected exactly one match"
    )

pins_path.write_text(updated)
print("updated nix/pins.nix:")
print(f'  tag = "{release_tag}";')
print(f'  version = "{waypipe["version"]}";')
print(f'  revision = "{waypipe["revision"]}";')
for system, asset in sorted(waypipe["assets"].items()):
    print(f'  {system}.asset = "{asset_name(asset["url"])}";')
PY
