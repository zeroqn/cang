# The Nix configuration the cang image carries.
#
# `file` is written to `/etc/nix/nix.conf` (nix/image/container.nix). The guest
# runs a multi-user store: `cang-guest-init` starts `nix-daemon` as root, so this
# file is the system configuration the daemon reads, and it is where substituter
# settings have to live - the daemon ignores a client's own substituter settings
# unless the cache named there is already in its `trusted-substituters`.
#
# Both caches are the ones the flake's own `nixConfig` adds (flake.nix), with
# their own public signing keys: `fenix.cachix.org` is fenix's cache, and
# `nix-community.cachix.org` is where fenix's CI publishes the toolchain
# derivations cang builds with (measured: the toolchain's `rustfmt-preview` path
# is a 404 on `fenix.cachix.org` and a 200 on `nix-community.cachix.org`), so a
# guest `nix build` of a fenix-derived derivation downloads its inputs instead
# of building them. The default `cache.nixos.org` entry is restated because a
# `substituters` line replaces the default set rather than adding to it.
#
# `env` is the `NIX_CONFIG` variable of the image's `Env` (nix/image/config.nix).
# It stays the single `experimental-features` line: the guest builds through
# `nix-daemon`, so the substituters belong in the file the daemon reads.
{
  file = ''
    experimental-features = nix-command flakes
    substituters = https://cache.nixos.org https://fenix.cachix.org https://nix-community.cachix.org
    trusted-substituters = https://fenix.cachix.org https://nix-community.cachix.org
    trusted-public-keys = cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY= fenix.cachix.org-1:ecJhr+RdYEdcVgUkjruiYhjbBloIEGov7bos90cZi0Q= nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=
  '';
  env = "experimental-features = nix-command flakes";
}
