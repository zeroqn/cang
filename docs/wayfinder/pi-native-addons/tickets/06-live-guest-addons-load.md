---
label: wayfinder:task
title: Show both addons loading inside a real cang guest
status: open
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: ""
---

## Question

The destination's live evidence: prove inside a real cang microVM that a pi
extension's glibc addons load with the ticket-05 mechanism in place.

Build a scored probe in the style of `tools/chromium-cang-smoke/chromium-smoke.sh`:

- boots a guest from the built image with an isolated config/state home and a
  hermetic container storage directory (never the ambient
  `~/.config/containers/storage.conf`), state and store on the btrfs disk at
  `/home/dev/cang/disk`;
- runs both modules in the guest - `require('sharp')` and
  `require('onnxruntime-node')` from `~/.pi/agent/git/github.com/zeroqn/pi` -
  under the guest's default allocator (mimalloc) and without any extra
  environment, so it proves the *wrapper* carried it;
- asserts the positive evidence and exits non-zero otherwise, with freshness
  checks on the evidence files so a stale result cannot pass;
- records the image digest, the launch command, the guest's
  `/etc/ld-nix.so.preload` contents, and `LD_DEBUG=libs` for the `libstdc++.so.6`
  lookup.

Ticket 04's live-guest run is the pre-fix baseline for the same harness; reuse
whatever it built rather than starting from scratch.
