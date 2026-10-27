#!/bin/bash
# cang release driver: the phases of docs/maintenance.md's "cang release scheme"
# as one command, so a release cannot skip a step or guess a value.
#
# The asset SRI is never computed locally. Which patchelf normalizes the release
# asset depends on the host's flake registry (patchelf 0.18 moved where
# --set-interpreter places .interp), so a locally normalized asset is a different
# byte stream from CI's and a pin cut from it silently misses - v0.15.1 shipped
# that way. The driver takes the SRI from the cang CI run that built the same
# source instead: a branch push publishes the alpha asset (byte-identical to the
# versioned one, only the upload name differs), and its `.sha256` is the hash the
# tag push will publish.
#
#   scripts/cang-release.sh plan   <version>
#   scripts/cang-release.sh all    <version>   gates, bump, pin, tag, verify
#   scripts/cang-release.sh gate               the static gates a release passes
#   scripts/cang-release.sh bump   <version>   version + cargoDeps hash + commit
#   scripts/cang-release.sh pin    <version>   SRI from CI's alpha asset + commit
#   scripts/cang-release.sh push               push main, wait for its CI run
#   scripts/cang-release.sh tag    <version>   tag HEAD, push it, wait for its run
#   scripts/cang-release.sh verify <version>   published asset == pin, prebuilt runs
#   scripts/cang-release.sh status <sha> [branch]
#                                              report one CI run (branch: main)
#   scripts/cang-release.sh wait   <sha> [branch]
#                                              block until that CI run succeeds
#
# Environment:
#   CANG_RELEASE_SKIP_GATES=1   skip fmt/clippy/deny/test (already run)
#   CANG_RELEASE_SMOKE_OK=1     acknowledge a live smoke after a deps/libkrun move
#   CANG_RELEASE_ASSUME_YES=1   do not ask before pushing
#   CANG_RELEASE_CI_TIMEOUT     seconds to wait for one CI run (default 2400)
#   CANG_RELEASE_CI_POLL        seconds between polls (default 20)
#   CANG_RELEASE_SLUG           owner/repo override when origin is not GitHub
#   GITHUB_TOKEN / GH_TOKEN     raise the unauthenticated API rate limit
set -euo pipefail

self="${BASH_SOURCE[0]}"
repo_root="$(cd "$(dirname "$self")/.." && pwd)"
cd "$repo_root"

die() {
  printf 'cang-release: %s\n' "$*" >&2
  exit 1
}

say() {
  printf 'cang-release: %s\n' "$*"
}

usage() {
  awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "$self"
}

# Recipes run outside `nix develop` (and `just` itself is only on that PATH), so
# re-enter the devshell rather than failing on a missing cargo.
if ! command -v cargo >/dev/null 2>&1 && [ -z "${CANG_RELEASE_NO_REEXEC-}" ]; then
  export CANG_DISABLE_AUTO_FISH=1 CANG_RELEASE_NO_REEXEC=1
  exec nix develop --command "$self" "$@"
fi

for cmd in cargo curl git jq nix python3; do
  command -v "$cmd" >/dev/null 2>&1 || die "missing required command: $cmd"
done

workflow="Publish release binaries"

origin_url="$(git remote get-url origin 2>/dev/null || true)"
slug="${CANG_RELEASE_SLUG:-}"
if [ -z "$slug" ]; then
  case "$origin_url" in
    *github.com*) slug="$(printf '%s' "$origin_url" | sed -E 's#^.*[:/]([^/:]+/[^/:]+?)(\.git)?$#\1#')" ;;
  esac
fi
if ! printf '%s' "$slug" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'; then
  die "cannot derive owner/repo from origin url '$origin_url'; set CANG_RELEASE_SLUG=owner/repo"
fi

asset_arch() {
  case "$(uname -m)" in
    x86_64) printf 'x86_64' ;;
    aarch64 | arm64) printf 'aarch64' ;;
    *) die "unsupported architecture: $(uname -m)" ;;
  esac
}

