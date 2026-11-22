#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zeroqn"
repo="cang"
system="x86_64-linux"
release_tag=""
provided_hash=""
provided_pump_hash=""
asset_path=""
pump_asset_path=""

usage() {
  cat <<'USAGE'
Usage: update-cang-prebuilt.sh [--tag <release-tag>] [--system <system>]
                                [--hash <sri>] [--pump-hash <sri>]
                                [--asset-path <file>] [--pump-asset-path <file>]
                                [--pins-file <file>]

Refresh the pinned cang prebuilt release metadata in nix/pins.nix by querying
GitHub Releases, rejecting legacy/concrete-store-referencing payloads, and
recomputing the binary SRI hashes.

A cang release publishes two neutral ELF assets - the host `cang` binary and the
workspace reverse share's host half `relay-pump` - and .#cang-prebuilt fetches
both. This updater pins them together, from one release: it validates both
payloads and only then writes both, so a pin can never name a cang ELF whose
matching pump is missing.

  --hash       pin this cang SRI instead of downloading the asset. Only for the
               pre-tag flow in scripts/cang-release.sh, which takes the hash from
               the cang CI run that built the same source: the versioned asset
               does not exist yet, so there is nothing to download and nothing to
               check.
  --pump-hash  the same for the relay-pump asset; --hash and --pump-hash always
               go together.
  --asset-path / --pump-asset-path
               hash (and validate) a local file instead of downloading the
               release asset.
  --pins-file  the nix/pins.nix to rewrite. Defaults to this repository's; the
               repository tests point it at a copy so the updater can be
               exercised offline.

Defaults:
  --tag     newest rolling sha-<revision> prerelease containing the selected
            cang asset. Pass a permanent v<version> tag (for example
            --tag v0.7.1) to pin an artifact from a versioned release: those
            releases are never pruned, while the release workflow deletes all
            but the 20 newest sha-<revision> prereleases.
  --system  x86_64-linux
USAGE
}

die() {
  printf 'update-cang-prebuilt: %s\n' "$*" >&2
  exit 1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --tag)
      release_tag="${2:?missing value for --tag}"
      shift 2
      ;;
    --hash)
      provided_hash="${2:?missing value for --hash}"
      shift 2
      ;;
    --pump-hash)
      provided_pump_hash="${2:?missing value for --pump-hash}"
      shift 2
      ;;
    --asset-path)
      asset_path="${2:?missing value for --asset-path}"
      shift 2
      ;;
    --pump-asset-path)
      pump_asset_path="${2:?missing value for --pump-asset-path}"
      shift 2
      ;;
    --pins-file)
      pins_file="${2:?missing value for --pins-file}"
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

if [ -n "$provided_hash" ] && [ -z "$provided_pump_hash" ]; then
  die "--hash also needs --pump-hash: one pin carries the cang ELF and its relay pump"
fi
if [ -n "$provided_pump_hash" ] && [ -z "$provided_hash" ]; then
  die "--pump-hash also needs --hash: one pin carries the cang ELF and its relay pump"
fi
if [ -n "$provided_hash" ] && [ -z "$release_tag" ]; then
  die "--hash also needs --tag: the asset names come from the release tag"
fi

# Python always edits the pin; curl and jq are only needed for the paths that
# talk to GitHub Releases.
command -v python3 >/dev/null 2>&1 || die "missing required command: python3"

case "$system" in
  x86_64-linux)
    asset_arch="x86_64"
    ;;
  aarch64-linux)
    asset_arch="aarch64"
    ;;
  *)
    die "unsupported system: $system"
    ;;
esac

releases_api="https://api.github.com/repos/$owner/$repo/releases?per_page=100"

download_asset() {
  local name="$1" path="$2"
  command -v curl >/dev/null 2>&1 || die "missing required command: curl"
  local url="https://github.com/$owner/$repo/releases/download/$release_tag/$name"
  local attempt payload code
  # A release asset can take a moment to become readable after the run that
  # uploaded it finished, so retry rather than failing the pin on a race.
  for attempt in 1 2 3 4 5; do
    payload="$(curl -sSL -w $'\n%{http_code}' --output "$path" "$url" 2>/dev/null || true)"
    code="${payload##*$'\n'}"
    if [ "$code" = 200 ]; then
      return 0
    fi
    echo "download of $url returned HTTP $code, retrying ($attempt/5)" >&2
    sleep 10
  done
  die "could not download $url (last HTTP $code)"
}

