#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="pi"
release_tag=""
completed=0
original_pins="$(mktemp)"
cp "$pins_file" "$original_pins"
cleanup() {
  if [ "$completed" -ne 1 ]; then
    cp "$original_pins" "$pins_file"
  fi
  rm -f "$original_pins"
}
trap cleanup EXIT

usage() {
  cat <<'USAGE'
Usage: update-pi-aot.sh [--tag <pi-release-tag>]

Refresh the pinned zeroqn/pi AOT host and the Bun canary it was compiled with
(nix/pins.nix `piAotRelease`) from a `pi-v<version>` release of
https://github.com/zeroqn/pi. That release carries the two per-system assets
`tui/flake.nix` publishes (`tui/nix/manifest.nix` there):

  pi-aot-<version>-<system>.tar.gz    the host, compiled with `bun --aot`
  bun-aot-canary-<date>-<system>.zip  the canary that compiled it

The script records the release tag, the version the asset names carry, the
per-system asset hashes (checked against the digest GitHub reports for each
asset), and then builds `.#bun` from the new pin to read the executable's own
`--version` and `--revision` - the archive's canary-date label says nothing
about either.

Defaults:
  --tag     newest `pi-v*` release of the repository above (the latest release
            overall may be a client release, `tui-v*`)
USAGE
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

for cmd in curl jq nix python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

if [ -z "$release_tag" ]; then
  release_tag="$(
    curl -fsSL "https://api.github.com/repos/$owner/$repo/releases?per_page=100" |
      jq -r '[.[] | select(.tag_name | startswith("pi-v"))][0].tag_name // empty'
  )"
fi

if [ -z "$release_tag" ]; then
  echo "failed to determine a pi release tag; pass --tag explicitly" >&2
  exit 1
fi

release_json="$(curl -fsSL "https://api.github.com/repos/$owner/$repo/releases/tags/$release_tag")"

RELEASE_JSON="$release_json" python3 - "$pins_file" "$release_tag" <<'PY'
import base64
import hashlib
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

pins_path = Path(sys.argv[1])
release_tag = sys.argv[2]
release = json.loads(os.environ["RELEASE_JSON"])
systems = ["x86_64-linux", "aarch64-linux"]


def sri_hash(url, expected_digest):
    with urllib.request.urlopen(url) as response:
        digest = hashlib.sha256(response.read()).hexdigest()
    # GitHub reports the asset's own sha256; a mismatch means the download this
    # script hashed is not the bytes the release describes.
    if expected_digest and expected_digest.removeprefix("sha256:") != digest:
        raise SystemExit(f"{url}: sha256 {digest} does not match the release digest {expected_digest}")
    return "sha256-" + base64.b64encode(bytes.fromhex(digest)).decode()


assets = {asset["name"]: asset for asset in release.get("assets", [])}
versions = set()
lines = [
    "  piAotRelease = {",
    '    owner = "zeroqn";',
    '    repo = "pi";',
    f'    tag = "{release_tag}";',
]

per_system = {}
for system in systems:
    pi_asset = f"pi-aot-{system}"
    found = [name for name in assets if name.startswith("pi-aot-") and name.endswith(f"-{system}.tar.gz")]
    if len(found) != 1:
        raise SystemExit(f"expected one pi-aot tarball for {system} in {release_tag}, found {found}")
    match = re.fullmatch(r"pi-aot-(.+)-" + re.escape(system) + r"\.tar\.gz", found[0])
    if not match:
        raise SystemExit(f"cannot read the version from {found[0]}")
    version = match.group(1)
    versions.add(version)

    bun_found = [name for name in assets if name.startswith("bun-aot-") and name.endswith(f"-{system}.zip")]
    if len(bun_found) != 1:
        raise SystemExit(f"expected one bun-aot archive for {system} in {release_tag}, found {bun_found}")

    per_system[system] = {
        "piAot": (found[0], sri_hash(assets[found[0]]["browser_download_url"], assets[found[0]].get("digest"))),
        "bunAot": (bun_found[0], sri_hash(assets[bun_found[0]]["browser_download_url"], assets[bun_found[0]].get("digest"))),
    }

if len(versions) != 1:
    raise SystemExit(f"the release's pi assets disagree about the version: {sorted(versions)}")
version = versions.pop()

text = pins_path.read_text()
previous = re.search(r'    bunVersion = "(.*?)";\n    bunRevision = "(.*?)";', text)
if not previous:
    raise SystemExit("nix/pins.nix has no bunVersion/bunRevision to keep until the build runs")

lines.extend([
    f'    version = "{version}";',
    # Kept from the previous pin until the built canary answers below.
    f'    bunVersion = "{previous.group(1)}";',
    f'    bunRevision = "{previous.group(2)}";',
    "    systems = {",
])
for system in systems:
    lines.extend([
        f"      {system} = {{",
        "        piAot = {",
        f'          asset = "{per_system[system]["piAot"][0]}";',
        f'          hash = "{per_system[system]["piAot"][1]}";',
        "        };",
        "        bunAot = {",
        f'          asset = "{per_system[system]["bunAot"][0]}";',
        f'          hash = "{per_system[system]["bunAot"][1]}";',
        "        };",
        "      };",
    ])
lines.extend(["    };", "  };"])

updated, count = re.subn(r"  piAotRelease = \{.*?\n  \};", "\n".join(lines), text, count=1, flags=re.S)
if count != 1:
    raise SystemExit("failed to replace piAotRelease block; expected exactly one match")
pins_path.write_text(updated)

print("updated nix/pins.nix:")
print(f'  tag = "{release_tag}";')
print(f'  version = "{version}";')
for system in systems:
    print(f"  {system}.piAot.asset = \"{per_system[system]['piAot'][0]}\";")
    print(f"  {system}.piAot.hash = \"{per_system[system]['piAot'][1]}\";")
    print(f"  {system}.bunAot.asset = \"{per_system[system]['bunAot'][0]}\";")
    print(f"  {system}.bunAot.hash = \"{per_system[system]['bunAot'][1]}\";")
PY

# The canary's own version and revision are what the image asserts, and only the
# built binary can answer: build the package the new pin names and read them.
bun_bin="$(cd "$repo_root" && nix build --no-link --print-out-paths .#bun | tail -1)/bin/bun"
if [ ! -x "$bun_bin" ]; then
  echo "failed to build the newly pinned .#bun; cannot read its version" >&2
  exit 1
fi
bun_version="$("$bun_bin" --version)"
bun_revision="$("$bun_bin" --revision)"

python3 - "$pins_file" "$bun_version" "$bun_revision" <<'PY'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
bun_version = sys.argv[2]
bun_revision = sys.argv[3]
text = pins_path.read_text()
text, version_count = re.subn(
    r'    bunVersion = ".*?";',
    f'    bunVersion = "{bun_version}";',
    text,
    count=1,
)
text, revision_count = re.subn(
    r'    bunRevision = ".*?";',
    f'    bunRevision = "{bun_revision}";',
    text,
    count=1,
)
if version_count != 1 or revision_count != 1:
    raise SystemExit("failed to replace bunVersion/bunRevision in nix/pins.nix")
pins_path.write_text(text)
PY

completed=1
cat <<EOF_SUMMARY
updated nix/pins.nix:
  piAotRelease.bunVersion = "$bun_version";
  piAotRelease.bunRevision = "$bun_revision";
EOF_SUMMARY