workspace_version() {
  python3 - "$repo_root/Cargo.toml" <<'PY'
import re
import sys

text = open(sys.argv[1]).read()
match = re.search(r'\[workspace\.package\]\n(?:.*\n)*?version = "([^"]+)"', text)
if match is None:
    raise SystemExit("failed to read [workspace.package] version from Cargo.toml")
print(match.group(1))
PY
}

pinned_tag() {
  nix eval --raw --impure --expr "(import $repo_root/nix/pins.nix).cangPrebuiltRelease.tag"
}

# `systems."x86_64-linux".hash` needs the quotes: an unquoted dashed attribute
# name parses as subtraction.
pinned_sri() {
  nix eval --raw --impure --expr "(import $repo_root/nix/pins.nix).cangPrebuiltRelease.systems.\"$(asset_system)\".hash"
}

asset_system() {
  case "$(asset_arch)" in
    x86_64) printf 'x86_64-linux' ;;
    aarch64) printf 'aarch64-linux' ;;
  esac
}

gh_api() {
  local token="${GITHUB_TOKEN:-${GH_TOKEN-}}"
  if [ -n "$token" ]; then
    curl -fsSL -H "Authorization: Bearer $token" -H 'Accept: application/vnd.github+json' \
      "https://api.github.com$1"
  else
    curl -fsSL -H 'Accept: application/vnd.github+json' "https://api.github.com$1"
  fi
}

# The alpha release is rewritten by every main push, so only read it once the
# run for the revision under test has finished - and read it a few times before
# giving up, because the read can race the release update.
alpha_asset_sri() {
  local url payload code hex attempt
  url="https://github.com/$slug/releases/download/alpha/cang-$(asset_arch)-unknown-linux-gnu.sha256"
  for attempt in 1 2 3 4 5; do
    payload="$(curl -sSL -w $'\n%{http_code}' "$url" 2>/dev/null || true)"
    code="${payload##*$'\n'}"
    hex="$(printf '%s\n' "$payload" | awk 'NR == 1 {print $1}')"
    if [ "$code" = 200 ] && [ "${#hex}" -eq 64 ]; then
      case "$hex" in
        *[!0-9a-f]*) ;;
        *) nix hash convert --hash-algo sha256 --to sri "$hex" && return 0 ;;
      esac
    fi
    say "alpha asset fetch returned HTTP $code, retrying ($attempt/5): $url"
    sleep 10
  done
  die "could not read $url (last HTTP $code)"
}

run_for() {
  local sha
  sha="$(git rev-parse --verify "$1^{commit}")"
  gh_api "/repos/$slug/actions/runs?head_sha=$sha&per_page=50" |
    jq -r --arg branch "$2" --arg workflow "$workflow" '
      [.workflow_runs[]
        | select(.name == $workflow)
        | select(.head_branch == $branch)]
      | sort_by(.created_at)
      | last
      | .id // empty'
}

wait_run() {
  local sha="$1" branch="$2" label="$3"
  local deadline=$((SECONDS + ${CANG_RELEASE_CI_TIMEOUT:-2400}))
  local poll="${CANG_RELEASE_CI_POLL:-20}"
  local id status conclusion
  while :; do
    id="$(run_for "$sha" "$branch")"
    if [ -n "$id" ]; then
      read -r status conclusion < <(gh_api "/repos/$slug/actions/runs/$id" |
        jq -r '"\(.status) \(.conclusion // "-")"')
      if [ "$status" = completed ]; then
        [ "$conclusion" = success ] ||
          die "CI $label run $id concluded $conclusion: https://github.com/$slug/actions/runs/$id"
        say "CI $label run $id succeeded"
        return 0
      fi
      say "waiting for CI $label run $id ($status)"
    else
      say "waiting for the CI $label run of $(printf '%.12s' "$sha") to appear"
    fi
    [ "$SECONDS" -lt "$deadline" ] ||
      die "timed out after ${CANG_RELEASE_CI_TIMEOUT:-2400}s waiting for the CI $label run of $sha"
    sleep "$poll"
  done
}

