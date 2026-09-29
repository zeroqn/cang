---
label: wayfinder:task
title: Install the pi wrapper and the native addon runtime directory
status: closed
blocked_by: []
claimed_by: pi session (2026-09-29)
---

## Question

Do the work ticket 03 decided:

- Add the **native addon runtime directory**: a cang-owned derivation exposing
  exactly `libstdc++.so.6` from the flake's gcc (`pkgs.stdenv.cc.cc.lib`), so a
  lookup that would otherwise fall through to nothing finds the one soname
  prebuilt addons are missing. One soname only - see ticket 03's reasoning.
- Change `nix/pkgs/pi-coding-agent.nix` so `$out/bin/pi` is a wrapper that
  prepends that directory to `LD_LIBRARY_PATH` and execs
  `$out/lib/pi-coding-agent/pi`, which stays the raw binary. The repo's wrapper
  style is a plain shell script (`nix/image/layers.nix`), not `makeWrapper`.
- Keep `doInstallCheck`/`versionCheckHook` passing through the wrapper
  (`$out/bin/pi --version`), and make sure `nix build .#pi-coding-agent` and
  `nix build .#container` are both green.

Acceptance: `pi` runs normally with the wrapper in place on the host, and a
process started under the wrapper's environment can `dlopen` a libstdc++-linked
addon even when no malloc provider is supplying libstdc++ (the sandbox probe in
`../notes/01-glibc-addon-resolution.md` is the harness: mask `/etc`, then
`bun -e "require('onnxruntime-node')"` must report LOAD_OK).

Out of scope for this ticket: the live-guest proof (ticket 06), the invariant
(ticket 07), the docs (ticket 08) and bob's host rebuild (ticket 09).

## Resolution (2026-09-29) - landed and verified

`nix/pkgs/pi-coding-agent.nix`:

- a `cang-native-addon-runtime` derivation exposes exactly one entry,
  `lib/libstdc++.so.6` -> `${pkgs.stdenv.cc.cc.lib}/lib/libstdc++.so.6`, and is
  exported as `passthru.nativeAddonRuntimeDir` so a check (ticket 07) can reach
  it without a new flake output;
- `installPhase` no longer symlinks `bin/pi`; it runs
  `makeWrapper $out/lib/pi-coding-agent/pi $out/bin/pi --prefix LD_LIBRARY_PATH : ${nativeAddonRuntimeDir}/lib`,
  so the raw bun binary stays at `lib/pi-coding-agent/pi` and `pkgs.makeWrapper`
  joined `nativeBuildInputs`.

Evidence:

- `nix build .#pi-coding-agent` -> `/nix/store/nwvcnfx3bma7h97gzhh79msr3g4jswww-pi-coding-agent-0.87.1`.
  The derivation's own `versionCheckHook` runs `bin/pi --version` *through the
  wrapper* and now prints `0.87.1`.
- The wrapper body exports
  `LD_LIBRARY_PATH=/nix/store/0hpv152zh95hv36a8ksi0iywicgaphpn-cang-native-addon-runtime/lib`
  ahead of any existing value and execs the inner binary.
- Loader-condition probes (guest condition: `/etc` masked so the NixOS malloc
  preload disappears; evidence in `/home/dev/cang/disk/pi-addon-probe/ticket05`):

  | condition | sharp | onnxruntime-node |
  |---|---|---|
  | control, no runtime dir | LOAD_FAIL | LOAD_FAIL (`libstdc++.so.6: cannot open shared object file`) |
  | runtime dir only | **LOAD_OK** | **LOAD_OK** |
  | guest default allocator (`LD_PRELOAD=mimalloc`) + runtime dir | **LOAD_OK** | **LOAD_OK** |

  The middle row is the point of the artifact decision: a directory holding
  *only* `libstdc++.so.6` is sufficient, so nothing else in a nix binary's search
  is pre-empted.
- `pi list` and `pi --help` run through the wrapper under the same masked
  condition with rc 0 and no addon load error.
- `nix build .#container` -> rc 0 (`/nix/store/8vw3xrkv5zfrkri08w7savlabjbvpsax-cang.tar.gz`),
  including `cang-image-wrapper-contracts-check`, so the image is green with the
  wrapped pi; `nixfmt --check` clean on the changed file.

Lesson worth keeping: the first wrapper attempt was a `cat > ... <<EOF` heredoc,
and the *build* shell expanded `$@`/`$LD_LIBRARY_PATH`/the runtime variable to
nothing, producing a wrapper that dropped every argument - the derivation's
`versionCheckHook` caught it (`pi --version` started a session and printed "No
API key found"). Any future hand-written wrapper in this repo should either
quote its heredoc or use `makeWrapper`.

Unblocked by this: ticket 06 (live guest), 07 (invariant), 08 (docs), 09 (host
checklist).
