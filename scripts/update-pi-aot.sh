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
(nix/pins.nix `piAotRelease`) from the per-system entries of that repository's
`tui/nix/manifest.nix`, which is what its `tui/flake.nix` publishes:

  pi-aot-<version>-<system>.tar.gz     the host, compiled with `bun --aot`
  bun-aot-canary-<date>-<system>.zip   the canary that compiled it

The manifest is what picks the bytes: a release accumulates assets, so its own
asset list cannot say which canary pairs with which `pi-aot` (pi-v1.6.0 carries
two x86_64 canaries, 20261123 and 20261124). The script records the release tag,
the version the `pi-aot` asset names carry, the per-system asset hashes - each
download checked against the manifest's SRI hash *and* the digest GitHub reports
for the asset - and then builds `.#bun` from the new pin to read the executable's
own `--version` and `--revision`; the archive's canary-date label says nothing
about either.

A system the manifest lists is a system the pin lists. The manifest currently
carries x86_64-linux alone, so the pin does too, and `pi`/`bun` refuse to build
for a system without an entry.

Defaults:
  --tag     the `pi-v*` release the manifest's asset URLs name. Passing another
            tag is refused: the manifest lives on `main`, so it describes the
            newest release, not an older one.
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

for cmd in curl nix python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

# The manifest is a plain attrset, so `import` of the fetched file is the parse:
# nothing here reads Nix source by hand. It is on `main` rather than at the
# release tag, because a release's own tree still carries the empty stub.
manifest_json="$(
  nix eval --impure --json --expr \
    "import (builtins.fetchurl { url = \"https://raw.githubusercontent.com/$owner/$repo/main/tui/nix/manifest.nix\"; })"
)"

manifest_tag="$(
  MANIFEST_JSON="$manifest_json" python3 - <<'PY' || exit 1
import json
import os

manifest = json.loads(os.environ["MANIFEST_JSON"])
if not manifest:
    raise SystemExit("the repository's tui/nix/manifest.nix is empty: no AOT asset is published yet")
tags = {
    entry[artifact]["url"].split("/download/")[1].split("/")[0]
    for entry in manifest.values()
    for artifact in ("piAot", "bunAot")
}
if len(tags) != 1:
    raise SystemExit(f"the manifest's assets disagree about their release: {sorted(tags)}")
print(tags.pop())
PY
)"

if [ -z "$release_tag" ]; then
  release_tag="$manifest_tag"
elif [ "$release_tag" != "$manifest_tag" ]; then
  echo "the manifest on main describes $manifest_tag, not $release_tag: it is rewritten at each release, so an older release cannot be pinned from it" >&2
  exit 1
fi

release_json="$(curl -fsSL "https://api.github.com/repos/$owner/$repo/releases/tags/$release_tag")"

MANIFEST_JSON="$manifest_json" RELEASE_JSON="$release_json" python3 - "$pins_file" "$release_tag" <<'PY'
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
manifest = json.loads(os.environ["MANIFEST_JSON"])
release = json.loads(os.environ["RELEASE_JSON"])


def sha256_of(url):
    with urllib.request.urlopen(url) as response:
        return hashlib.sha256(response.read()).hexdigest()


assets = {asset["name"]: asset for asset in release.get("assets", [])}
per_system = {}
versions = set()
for system, entry in manifest.items():
    pinned = {}
    for artifact in ("piAot", "bunAot"):
        url = entry.get(artifact, {}).get("url")
        if not url:
            raise SystemExit(f"the manifest's {system} entry has no {artifact}")
        # The manifest is the authority for which bytes pair with which: a
        # release's asset list keeps every version it ever published.
        asset = url.rsplit("/", 1)[1]
        if f"/download/{release_tag}/" not in url:
            raise SystemExit(f"the manifest's {system} {artifact} is not an asset of {release_tag}: {url}")
        if asset not in assets:
            raise SystemExit(f"{release_tag} publishes no asset named {asset}")
        digest = sha256_of(url)
        sri = "sha256-" + base64.b64encode(bytes.fromhex(digest)).decode()
        if sri != entry[artifact]["hash"]:
            raise SystemExit(f"{url}: sha256 {digest} does not match the manifest hash {entry[artifact]['hash']}")
        # GitHub's digest for that asset: the download is the bytes the release
        # published, not a same-named replacement.
        if assets[asset].get("digest") and assets[asset]["digest"].removeprefix("sha256:") != digest:
            raise SystemExit(f"{url}: sha256 {digest} does not match the release digest {assets[asset]['digest']}")
        pinned[artifact] = (asset, sri)
    per_system[system] = pinned
    versions.add(entry["piAot"].get("version") or "")

if "" in versions or len(versions) != 1:
    raise SystemExit(f"the manifest's pi assets carry no single version: {sorted(versions)}")
version = versions.pop()

text = pins_path.read_text()
previous = re.search(r'    bunVersion = "(.*?)";\n    bunRevision = "(.*?)";', text)
if not previous:
    raise SystemExit("nix/pins.nix has no bunVersion/bunRevision to keep until the build runs")

lines = [
    "  piAotRelease = {",
    '    owner = "zeroqn";',
    '    repo = "pi";',
    f'    tag = "{release_tag}";',
    f'    version = "{version}";',
    # Kept from the previous pin until the built canary answers below.
    f'    bunVersion = "{previous.group(1)}";',
    f'    bunRevision = "{previous.group(2)}";',
    "    systems = {",
]
for system, pinned in per_system.items():
    lines.extend([
        f"      {system} = {{",
        "        piAot = {",
        f'          asset = "{pinned["piAot"][0]}";',
        f'          hash = "{pinned["piAot"][1]}";',
        "        };",
        "        bunAot = {",
        f'          asset = "{pinned["bunAot"][0]}";',
        f'          hash = "{pinned["bunAot"][1]}";',
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
for system, pinned in per_system.items():
    print(f'  {system}.piAot = "{pinned["piAot"][0]}" {pinned["piAot"][1]}')
    print(f'  {system}.bunAot = "{pinned["bunAot"][0]}" {pinned["bunAot"][1]}')
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
