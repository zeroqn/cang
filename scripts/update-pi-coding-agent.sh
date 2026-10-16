#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
package_json_file="$repo_root/nix/pkgs/pi-coding-agent-package.json"
package_lock_file="$repo_root/nix/pkgs/pi-coding-agent-package-lock.json"
owner="earendil-works"
repo="pi"
rev=""
fake_hash="sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
completed=0
original_pins="$(mktemp)"
original_package_json="$(mktemp)"
original_package_lock="$(mktemp)"
work_dir="$(mktemp -d)"
cp "$pins_file" "$original_pins"
if [ -f "$package_json_file" ]; then
  cp "$package_json_file" "$original_package_json"
else
  : >"$original_package_json"
fi
if [ -f "$package_lock_file" ]; then
  cp "$package_lock_file" "$original_package_lock"
else
  : >"$original_package_lock"
fi
cleanup() {
  if [ "$completed" -ne 1 ]; then
    cp "$original_pins" "$pins_file"
    if [ -s "$original_package_json" ]; then
      cp "$original_package_json" "$package_json_file"
    else
      rm -f "$package_json_file"
    fi
    if [ -s "$original_package_lock" ]; then
      cp "$original_package_lock" "$package_lock_file"
    else
      rm -f "$package_lock_file"
    fi
  fi
  rm -f "$original_pins" "$original_package_json" "$original_package_lock"
  rm -rf "$work_dir"
}
trap cleanup EXIT

usage() {
  cat <<'USAGE'
Usage: update-pi-coding-agent.sh [--tag <release-tag>]
       update-pi-coding-agent.sh [--rev <git-revision-or-release-tag>]

Refresh the pinned Pi coding agent source/npm metadata in nix/pins.nix from
https://github.com/earendil-works/pi/tree/main/packages/coding-agent. The script
prefetches the source archive, regenerates a Nix-friendly package lock for the
coding-agent workspace closure, and records the source, npm dependency, and
@earendil-works/pi-ai tarball fixed-output hashes.

Defaults:
  --tag     latest GitHub release tag

Compatibility:
  --rev     accepted as an alias for --tag/--rev
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tag|--rev)
      rev="${2:?missing value for $1}"
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

for cmd in curl jq nix nix-prefetch-url npm python3 tar; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

if [ -z "$rev" ]; then
  rev="$(
    curl -fsSL "https://api.github.com/repos/$owner/$repo/releases/latest" |
      jq -r '.tag_name // empty'
  )"
fi

if [ -z "$rev" ]; then
  echo "failed to determine latest Pi release; pass --tag explicitly" >&2
  exit 1
fi

archive_url="https://github.com/$owner/$repo/archive/$rev.tar.gz"
mapfile -t prefetch_output < <(nix-prefetch-url --print-path --unpack "$archive_url")
if [ "${#prefetch_output[@]}" -lt 2 ] || [ -z "${prefetch_output[0]}" ] || [ -z "${prefetch_output[1]}" ]; then
  echo "failed to prefetch Pi source archive: $archive_url" >&2
  exit 1
fi
src_hash_base32="${prefetch_output[0]}"
src_path="${prefetch_output[1]}"
src_hash_sri="$(nix hash convert --hash-algo sha256 --to sri "$src_hash_base32")"

source_copy="$work_dir/source"
cp -R "$src_path" "$source_copy"
chmod -R u+w "$source_copy"

python3 - "$source_copy/package.json" <<'PY'
import json
import sys
from pathlib import Path

package_json = Path(sys.argv[1])
data = json.loads(package_json.read_text())
# Keep the lockfile focused on the workspace closure required by
# packages/coding-agent's build:binary script. The upstream monorepo lockfile is
# currently missing many resolved URLs, and the full workspace set pulls in
# unrelated web/example dependencies that are not needed for this package.
data["workspaces"] = [
    "packages/agent",
    "packages/ai",
    "packages/tui",
    "packages/coding-agent",
]
package_json.write_text(json.dumps(data, indent="\t") + "\n")
PY

rm -f "$source_copy/package-lock.json"
(
  cd "$source_copy"
  # `--min-release-age=0` overrides the supply-chain cooldown the upstream
  # `.npmrc` sets (`min-release-age=2`, a two-day delay). The lock generated
  # here is for the release under test, and that release's own packages - the
  # `@earendil-works/*` members the restricted `workspaces` list above leaves
  # out of the workspace, so npm resolves them from the registry - were
  # published the same day; the cooldown would exclude exactly what this script
  # exists to pin. What actually lands is fixed by the lockfile and by the
  # `npmDepsHash` recorded in nix/pins.nix, not by the resolution window.
  npm_config_audit=false npm_config_fund=false npm install --package-lock-only --ignore-scripts --min-release-age=0
)

package_json="$source_copy/packages/coding-agent/package.json"
if [ ! -f "$package_json" ]; then
  echo "failed to locate packages/coding-agent/package.json in unpacked source" >&2
  exit 1
