---
label: wayfinder:task
title: Accept a real host block device (or partition) as the workspace backing, as a second step
status: open
blocked_by: [18]
claimed_by: ""
---

## Question

bob's Q3 answer (2026-11-20) was "raw image first, a real `/dev/sdx` as a second
ticket": same `DiskAttachment`, so this is a flag and a validation, not a new
mechanism. Which device, and under what rules?

Do:

1. **Surface**: how a host device is named (`--workspace=dev:/dev/sdb1`?), and
   how it is told apart from an image file.
2. **Refusals, by name**: not a block device; a mounted filesystem (host has it
   open - `statfs`/`/proc/self/mountinfo`); wrong filesystem; device smaller than
   the tree; a device that is a host root or swap (an explicit list, or the
   `blkid` type).
3. **Ownership and blast radius**: the guest can reformat or grow what it is given,
   so this needs a stated rule (never write where a host process has it open,
   never "helpfully" `mkfs` an existing fs) and a documented warning in
   `docs/images-and-storage.md`.
4. **The honest check**: a loop-mounted image registered as a device is not the
   same as a real one; prove at least one arm on real hardware or say plainly that
   only the refusal paths were exercised.

## Resolution

(to follow)
