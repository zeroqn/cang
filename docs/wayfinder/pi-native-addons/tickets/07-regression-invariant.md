---
label: wayfinder:task
title: Make a regression in the wrapper wiring fail loudly
status: closed
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: pi session (2026-09-29)
---

## Question

The cheap guard the destination promises: something in the repo that fails when
the ticket-05 wiring is undone - the wrapper losing its `LD_LIBRARY_PATH`
prefix, or the runtime directory losing `libstdc++.so.6`.

Pick the cheapest check that actually catches both, following the repo's
existing patterns:

- `nix/image/checks.nix` wrapper contracts assert the image carries the wiring
  (see `wrapperContracts`, and the absence-check recipe for layer contents);
- a repository test under `crates/cang-repository-tests/` can assert the
  derivation's shape (`nix/pkgs/pi-coding-agent.nix` installs a wrapper that
  names the runtime directory, and the directory derivation links
  `libstdc++.so.6`);
- the probe from ticket 06 is the end-to-end complement, not a substitute: it is
  slow and needs a guest.

Requirement: build the check *first* and confirm it fails against the unwired
tree, then confirm it passes once ticket 05 lands.

## Resolution (2026-09-29) - red/green demonstrated

One `nativeAddonRuntimeContracts` block in `nix/image/checks.nix`, added to the
existing `wrapperContracts` `runCommand`, so it rides the already-wired
`container-wrapper-contracts` flake check and the `nix build .#container` path -
no new flake output. It asserts:

- the wiring is still in the derivation: `nativeAddonRuntimeDir` and
  `makeWrapper $out/lib/pi-coding-agent/pi $out/bin/pi` appear in
  `nix/pkgs/pi-coding-agent.nix` (read into the check the way `layers.nix`,
  `config.nix` and `container.nix` already are);
- the artifact is the wrapper, not the old shape: `test ! -L
  ${piCodingAgent}/bin/pi`, the inner `lib/pi-coding-agent/pi` is executable, and
  both `${piCodingAgent}/bin/pi` and `${layers.agentImageLayer}/bin/pi` name the
  runtime directory;
- the directory really carries the soname: `test -e
  ${nativeAddonRuntimeDir}/lib/libstdc++.so.6`;
- the wrapper keeps working: `bin/pi --version` through the wrapper;
- **behaviourally** that a child process started through the wrapper sees it:
  `sed 's|^exec .*|exec env|'` rewrites the wrapper's generated exec line into
  `exec env` (with a `cmp` guard, so a wrapper shape the rewrite cannot match
  fails loudly instead of passing vacuously), runs it, and requires
  `LD_LIBRARY_PATH=${nativeAddonRuntimeDir}/lib` in the output. That is the same
  trick as the existing `cang-render-server-env-sources` check, which asserts a
  child process sees an exported variable.

Green: `nix build .#checks.x86_64-linux.container-wrapper-contracts` -> rc 0
(`/nix/store/n8m6v56a6gv2yxmrwa4mi9gh7zdw9mvl-cang-image-wrapper-contracts-check`).

Red, by overriding the pi derivation so the tree is genuinely unwired
(`nix build --impure -f <expr> redWrapper|redDir`, both rc 1):

| override | failure |
|---|---|
| `installPhase` reverted to the pre-fix shape (`ln -s ../lib/pi-coding-agent/pi $out/bin/pi`) | `test ! -L ${piCodingAgent}/bin/pi` - walked the block against the red output `/nix/store/0ygnmd5mw7g23qq36kvawb6p5qqrn3sf-pi-coding-agent-0.87.1`, where that one line is the only failing assertion (the runtime dir and the inner binary both still pass) |
| `passthru.nativeAddonRuntimeDir` pointing at an empty directory | `test -e ${nativeAddonRuntimeDir}/lib/libstdc++.so.6` - the block's first artifact assertion, and the last thing the build log reaches |

Both red logs contain no disk-full errors (`grep -c 'no space'` = 0); an *earlier*
red attempt is explicitly discarded because its pi rebuild died with
`no space left on device`, which is a different failure and no evidence at all.

Recipe for re-running the red cases:

```nix
let
  f = builtins.getFlake "/home/dev/cang/cang";
  sys = "x86_64-linux";
  pkgs = f.inputs.nixpkgs.legacyPackages.${sys};
  bun = f.inputs.nixpkgs-unstable.legacyPackages.${sys}.bun;
  p = f.packages.${sys};
  real = p.pi-coding-agent;
  redWrapperPi = real.overrideAttrs (old: { installPhase = <the pre-fix installPhase>; });
  redDirPi = real.overrideAttrs (old: { passthru = old.passthru // {
    nativeAddonRuntimeDir = pkgs.runCommand "cang-native-addon-runtime-empty" { } "mkdir -p $out/lib";
  }; });
  mkChecks = piCodingAgent: import /home/dev/cang/cang/nix/image/checks.nix {
    inherit pkgs bun piCodingAgent; # plus the remaining image-check arguments from f.packages
  };
in { redWrapper = (mkChecks redWrapperPi).wrapperContracts; redDir = (mkChecks redDirPi).wrapperContracts; }
```

Not chosen: a `crates/cang-repository-tests` text assertion (cheaper to run, but
it cannot see the built wrapper or the runtime directory) and the ticket-06 live
probe (end-to-end, but slow and needs a guest - it complements this check rather
than replacing it).