# The vendored lock records the workspace-member versions, so a version bump
# invalidates cargoDeps.hash; take the value out of the build's own mismatch.
set_cargo_deps_hash() {
  python3 - "$repo_root/nix/pkgs/cang-rust.nix" "$1" <<'PY'
import re
import sys

path, value = sys.argv[1], sys.argv[2]
text = open(path).read()
replacement = "hash = pkgs.lib.fakeHash;" if value == "fake" else f'hash = "{value}";'
marker = "cargoDeps = pkgs.rustPlatform.fetchCargoVendor {"
start = text.index(marker)
end = text.index("};", start)
block, count = re.subn(r"hash = (?:pkgs\.lib\.fakeHash|\"[^\"]*\");", replacement, text[start:end], count=1)
if count != 1:
    raise SystemExit("failed to locate the cang cargoDeps hash in nix/pkgs/cang-rust.nix")
open(path, "w").write(text[:start] + block + text[end:])
PY
}

set_workspace_version() {
  python3 - "$repo_root/Cargo.toml" "$1" <<'PY'
import re
import sys

path, version = sys.argv[1], sys.argv[2]
text = open(path).read()
match = re.search(r'(\[workspace\.package\]\n(?:.*\n)*?version = ")[^"]+(")', text)
if match is None:
    raise SystemExit("failed to locate [workspace.package] version in Cargo.toml")
open(path, "w").write(text[: match.start()] + match.group(1) + version + match.group(2) + text[match.end() :])
PY
}

require_clean_tree() {
  git diff --quiet && git diff --cached --quiet || die "worktree is dirty; commit or stash first"
}

require_version_arg() {
  case "${1-}" in
    '') die "missing release version (for example: 0.15.2)" ;;
    *[!0-9.]* | .* | *. | *..*) die "unsupported release version: $1" ;;
  esac
  [ "$(printf '%s' "$1" | awk -F. '{print NF}')" -eq 3 ] ||
    die "release versions are three-part (Cargo rejects four): $1"
}

confirm() {
  [ "${CANG_RELEASE_ASSUME_YES:-0}" = 1 ] && return 0
  [ -t 0 ] || return 0
  local answer
  read -r -p "$1 [y/N] " answer
  case "$answer" in
    y | Y | yes | YES) return 0 ;;
    *) die "aborted" ;;
  esac
}

gate() {
  if [ "${CANG_RELEASE_SKIP_GATES:-0}" = 1 ]; then
    say "skipping gates (CANG_RELEASE_SKIP_GATES=1)"
    return 0
  fi
  say "gate: cargo fmt --check"
  cargo fmt --check
  say "gate: cargo clippy --all-targets --all-features -- -D warnings"
  cargo clippy --all-targets --all-features -- -D warnings
  say "gate: cargo deny check"
  cargo deny check
  say "gate: cargo test"
  cargo test
}

# A libkrun pin move is release-blocking until both live smokes pass; that is a
# 25-40 minute GPU job, so the driver asks for the acknowledgement rather than
# pretending to run it.
require_smoke_ack() {
  local previous
  previous="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)"
  [ -n "$previous" ] || return 0
  if git diff --quiet "$previous" HEAD -- deps/libkrun; then
    return 0
  fi
  [ "${CANG_RELEASE_SMOKE_OK:-0}" = 1 ] ||
    die "deps/libkrun moved since $previous: run tools/chromium-cang-smoke and tools/virglrenderer-cang-smoke, then re-run with CANG_RELEASE_SMOKE_OK=1"
  say "deps/libkrun moved since $previous, live smoke acknowledged"
}

