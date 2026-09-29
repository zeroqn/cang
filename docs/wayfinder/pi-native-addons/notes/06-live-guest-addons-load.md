# Ticket 06 - both addons loading inside a real cang guest (post-fix, default allocator)

Status: IN PROGRESS - skeleton pinned before the expensive build/VM run; filled in incrementally.

## Question

Prove inside a real cang microVM, built from the CURRENT tree, that a pi
extension's glibc-linked addons (`sharp`, `onnxruntime-node`) load **because of
the ticket-05 mechanism** (commit `1d8272d`: `bin/pi` is a `makeWrapper` script
prepending `${nativeAddonRuntimeDir}/lib`, which holds exactly
`libstdc++.so.6`, to `LD_LIBRARY_PATH`).

Ticket 04's pre-fix live-guest run is the baseline; its labelled post-fix
addendum is the starting point.

## Pinned inputs (before the run)

- repo HEAD: TBD
- image tar: TBD
- image digest: TBD
- cang binary: TBD
- guest-init: TBD
- allocator: default (mimalloc), no `--alloc` flag
- launch: TBD

## Answer

TBD

## Raw evidence

TBD