# Both payloads are neutral raw ELFs: a wrapper script or a concrete store
# reference means the release published the wrong thing and the pin must refuse
# it (docs/adr/0005-neutral-cang-prebuilt-assets.md).
validate_asset() {
  local path="$1" name="$2" label="$3"
  python3 - "$path" "$name" "$release_tag" "$label" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
asset_name, release_tag, label = sys.argv[2], sys.argv[3], sys.argv[4]
data = path.read_bytes()
if data.startswith(b"#!"):
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} is a wrapper script, not raw ELF"
    )
if data[:4] != b"\x7fELF":
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} is not an ELF {label} payload"
    )
if re.search(rb"/nix/store/[0-9a-df-np-sv-z]{32}-", data):
    raise SystemExit(
        f"upstream asset blocker: {asset_name} in {release_tag} contains concrete /nix/store references; publish a neutral {label} asset"
    )
PY
}

sri_of() {
  python3 - "$1" <<'PY'
import base64
import hashlib
import pathlib
import sys

digest = hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).digest()
print("sha256-" + base64.b64encode(digest).decode())
PY
}

# A permanent v<version> release publishes its neutral assets under the versioned
# names, while a rolling sha-<revision> prerelease made from a branch push carries
# the unversioned ones (publish_release.yml in this repository).
if [ -z "$release_tag" ]; then
  command -v jq >/dev/null 2>&1 || die "missing required command: jq"
  command -v curl >/dev/null 2>&1 || die "missing required command: curl"
  search_asset_name="cang-$asset_arch-unknown-linux-gnu"
  release_tag="$(
    curl -fsSL "$releases_api" |
      jq -r --arg asset_name "$search_asset_name" '
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
  die "failed to determine a sha-* release tag containing a cang asset; pass --tag explicitly after publishing one"
fi

case "$release_tag" in
  sha-*)
    cang_asset_name="cang-$asset_arch-unknown-linux-gnu"
    pump_asset_name="cang-relay-pump-$asset_arch-unknown-linux-gnu"
    ;;
  v[0-9]*)
    cang_asset_name="cang-$release_tag-$asset_arch-unknown-linux-gnu"
    pump_asset_name="cang-relay-pump-$release_tag-$asset_arch-unknown-linux-gnu"
    ;;
  *)
    die "unsupported release tag: $release_tag (expected a rolling sha-<revision> or a permanent v<version> release)"
    ;;
esac

if [[ "$cang_asset_name$pump_asset_name" == *-linux-flake-locked ]]; then
  die "internal error: refusing legacy cang flake-locked asset name: $cang_asset_name $pump_asset_name"
fi

cang_sri=""
pump_sri=""

if [ -n "$provided_hash" ]; then
  cang_sri="$provided_hash"
  pump_sri="$provided_pump_hash"
else
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT

  if [ -z "$asset_path" ]; then
    asset_path="$tmp_dir/$cang_asset_name"
    download_asset "$cang_asset_name" "$asset_path"
  fi
  [ -f "$asset_path" ] || die "asset path does not exist: $asset_path"
  validate_asset "$asset_path" "$cang_asset_name" cang
  cang_sri="$(sri_of "$asset_path")"

  # The pump is validated before anything is written, so a release that publishes
  # a broken or missing pump leaves nix/pins.nix untouched.
  if [ -z "$pump_asset_path" ]; then
    pump_asset_path="$tmp_dir/$pump_asset_name"
    download_asset "$pump_asset_name" "$pump_asset_path"
  fi
  [ -f "$pump_asset_path" ] || die "relay pump asset path does not exist: $pump_asset_path"
  validate_asset "$pump_asset_path" "$pump_asset_name" "relay pump"
  pump_sri="$(sri_of "$pump_asset_path")"
fi

python3 - "$pins_file" "$release_tag" "$system" "$cang_asset_name" "$cang_sri" "$pump_asset_name" "$pump_sri" <<'PY'
import re
import sys
from pathlib import Path

(
    pins_path,
    release_tag,
    system,
    cang_asset_name,
    cang_sri,
    pump_asset_name,
    pump_sri,
) = sys.argv[1:8]

