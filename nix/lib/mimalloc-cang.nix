# cang's mimalloc: zeroqn/headless's prebuilt release asset when one is pinned
# for this system, otherwise nixpkgs' own package upgraded to at least 3.5.3.
#
# Both consumers evaluate this against their own package set, and both preload
# the same object:
#  * the guest image, whose `/etc/ld-nix.so.preload` makes mimalloc the default
#    allocator for every Nix-linked guest process (so a run pays one allocator's
#    behaviour, not glibc's, wherever its binaries are Nix-linked);
#  * the launcher, which preloads it into the venus `virgl_render_server` (a C
#    program whose allocation hot path is in libvirglrenderer.so, so the
#    interposition has to be process-wide rather than linked into the
#    executable).
#
# Why not nixpkgs' own version: a 3.3.2 preload faults when the preloaded object
# is called before its own initialisation - ld.so runs a preload's constructor
# after the binary's dependency initialisers, so libstdc++'s static initialiser
# (the C++ locale) reaches an allocator that is not ready - and the guest's `zg`
# died with a SIGSEGV before `main`. 3.5.3 does not, with nothing else changed
# (docs/wayfinder/pi-native-addons/notes/10-raw-preload-early-call.md). The
# headless pin carries 3.6.1; the source fallback keeps the version guard so the
# shim retires itself once the inputs catch up.
{ pkgs, pins }:
let
  release = pins.mimallocPrebuiltRelease or { };
  systems = release.systems or { };
  system = pkgs.stdenv.hostPlatform.system;
  base = pkgs.mimalloc;
in
if builtins.hasAttr system systems then
  let
    asset = builtins.getAttr system systems;
  in
  pkgs.callPackage ../pkgs/mimalloc-prebuilt.nix {
    runtimeDeps = [ pkgs.stdenv.cc.cc.lib ];
    releaseAsset = asset // {
      inherit system;
      inherit (release)
        owner
        repo
        tag
        version
        ;
      url = "https://github.com/${release.owner}/${release.repo}/releases/download/${release.tag}/${asset.asset}";
    };
  }
else if pkgs.lib.versionAtLeast base.version "3.5.3" then
  base
else
  base.overrideAttrs (_old: {
    version = "3.5.3";
    src = pkgs.fetchFromGitHub {
      owner = "microsoft";
      repo = "mimalloc";
      tag = "v3.5.3";
      hash = "sha256-GmLMWdUR2/VXJY7A8dZtwoV9FJIx0sxj8KWI4kG8IrU=";
    };
  })
