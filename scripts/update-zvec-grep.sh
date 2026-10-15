#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pins_file="$repo_root/nix/pins.nix"
owner="zvec-ai"
repo="zvec-grep"
rev=""
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
Usage: update-zvec-grep.sh --rev <git-revision-or-tag>

Refresh the pinned zvec-ai/zvec-grep Rust rewrite in nix/pins.nix. The image and
downstream consumers get `zg` from the `rust/` workspace of this repository (see
nix/pkgs/zvec-grep.nix); upstream's release tags still name the TypeScript build,
so the revision to pin has to be given explicitly.

The updater prefetches the GitHub archive, records its SRI hash, and refuses a
revision without `rust/Cargo.lock` or whose `zvec-rust` crate version does not
match the pinned `zvecRustLib` release asset in nix/pins.nix.

The Rust workspace never bumps `[workspace.package] version`, so `zvecGrep.version`
stays the `zg` stamp (0.3.0); this updater does not change it.

Options:
  --rev <rev>  git revision or tag to pin (required; --tag is an alias)
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --rev|--tag)
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

for cmd in nix nix-prefetch-url python3; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "missing required command: $cmd" >&2
    exit 1
  fi
done

if [ -z "$rev" ]; then
  echo "missing required --rev <git-revision-or-tag>" >&2
  usage >&2
  exit 1
fi

archive_url="https://github.com/$owner/$repo/archive/$rev.tar.gz"
mapfile -t prefetch_output < <(nix-prefetch-url --print-path --unpack "$archive_url")
if [ "${#prefetch_output[@]}" -lt 2 ] || [ -z "${prefetch_output[0]}" ] || [ -z "${prefetch_output[1]}" ]; then
  echo "failed to prefetch zvec-grep source archive: $archive_url" >&2
  exit 1
fi
src_hash_base32="${prefetch_output[0]}"
src_path="${prefetch_output[1]}"
src_hash_sri="$(nix hash convert --hash-algo sha256 --to sri "$src_hash_base32")"

workspace_manifest="$src_path/rust/Cargo.toml"
if [ ! -f "$workspace_manifest" ]; then
  echo "the pinned revision has no rust/Cargo.toml; $rev predates the Rust zg rewrite" >&2
  exit 1
fi
if [ ! -f "$src_path/rust/Cargo.lock" ]; then
  echo "the pinned revision has no rust/Cargo.lock; nix/pkgs/zvec-grep.nix vendors from it" >&2
  exit 1
fi

workspace_zvec_version="$(sed -n 's/^zvec-rust = "=\(.*\)"$/\1/p' "$workspace_manifest" | head -1)"
if [ -z "$workspace_zvec_version" ]; then
  echo "failed to read the zvec-rust crate version from $workspace_manifest" >&2
  exit 1
fi
pinned_zvec_version="$(cd "$repo_root" && nix eval --raw --impure --expr '(import ./nix/pins.nix).zvecRustLib.version')"
if [ "$workspace_zvec_version" != "$pinned_zvec_version" ]; then
  echo "the Rust workspace pins zvec-rust $workspace_zvec_version but nix/pins.nix zvecRustLib.version is $pinned_zvec_version" >&2
  echo "update the zvecRustLib release asset (version, asset, hash) in nix/pins.nix in the same change" >&2
  exit 1
fi

python3 - "$pins_file" "$rev" "$src_hash_sri" <<'PY'
import re
import sys
from pathlib import Path

pins_path = Path(sys.argv[1])
rev = sys.argv[2]
src_hash = sys.argv[3]

text = pins_path.read_text()
block_match = re.search(r'(  zvecGrep = \{\n)(?P<body>.*?)(\n  \};)', text, re.S)
if block_match is None:
    raise SystemExit("failed to locate zvecGrep block in nix/pins.nix")

body = block_match.group(2)
body, rev_count = re.subn(r'rev = "[^"]*";', f'rev = "{rev}";', body, count=1)
if rev_count != 1:
    raise SystemExit("failed to update zvecGrep.rev in nix/pins.nix")
body, hash_count = re.subn(r'srcHash = "[^"]*";', f'srcHash = "{src_hash}";', body, count=1)
if hash_count != 1:
    raise SystemExit("failed to update zvecGrep.srcHash in nix/pins.nix")

pins_path.write_text(text[: block_match.start()] + block_match.group(1) + body + block_match.group(3) + text[block_match.end() :])
PY

completed=1
cat <<EOF_SUMMARY
updated nix/pins.nix:
  zvecGrep.rev = "$rev";
  zvecGrep.srcHash = "$src_hash_sri";
  zvecGrep.version = "$(cd "$repo_root" && nix eval --raw --impure --expr '(import ./nix/pins.nix).zvecGrep.version')" (unchanged)
EOF_SUMMARY