# The tag gate in publish_release.yml refuses a tagged release while the
# libkrunfw pin is a rolling one.
require_permanent_libkrunfw_pin() {
  local tag
  tag="$(nix eval --raw --impure --expr "(import $repo_root/nix/pins.nix).libkrunfwRelease.tag")"
  case "$tag" in
    v[0-9]*-cang.*) ;;
    *) die "libkrunfwRelease.tag is rolling ($tag); publish a permanent v<version>-cang.<n> fork release and re-pin first" ;;
  esac
}

cmd_plan() {
  require_version_arg "${1-}"
  local version="$1" current tag_sha_lookalike
  current="$(workspace_version)"
  say "cang release plan for v$version (current workspace version: $current)"
  cat <<PLAN
  phase 1  gates      cargo fmt --check, clippy -D warnings, cargo deny check, cargo test
  phase 2  bump       Cargo.toml + Cargo.lock workspace members -> $version,
                      cargoDeps.hash from the fakeHash build, commit
  phase 3  push       git push origin main, wait for the "$workflow" run
  phase 4  pin        SRI = CI's alpha asset .sha256 for phase 3's revision,
                      scripts/update-cang-prebuilt.sh --tag v$version --hash <sri>, commit
  phase 5  push       git push origin main, wait for the pin commit's run and
                      re-check its alpha .sha256 against the pin
  phase 6  tag        git tag v$version on the pin commit, git push origin v$version,
                      wait for the tag run
  phase 7  verify     published cang-v$version-$(asset_arch)-unknown-linux-gnu.sha256 == pin,
                      scripts/update-cang-prebuilt.sh --tag v$version is a no-op,
                      nix build .#cang-prebuilt and \`cang --version\` report $version
PLAN
  if git rev-parse -q --verify "refs/tags/v$version" >/dev/null; then
    say "note: tag v$version already exists locally"
  fi
}

cmd_bump() {
  local version="$1" current got
  require_version_arg "$version"
  require_clean_tree
  current="$(workspace_version)"
  [ "$current" != "$version" ] || die "workspace version is already $version"

  say "bump: workspace version $current -> $version"
  set_workspace_version "$version"
  cargo update -w >/dev/null

  say "bump: cargoDeps.hash from the fakeHash build"
  set_cargo_deps_hash fake
  got="$(nix build .#cang --no-link 2>&1 |
    grep -o 'got: *sha256-[A-Za-z0-9+/=]*' | head -1 | sed 's/got: *//')" || true
  [ -n "$got" ] ||
    die "the fakeHash build reported no hash mismatch; the version bump may not have invalidated cargoDeps (see nix/pkgs/cang-rust.nix)"
  set_cargo_deps_hash "$got"
  say "bump: cargoDeps.hash = $got"

  say "bump: rebuilding .#cang with the new hash"
  nix build .#cang --no-link >/dev/null

  git add Cargo.toml Cargo.lock nix/pkgs/cang-rust.nix
  git commit -q -m "cang: bump the version to $version"
  say "bump: committed $(git rev-parse --short HEAD)"
}

cmd_push() {
  timeout 300 git push origin main </dev/null >&2
  wait_run "$(git rev-parse HEAD)" main "branch ($(git rev-parse --short HEAD))"
}

cmd_pin() {
  local version="$1" sri
  require_version_arg "$version"
  require_clean_tree
  sri="$(alpha_asset_sri)"
  say "pin: CI alpha asset SRI = $sri"
  ./scripts/update-cang-prebuilt.sh --tag "v$version" --hash "$sri" >/dev/null
  [ "$(pinned_tag)" = "v$version" ] || die "pins.nix was not updated to v$version"
  [ "$(pinned_sri)" = "$sri" ] || die "pins.nix does not carry $sri"
  git add nix/pins.nix
  git commit -q -m "pins: pin the cang v$version prebuilt at the CI-published bytes"
  say "pin: committed $(git rev-parse --short HEAD)"
}