path = Path(pins_path)
text = path.read_text()

block_match = re.search(
    r'(  cangPrebuiltRelease = \{\n)(?P<body>.*?)(\n  \};)',
    text,
    re.S,
)
if block_match is None:
    raise SystemExit("failed to locate cangPrebuiltRelease block in nix/pins.nix")

prefix = block_match.group(1)
body = block_match.group("body")
suffix = block_match.group(3)

body, tag_count = re.subn(r'\n    tag = "[^"]+";', f'\n    tag = "{release_tag}";', body, count=1)
if tag_count != 1:
    raise SystemExit("failed to update the cang prebuilt release tag in nix/pins.nix")

system_entry = (
    f"      {system} = {{\n"
    f'        asset = "{cang_asset_name}";\n'
    f'        hash = "{cang_sri}";\n'
    f'        relayPumpAsset = "{pump_asset_name}";\n'
    f'        relayPumpHash = "{pump_sri}";\n'
    f"      }};"
)

# Match the whole per-system entry so comments inside it (which the field
# regexes below must not trip over) are preserved.
system_pattern = re.compile(
    rf"(      {re.escape(system)} = \{{\n)(?P<entry>.*?)(\n      \}};)",
    re.S,
)
system_match = system_pattern.search(body)

if system_match is not None:
    entry = system_match.group("entry")
    entry, asset_count = re.subn(
        r'(?m)^        asset = "[^"]+";',
        f'        asset = "{cang_asset_name}";',
        entry,
        count=1,
    )
    if asset_count != 1:
        raise SystemExit(f"failed to update the {system} cang asset in nix/pins.nix")
    if 'relayPumpAsset = "' in entry:
        entry, pump_asset_count = re.subn(
            r'(?m)^        relayPumpAsset = "[^"]+";',
            f'        relayPumpAsset = "{pump_asset_name}";',
            entry,
            count=1,
        )
        entry, pump_hash_count = re.subn(
            r'(?m)^        relayPumpHash = "[^"]+";',
            f'        relayPumpHash = "{pump_sri}";',
            entry,
            count=1,
        )
        if (pump_asset_count, pump_hash_count) != (1, 1):
            raise SystemExit(f"failed to update the {system} relay pump fields in nix/pins.nix")
        entry, hash_count = re.subn(
            r'(?m)^        hash = "[^"]+";',
            f'        hash = "{cang_sri}";',
            entry,
            count=1,
        )
        if hash_count != 1:
            raise SystemExit(f"failed to update the {system} cang hash in nix/pins.nix")
    else:
        entry, hash_count = re.subn(
            r'(?m)^(        hash = "[^"]+";)',
            lambda match: (
                f'        hash = "{cang_sri}";'
                f'\n        relayPumpAsset = "{pump_asset_name}";'
                f'\n        relayPumpHash = "{pump_sri}";'
            ),
            entry,
            count=1,
        )
        if hash_count != 1:
            raise SystemExit(f"failed to add the {system} relay pump fields in nix/pins.nix")
    body = body[: system_match.start("entry")] + entry + body[system_match.end("entry") :]
else:
    empty_systems = "systems = { };"
    if empty_systems in body:
        body = body.replace(empty_systems, f"systems = {{\n{system_entry}\n    }};", 1)
    else:
        systems_match = re.search(r"(systems = \{\n)(?P<systems>.*?)(    \};)", body, re.S)
        if systems_match is None:
            raise SystemExit("failed to locate cangPrebuiltRelease.systems block in nix/pins.nix")
        insertion = systems_match.group("systems")
        if insertion and not insertion.endswith("\n"):
            insertion += "\n"
        insertion += system_entry + "\n"
        body = body[: systems_match.start("systems")] + insertion + body[systems_match.end("systems") :]

updated = text[: block_match.start("body")] + body + text[block_match.end("body") :]
path.write_text(updated)
PY

cat <<EOF_OUT
updated nix/pins.nix:
  tag = "$release_tag";
  $system.asset = "$cang_asset_name";
  $system.hash = "$cang_sri";
  $system.relayPumpAsset = "$pump_asset_name";
  $system.relayPumpHash = "$pump_sri";
EOF_OUT
