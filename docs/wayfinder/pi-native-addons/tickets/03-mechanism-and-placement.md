---
label: wayfinder:grilling
title: Which mechanism carries the C++ runtime to pi's addons, and where does it live?
status: open
blocked_by: ["01-glibc-addon-resolution-today", "02-loader-visible-compat-libs"]
claimed_by: ""
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