cmd_tag() {
  local version="$1"
  require_version_arg "$version"
  require_clean_tree
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "tag v$version already exists"
  [ "$(pinned_tag)" = "v$version" ] ||
    die "nix/pins.nix does not pin v$version yet; run the pin phase first"
  git tag "v$version"
  timeout 300 git push origin "v$version" </dev/null >&2
  wait_run "$(git rev-parse HEAD)" "v$version" "tag"
}

cmd_status() {
  local sha="${1-}" branch="${2:-main}" id
  [ -n "$sha" ] || die "missing commit sha"
  id="$(run_for "$sha" "$branch")"
  [ -n "$id" ] || die "no \"$workflow\" run for $sha on $branch"
  gh_api "/repos/$slug/actions/runs/$id" |
    jq -r '"run \(.id) \(.status) \(.conclusion // "-") \(.html_url)"'
}

cmd_wait() {
  local sha="${1-}" branch="${2:-main}"
  [ -n "$sha" ] || die "missing commit sha"
  wait_run "$(git rev-parse --verify "$sha^{commit}")" "$branch" "$branch"
}

cmd_verify() {
  local version="$1" pin_sri pin_hex published_hex target output
  require_version_arg "$version"
  pin_sri="$(pinned_sri)"
  pin_hex="$(nix hash convert --to base16 "$pin_sri")"
  published_hex="$(curl -fsSL "https://github.com/$slug/releases/download/v$version/cang-v$version-$(asset_arch)-unknown-linux-gnu.sha256" | awk '{print $1}')"
  [ "$pin_hex" = "$published_hex" ] ||
    die "pin $pin_hex != published $published_hex"

  say "verify: re-deriving the pin from the published asset"
  ./scripts/update-cang-prebuilt.sh --tag "v$version" >/dev/null
  git diff --exit-code -- nix/pins.nix >/dev/null ||
    die "nix/pins.nix does not match the published v$version asset; the pin is stale or the asset was rebuilt"

  say "verify: nix build .#cang-prebuilt"
  target="$(nix build .#cang-prebuilt -o "$repo_root/result-cang-prebuilt-release" --print-out-paths)"
  output="$("$target/bin/cang" --version)"
  [ "$output" = "cang $version" ] || die "the prebuilt reports '$output'"
  say "verify: v$version is published and pinned (asset $published_hex)"
}

cmd_all() {
  local version="$1" hash_before hash_after attempt
  require_version_arg "$version"
  cmd_plan "$version"
  require_clean_tree
  require_smoke_ack
  require_permanent_libkrunfw_pin
  [ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "releases are cut from main"
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "tag v$version already exists"

  gate
  confirm "cut and publish v$version?"

  cmd_bump "$version"
  cmd_push
  hash_before="$(alpha_asset_sri)"
  say "CI built $hash_before for $(git rev-parse --short HEAD)"

  cmd_pin "$version"
  for attempt in 1 2 3; do
    cmd_push
    hash_after="$(alpha_asset_sri)"
    [ "$hash_after" = "$(pinned_sri)" ] && break
    say "CI built $hash_after for the pin commit; re-pinning to that"
    cmd_pin "$version"
  done
  [ "$attempt" -lt 4 ] || die "the asset hash keeps moving across pin commits; the build is not reproducible"

  cmd_tag "$version"
  cmd_verify "$version"
}

command="${1-}"
[ "$#" -gt 0 ] && shift

case "$command" in
  plan) cmd_plan "${1-}" ;;
  gate) gate ;;
  bump) cmd_bump "${1-}" ;;
  pin) cmd_pin "${1-}" ;;
  push) require_clean_tree && cmd_push ;;
  tag) cmd_tag "${1-}" ;;
  verify) cmd_verify "${1-}" ;;
  status) cmd_status "${1-}" "${2-}" ;;
  wait) cmd_wait "${1-}" "${2-}" ;;
  all) cmd_all "${1-}" ;;
  -h | --help | help | '') usage ;;
  *) usage >&2; die "unknown command: $command" ;;
esac
