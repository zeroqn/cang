---
label: wayfinder:task
title: Install the pi wrapper and the native addon runtime directory
status: open
blocked_by: []
claimed_by: ""
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
