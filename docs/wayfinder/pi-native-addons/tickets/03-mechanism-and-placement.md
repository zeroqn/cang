---
label: wayfinder:grilling
title: Which mechanism carries the C++ runtime to pi's addons, and where does it live?
status: closed
blocked_by: ["01-glibc-addon-resolution-today", "02-loader-visible-compat-libs"]
claimed_by: bob + pi session (2026-09-29)
---

## Question

Given ticket 01's failure and resolution chain and ticket 02's menu of
loader-visible mechanisms, decide - with bob - the mechanism and its placement:

- **The mechanism.** Candidates, none yet ruled out:
  - an `LD_LIBRARY_PATH` wrapper around `pi` (in `nix/pkgs/pi-coding-agent.nix`
    so host and guest share it, and/or in an image layer), and whether `node` and
    `bun` need wrapping too;
  - a shipped compat lib directory plus whatever makes the loader see it without
    an inherited variable (ticket 02's verdict);
  - an addition to the image Env in `nix/image/config.nix`;
  - a cang-side step that patches the addon tree's RUNPATH (on pi start or as an
    explicit command) - note this mutates the user's `~/.pi`;
  - some combination, e.g. a compat lib dir in the image plus a wrapper that
    points `LD_LIBRARY_PATH` at exactly that directory.
- **Placement.** The change must reach the NixOS host as well as the guest. The
  host consumes `unstable-inputs.cang.packages.<system>.pi-coding-agent`, so the
  pi derivation is the one place that reaches both; the image Env reaches only
  the guest. Say which, and what bob must do on the host (flake input bump,
  rebuild).
- **Blast radius.** Whatever is inherited by pi's process tree reaches nested
  `cang`, the `nix`/`podman`/`docker`/`rustc` wrappers, `cargo`, and chromium. In
  this repo inherited loader and preload state is already treated as a hazard
  (those wrappers unset `LD_PRELOAD`; an empty file is bind-mounted over
  `/etc/ld-nix.so.preload` for `rustc`/`rust-analyzer`). Weigh what each
  candidate does to those consumers.
- **Coverage.** bob scoped the class as "any dlopen'd glibc addon": does the
  chosen mechanism cover `node`/`npm` runs the agent launches itself, or only
  pi's tree - and if only pi's, is that an accepted limit or a second ticket?
- **Evidence and invariant.** What proves it (a scored live-guest probe in the
  style of `tools/chromium-cang-smoke/`, a host-side import check), and where the
  invariant that catches a regression lives (`nix/image/checks.nix`
  `wrapperContracts`, a repository test, or the smoke's own assertions).

## Deliverable

The decision as this ticket's resolution, an ADR in `docs/adr/` if it is hard to
reverse and surprising without context, and the tickets the decision graduates
(host checklist, invariant, docs, per-arch).

## Resolution (2026-09-29, bob + pi session)

Decided, with the evidence from tickets 01 and 02 (`../notes/01-*.md`,
`../notes/02-*.md`):

1. **Delivery: a `pi` wrapper in `nix/pkgs/pi-coding-agent.nix`.** `$out/bin/pi`
   stops being a symlink to the raw bun binary and becomes a wrapper that
   prepends the native addon runtime directory to `LD_LIBRARY_PATH`. One change
   reaches the guest (image layer) and the host (the dev host takes
   `pi-coding-agent` from the cang flake), and it covers everything the agent
   launches - extensions, `node`/`npm`/`bun` runs, subagents. Accepted limit: a
   dynamic process started from the guest task shell *outside* pi's tree is not
   covered, and the raw `$out/lib/pi-coding-agent/pi` still bypasses the wrapper.
2. **Artifact: the soname only.** A cang-owned directory exposes exactly
   `libstdc++.so.6` (built from the flake's gcc, which the image carries anyway).
   `LD_LIBRARY_PATH` is searched *ahead of* a binary's own `DT_RUNPATH`, so the
   directory is deliberately scoped to the one library addons are missing rather
   than pointing at gcc's whole lib directory.
3. **The `--alloc=hardened` stopgap gets documented, not defaulted.** The guest
   already loads both addons today with `--alloc=hardened`, because cang's
   graphene-hardened-malloc carries libstdc++ in its own `DT_NEEDED` - the
   host's accident reproduced deliberately. It is a stopgap for someone hitting
   this now; the allocator stays mimalloc by default, because `--alloc=glibc`
   would break the addons again and the allocator policy should not be load
   bearing for a loader problem.
4. **Evidence: a scored live-guest probe plus a repo invariant.** Both addons
   must import inside a real guest (probe exits non-zero otherwise), and a cheap
   repo check must fail when the wrapper or its directory is unwired.

Consequences recorded in the map: the wrapper is now the entry point that
matters, `versionCheckHook`/`--version` must keep working through it, and any
future addon needing a *different* soname needs its own decision.

Recorded as ADR `docs/adr/0009-pi-extension-cxx-runtime-delivery.md`.