fi
version="$(jq -r '.version // empty' "$package_json")"
if [ -z "$version" ]; then
  echo "failed to read Pi coding agent version from $package_json" >&2
  exit 1
fi

# The package unpacks package/dist/providers/data from the published
# @earendil-works/pi-ai tarball (see nix/pkgs/pi-coding-agent.nix postPatch), so
# record its fixed-output hash here too and fail early if the layout changes.
ai_tarball_url="https://registry.npmjs.org/@earendil-works/pi-ai/-/pi-ai-$version.tgz"
mapfile -t ai_tarball_prefetch < <(nix-prefetch-url --print-path --type sha256 "$ai_tarball_url")
if [ "${#ai_tarball_prefetch[@]}" -lt 2 ] || [ -z "${ai_tarball_prefetch[0]}" ] || [ -z "${ai_tarball_prefetch[1]}" ]; then
  echo "failed to prefetch @earendil-works/pi-ai tarball: $ai_tarball_url" >&2
  exit 1
fi
ai_tarball_hash_base32="${ai_tarball_prefetch[0]}"
ai_tarball_store_path="${ai_tarball_prefetch[1]}"
ai_tarball_hash_sri="$(nix hash convert --hash-algo sha256 --to sri "$ai_tarball_hash_base32")"
if ! tar -tzf "$ai_tarball_store_path" | grep '^package/dist/providers/data/' >/dev/null; then
  echo "pi-ai $version tarball no longer contains package/dist/providers/data; update the postPatch unpack in nix/pkgs/pi-coding-agent.nix" >&2
  exit 1
fi

cp "$source_copy/package.json" "$package_json_file"
cp "$source_copy/package-lock.json" "$package_lock_file"

python3 - "$pins_file" "$version" "$owner" "$repo" "$rev" "$src_hash_sri" "$fake_hash" "$ai_tarball_hash_sri" <<'PY'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
version = sys.argv[2]
owner = sys.argv[3]
repo = sys.argv[4]
rev = sys.argv[5]
src_hash = sys.argv[6]
npm_deps_hash = sys.argv[7]
ai_tarball_hash = sys.argv[8]

replacement = "\n".join(
    [
        "  piCodingAgent = {",
        f'    version = "{version}";',
        f'    owner = "{owner}";',
        f'    repo = "{repo}";',
        f'    rev = "{rev}";',
        f'    srcHash = "{src_hash}";',
        f'    npmDepsHash = "{npm_deps_hash}";',
        f'    aiNpmTarballHash = "{ai_tarball_hash}";',
        "  };",
    ]
)

text = pins_path.read_text()
updated, count = re.subn(
    r"  piCodingAgent = \{\n.*?\n  \};",
    replacement,
    text,
    count=1,
    flags=re.S,
)
if count != 1:
    raise SystemExit("failed to locate piCodingAgent block in nix/pins.nix")

pins_path.write_text(updated)
PY

prefetch_npm_deps="$(nix build --no-link --print-out-paths nixpkgs#prefetch-npm-deps)/bin/prefetch-npm-deps"
npm_cache="$work_dir/npm-cache"
for attempt in 1 2 3; do
  rm -rf "$npm_cache"
  if NPM_FETCHER_VERSION=2 "$prefetch_npm_deps" "$package_lock_file" "$npm_cache"; then
    break
  fi
  if [ "$attempt" -eq 3 ]; then
    echo "failed to prefetch Pi npm dependencies after $attempt attempts" >&2
    exit 1
  fi
  echo "npm dependency prefetch failed; retrying ($attempt/3)" >&2
  sleep 2
done

npm_deps_hash="$(nix hash path "$npm_cache")"
nix store add-path --name "pi-coding-agent-$version-npm-deps" "$npm_cache" >/dev/null

python3 - "$pins_file" "$npm_deps_hash" <<'PY'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
npm_deps_hash = sys.argv[2]
text = pins_path.read_text()
block_match = re.search(r'piCodingAgent = \{\n(?P<body>.*?)\n  \};', text, re.S)
if block_match is None:
    raise SystemExit("failed to locate piCodingAgent block in nix/pins.nix")
body = block_match.group("body")
body, count = re.subn(
    r'npmDepsHash = "sha256-[^"]+";',
    f'npmDepsHash = "{npm_deps_hash}";',
    body,
    count=1,
)
if count != 1:
    raise SystemExit("failed to update Pi coding agent npmDepsHash in nix/pins.nix")
pins_path.write_text(text[: block_match.start("body")] + body + text[block_match.end("body") :])
PY

completed=1
cat <<EOF_SUMMARY
updated nix/pins.nix:
  piCodingAgent.version = "$version";
  piCodingAgent.owner = "$owner";
  piCodingAgent.repo = "$repo";
  piCodingAgent.rev = "$rev";
  piCodingAgent.srcHash = "$src_hash_sri";
  piCodingAgent.npmDepsHash = "$npm_deps_hash";
  piCodingAgent.aiNpmTarballHash = "$ai_tarball_hash_sri";
updated generated lock inputs:
  $package_json_file
  $package_lock_file
EOF_SUMMARY
