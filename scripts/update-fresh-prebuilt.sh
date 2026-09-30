#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="sinelaw"
repo="fresh"
release_tag=""
min_age_days=2

usage() {
  cat <<'EOF'
Usage: update-fresh-prebuilt.sh [--tag <release-tag>]

Refresh the pinned fresh editor prebuilt release metadata in nix/pins.nix by
querying GitHub Releases and recomputing the static-musl release-asset SRI
hashes for all supported Linux systems.

The updater refuses to pin a release younger than two days: without --tag it
selects the newest release published at least two days ago, and with --tag it
verifies the named release is that old too. This keeps a just-published binary
out of the pin until the release has aged.

Defaults:
  --tag     newest release published at least two days ago
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
  "$pins_file" "$releases_json_file" "$owner" "$repo" "$release_tag" "$min_age_days" <<'PY'
import base64
import datetime
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
min_age_days = int(sys.argv[6])
text = pins_path.read_text()

now = datetime.datetime.now(datetime.timezone.utc)
min_age = datetime.timedelta(days=min_age_days)


def age_of(release):
    published = release.get("published_at")
    if not published:
        return None
    moment = datetime.datetime.strptime(published, "%Y-%m-%dT%H:%M:%SZ").replace(
        tzinfo=datetime.timezone.utc
    )
    return now - moment


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
            if not item.get("draft")
            and not item.get("prerelease")
            and age_of(item) is not None
            and age_of(item) >= min_age
        ),
        None,
    )
    if release is None:
        raise SystemExit(
            f"no {owner}/{repo} release is at least {min_age_days} days old"
        )

release_tag = release["tag_name"]
age = age_of(release)
if age is not None and age < min_age:
    raise SystemExit(
        f"{owner}/{repo} release {release_tag} is only "
        f"{age.total_seconds() / 3600:.1f}h old; refusing to pin a release "
        f"younger than {min_age_days} days"
    )

assets_by_system = {
    "x86_64-linux": "fresh-editor-x86_64-unknown-linux-musl.tar.gz",
    "aarch64-linux": "fresh-editor-aarch64-unknown-linux-musl.tar.gz",
}
available_assets = {
    asset["name"]: asset["browser_download_url"]
    for asset in release.get("assets", [])
}
hashes = {}


def sri_hash(url):
    with urllib.request.urlopen(url) as response:
        digest = hashlib.sha256(response.read()).digest()
    return "sha256-" + base64.b64encode(digest).decode()


lines = [
    "  freshPrebuiltRelease = {",
    f'    owner = "{owner}";',
    f'    repo = "{repo}";',
    f'    tag = "{release_tag}";',
    "    systems = {",
]

for system, asset_name in assets_by_system.items():
    url = available_assets.get(asset_name)
    if url is None:
        raise SystemExit(f"failed to find asset {asset_name} in release {release_tag}")
    hashes[system] = sri_hash(url)
    lines.extend(
        [
            f"      {system} = {{",
            f'        asset = "{asset_name}";',
            f'        hash = "{hashes[system]}";',
            "      };",
        ]
    )

lines.extend(["    };", "  };"])
replacement = "\n".join(lines)
updated, count = re.subn(
    r"  freshPrebuiltRelease = \{.*?\n  \};",
    replacement,
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit(
        "failed to replace freshPrebuiltRelease block; expected exactly one match"
    )

pins_path.write_text(updated)
print("updated nix/pins.nix:")
print(f'  tag = "{release_tag}";')
print(f"  release published: {release.get('published_at')} (age {age})")
for system, asset_name in assets_by_system.items():
    print(f'  {system}.asset = "{asset_name}";')
    print(f'  {system}.hash = "{hashes[system]}";')
PY
