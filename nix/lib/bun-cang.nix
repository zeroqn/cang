# cang's bun: nixpkgs-unstable's package moved to the release whose embedded root
# store rejects the compromised eMudhra/emSign intermediates (CVE-2026-111943,
# oven-sh/bun#46159). bun embeds its own CA bundle rather than reading the
# system's, so `--use-system-ca` cannot repair an affected build and *every*
# binary compiled from it carries the bad roots: this is what the image ships as
# `bun` and what compiles the pi standalone binary, so both are rebuilt here.
#
# NixOS/nixpkgs#577354 carries the same bump. Once the input's own bun reaches
# `pins.bun.version` the guard below hands the input's package straight back, so
# the pin can stay in place (nothing to remove) and an input update is never
# pinned back down. The flake exports the result as `packages.<system>.bun` for a
# downstream host whose own bun users need the fixed root store before its
# nixpkgs moves; `checks.bun-cang-version` asserts the image really gets it.
{ pkgs, pins }:

let
  pin = pins.bun;
  system = pkgs.stdenv.hostPlatform.system;
  # Every pinned system, not just this one, because `passthru.sources` is what the
  # package's own `src` and its updater script read: an override that moved only
  # the built system would leave `sources` advertising the input's version.
  sources = builtins.mapAttrs (
    _: asset:
    pkgs.fetchurl {
      url = "https://github.com/oven-sh/bun/releases/download/bun-v${pin.version}/${asset.asset}";
      inherit (asset) hash;
    }
  ) pin.systems;
in
if
  pkgs.lib.versionAtLeast pkgs.bun.version pin.version || !(builtins.hasAttr system pin.systems)
then
  pkgs.bun
else
  pkgs.bun.overrideAttrs (old: {
    version = pin.version;
    src = sources.${system};
    passthru = old.passthru // {
      inherit sources;
    };
  })
