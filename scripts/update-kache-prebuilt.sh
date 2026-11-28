#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="kunobi-ninja"
repo="kache"
release_tag=""

usage() {
  cat <<'EOF'
Usage: update-kache-prebuilt.sh [--tag <release-tag>]

Refresh the pinned kache prebuilt release metadata in nix/pins.nix by querying
GitHub Releases for upstream's static-musl tarballs and recomputing their SRI
hashes for all supported Linux systems.

The hash is computed from the asset bytes, and each asset's published `.sha256`
sidecar is verified against it: a pin whose sidecar disagrees is refused rather
than written.

Defaults:
  --tag     the newest published, non-prerelease release
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tag)
      release_tag="${2:?missing value for --tag}"
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

releases_json_file="$(mktemp)"
trap 'rm -f "$releases_json_file"' EXIT
curl -fsSL "https://api.github.com/repos/$owner/$repo/releases?per_page=40" > "$releases_json_file"

python3 - \
  "$pins_file" "$releases_json_file" "$owner" "$repo" "$release_tag" <<'PY'
import base64
import hashlib
import json
import re
import sys
import urllib.request
from pathlib import Path

pins_path = Path(sys.argv[1])
releases = json.loads(Path(sys.argv[2]).read_text())
owner = sys.argv[3]
repo = sys.argv[4]
requested_tag = sys.argv[5]
text = pins_path.read_text()

if requested_tag:
    release = next(
        (item for item in releases if item.get("tag_name") == requested_tag), None
    )
    if release is None or release.get("draft"):
        raise SystemExit(f"{owner}/{repo} has no published release {requested_tag}")
    if release.get("prerelease"):
        raise SystemExit(f"{owner}/{repo} release {requested_tag} is a prerelease")
else:
    release = next(
        (
            item
            for item in releases
            if not item.get("draft") and not item.get("prerelease")
        ),
        None,
    )
    if release is None:
        raise SystemExit(f"{owner}/{repo} publishes no stable release")

release_tag = release["tag_name"]
version = release_tag.removeprefix("v")

assets_by_system = {
    "x86_64-linux": "kache-x86_64-unknown-linux-musl.tar.gz",
    "aarch64-linux": "kache-aarch64-unknown-linux-musl.tar.gz",
}
available_assets = {
    asset["name"]: asset["browser_download_url"]
    for asset in release.get("assets", [])
}


def fetch(url):
    with urllib.request.urlopen(url) as response:
        return response.read()


lines = [
    "  kachePrebuiltRelease = {",
    f'    owner = "{owner}";',
    f'    repo = "{repo}";',
    f'    tag = "{release_tag}";',
    f'    version = "{version}";',
    "    systems = {",
]

for system, asset_name in assets_by_system.items():
    url = available_assets.get(asset_name)
    if url is None:
        raise SystemExit(f"failed to find asset {asset_name} in release {release_tag}")
    body = fetch(url)
    digest = hashlib.sha256(body).digest()
    sidecar = available_assets.get(asset_name + ".sha256")
    if sidecar is None:
        raise SystemExit(
            f"asset {asset_name} has no .sha256 sidecar in release {release_tag}"
        )
    published = fetch(sidecar).decode().split()[0].strip()
    if published != digest.hex():
        raise SystemExit(
            f"{asset_name}: sidecar says {published}, asset bytes hash to "
            f"{digest.hex()}"
        )
    sri = "sha256-" + base64.b64encode(digest).decode()
    lines.extend(
        [
            f"      {system} = {{",
            f'        asset = "{asset_name}";',
            f'        hash = "{sri}";',
            "      };",
        ]
    )

lines.extend(["    };", "  };"])
replacement = "\n".join(lines)
updated, count = re.subn(
    r"  kachePrebuiltRelease = \{.*?\n  \};",
    replacement,
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit(
        "failed to replace kachePrebuiltRelease block; expected exactly one match"
    )

pins_path.write_text(updated)
print("updated nix/pins.nix:")
print(f'  tag = "{release_tag}";')
print(f'  version = "{version}";')
for system, asset_name in assets_by_system.items():
    print(f'  {system}.asset = "{asset_name}";')
print(f"  release published: {release.get('published_at')}")
PY
