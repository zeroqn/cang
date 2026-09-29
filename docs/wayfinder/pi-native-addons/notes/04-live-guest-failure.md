# Ticket 04 - the addon failure inside a real cang guest

Status: IN PROGRESS (evidence files written incrementally).

## What this ticket is

Ticket 01 proved the failure by reproducing the guest's *loader condition* on
the host (bun + the guest's `pkgs.mimalloc` preload, `/etc` masked). This ticket
makes the same claim on the real target: boot a real cang microVM from the
current tree and record what `require('sharp')` and `require('onnxruntime-node')`
do inside it.

Nothing in the repository or the image is changed for this ticket; it is a
measurement. All state created lives under
`/home/dev/cang/disk/pi-native-addons-04` (the 40G btrfs loop image), never the
ambient `~/.config/containers/storage.conf` or a pre-existing cang store.

## Pinned inputs

- repo HEAD at start: `fd8c0a1449aa32969f6b315fb456d303768df10d`
- image built from the same tree: `nix build .#container`
- cang binary + guest-init: `nix build .#cang .#cang-musl` (current tree)

(filled in as the run proceeds)
