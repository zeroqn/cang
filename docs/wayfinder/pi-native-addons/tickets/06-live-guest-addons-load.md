---
label: wayfinder:task
title: Show both addons loading inside a real cang guest
status: closed
blocked_by: ["05-wrapper-and-runtime-dir"]
claimed_by: pi research child-4 / pi-native-addons-06-r2 (2026-09-29)
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

## Resolution (2026-09-29) - proven in a real guest, with the paired control

Image digest `sha256:923d645e8e212064e3fc1f75973efd6291611070e04fef3300b0da3bd3ca8249`
(id `78df820f0812`, tar `/nix/store/l28k4y2f...-cang.tar.gz`, built from HEAD
`4f950c4`), booted with `cang --mem 4 --seccomp=off --landlock=off --guest-init
<none, the image's own>` - no `--alloc` flag, so the guest's default is cang's
`pkgs.mimalloc`. Full write-up `../notes/06-live-guest-addons-load.md`, raw
evidence `../notes/06-raw-*.txt`.

In the guest:

| probe | sharp | onnxruntime-node |
|---|---|---|
| plain `bun` (control) | **LOAD_FAIL** | **LOAD_FAIL** (`libstdc++.so.6: cannot open shared object file`) |
| `bun` with exactly the wrapper's exported `LD_LIBRARY_PATH` | **LOAD_OK** | **LOAD_OK** |
| plain `node` (the known trap, not evidence) | LOAD_OK | LOAD_OK |

and the wrapper is what supplies it:

- guest `command -v pi` -> `...-cang-agent-layer/bin/pi` ->
  `nwvcnfx3...-pi-coding-agent-0.87.1/bin/pi`, the `makeWrapper` script, whose
  runtime directory holds `libstdc++.so.6` and nothing else;
- **a live `pi` process (pid 957) carries
  `LD_LIBRARY_PATH=/nix/store/0hpv152...-cang-native-addon-runtime/lib` in
  `/proc/957/environ`** - the same value the wrapper computes mechanically - so a
  real `pi` really does hand the directory to the process that `dlopen`s an
  extension's addon;
- `LD_DEBUG=libs` resolves it there (`calling init:
  .../cang-native-addon-runtime/lib/libstdc++.so.6`) on the wrapper path and
  errors on the control path;
- `/etc/ld-nix.so.preload` is still mimalloc, so this is not the
  `--alloc=hardened` accident ticket 04 documented.

Causality: the only difference between the two `bun` rows is the value the
wrapper exports - same image, allocator, module tree and boot - and the same
image family measured LOAD_FAIL for both rows before the wrapper existed
(ticket 04).

Two caveats the run recorded itself: (1) the image was built while
`nix/image/checks.nix` was dirty (committed mid-run as `927a21b`), but neither
that commit nor the docs commit `60cb5a6` touches `nix/pkgs/pi-coding-agent.nix`,
`nix/image/layers.nix` or `flake.nix`, and the agent-layer path `9ha2nyk8...`
and wrapped pi `nwvcnfx3...` are identical to the clean tree's build, so the
image content is the current tree's; (2) the probe drives `bun` with the value
the wrapper exports (plus the live-`pi` capture above) rather than opening the
addon from inside a `pi` session, which would need a model and a TTY.

The harness that produced this run is preserved beside the raw output, because
the working copy lived on the btrfs disk outside the repository:
`../notes/06-raw-harness-env.sh` (hermetic `CONTAINERS_STORAGE_CONF`, `TMPDIR`,
`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `CANG_IMAGE` on the btrfs disk),
`../notes/06-raw-harness-run-vm.sh` (host runner: builds/loads the image, boots
the VM under `script`, captures `logs/`), and
`../notes/06-raw-harness-guest-probe.sh` (the in-guest probe, in the version the
recorded run used).
