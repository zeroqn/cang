# /nix generation-switch live smoke

Proves, inside real cang microVMs, that a workspace `/nix` survives an image
move without losing a single store path — the property the snapshot-based
generation switch exists to give.

Run it with two images whose digests differ, the one the workspace starts on and
the one it moves to:

```bash
tools/nix-generation-smoke/nix-generation-smoke.sh \
  --cang ./result/bin/cang \
  --previous-image localhost/cang:previous \
  --image localhost/cang:latest
```

It boots three times and scores what each boot reports, from files the guest
writes to `/workspace/evidence` (so stale evidence cannot pass — every report is
checked for mtime against the run's start):

| boot | image | what must hold |
|---|---|---|
| `boot-fresh` | `--previous-image` | `/nix` generation inode is **256** (a btrfs subvolume), a store closure imports, `verify_bad=0`, the imported binary runs |
| `boot-seeded` | `--image` | same verification, the staged image DB was consumed (`staged_image` empty), the live DB has **at least** as many rows as the previous generation (the merge ran), the closure the previous generation supplied is still present and still verifies |
| `boot-steady` | `--image` | identical verification result, no drift |

Every boot also runs `cang-nix-store-repair --dry-run` and requires
`registered-but-absent=0 present-but-unregistered=0`; anything else fails the
smoke.

## Why these assertions

The generation being a subvolume is what makes an image move a `btrfs subvolume
snapshot` (~25 ms, 0 bytes) instead of a per-file clone, and it is also what
keeps the overlay's `upperdir` and `workdir` on one filesystem. The verified
store is what catches a layer-spanning inode-identity bug: with the image's nix
tree and the workspace upper in two subvolumes, both number their inodes from
257 under one synthesized `st_dev`, and an unverified inode cache answers a
lookup with the wrong object — the guest then sees `EISDIR` on a write, or a
store path holding another object's bytes.

## Requirements

- a btrfs output filesystem for the state root (the smoke asserts inode 256); a
  non-btrfs state root falls back to the reflink clone and this smoke will not
  pass, by design
- two locally available cang images with different digests (e.g. the previous
  release and the current build, loaded with `podman load`)
- the default imported closure is `hello` from `cache.nixos.org`; override it
  with `CANG_SMOKE_SOURCE_PATH` / `CANG_SMOKE_SOURCE_SUBSTITUTER`. The path must
  **not** already exist in either image's store, or the import copy-ups over a
  lower-layer object instead of creating one.

Artifacts (`evidence/`, `logs/`, `summary.txt`) land in `--out`, or a temp dir
that is printed. `--keep` leaves the workspace and state home in place for
inspection.
