# cang task runner. Everything here runs inside the repo's devshell: recipes
# re-enter `nix develop` themselves, so `just release 0.15.2` works from any
# shell that has just.
set shell := ["bash", "-euo", "pipefail", "-c"]

# List the recipes.
default:
    @just --list

# Print the phases `just release <version>` runs. No side effects.
release-plan version:
    @scripts/cang-release.sh plan {{version}}

# Cut and publish a release: gates, version bump, CI-checked pin, tag, verify.
# See docs/maintenance.md "cang release scheme" for what each phase proves.
release version:
    @scripts/cang-release.sh all {{version}}

# The static gates a release has to pass (fmt, clippy, deny, test).
gate:
    @scripts/cang-release.sh gate

# Check a published release the way the driver does: asset == pin, prebuilt runs.
release-verify version:
    @scripts/cang-release.sh verify {{version}}
