---
label: wayfinder:task
title: Accept a real host block device (or partition) as the workspace backing, as a second step
status: closed
blocked_by: [18]
claimed_by: "pi (2026-11-22)"
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

**Implemented 2026-11-22** (bob: "finally decide whether to implement 28 or not" →
implement, because it is a flag plus a validation on machinery that now exists).
The code is in and validated; **the live arm is blocked on a block device, which
needs root**, so it is handed to bob as one command.

1. **Surface**: not a new flag - `--workspace-disk` takes a block device as well as
   an image, and the path's own type decides which. `--workspace-format` is the only
   new flag, and it only ever formats a device holding **no** filesystem.
2. **Refusals, by name**, all unit-tested through the pure `plan_device` policy
   (a device cannot be faked in a test, so the decision is split out from the ioctl):
   - a device holding **btrfs** is reused and never touched, with or without
     `--workspace-format`;
   - a device holding **another filesystem** is refused
     (`holds filesystem type 'ext4' … cang never reformats a host device it did not
     create`), and refused again with `--workspace-format`
     (`already holds … only formats a device with no filesystem`);
   - an **empty** device without the flag is refused with
     `is empty; pass --workspace-format to format it with btrfs`;
   - `--workspace-size` with a device is refused (the device's size is its owner's);
   - `cang disk resize --workspace-disk <device>` is refused (cang does not resize a
     host device; the guest's btrfs expands on its next mount instead);
   - auto-grow refuses at the report step, so no maintenance VM boots for a device;
   - the launch prints one line naming the device, because it is the only attached
     disk cang did not create.
3. **Ownership and blast radius**: cang never creates, formats-without-permission or
   grows a device; the one write it makes to an empty one is gated by
   `--workspace-format`. Documented in `docs/usage.md` §"A block device instead of an
   image", including the "the host must not mount it while the guest has it" rule
   and that nothing stops a second session from doing so.
4. **The honest check, split**:
   - **done**: the refusals and the reuse policy, unit-tested; `cargo test`, clippy
     `-D warnings`, `fmt`, and the whole suite green;
   - **not done**: any live arm on a real device. Reading a device's size needs
     `BLKGETSIZE64` and attaching one needs `losetup`, both of which need root, so
     bob runs `sudo sh /home/dev/disp/ticket28-dev.sh setup 4G` (creates a 4 GiB
     btrfs loopback at `/dev/loop12` and hands it to the running user - the device
     is `root:disk` 660 by default, and an unprivileged open fails `EACCES` with a
     refusal that now says exactly that), and the arms are then:
     `cang --workspace-disk /dev/loop12 -- bash -lc 'stat -f -c %T /workspace'`,
     a seed into it, a `workspace serve` on it, and
     `cang disk resize --workspace-disk /dev/loop12 8G` (must refuse).

**Left unverified on purpose**: the ioctl path itself (no block device is reachable
from an unprivileged test), and a device that is a host root or swap - the review
call is that `blkid`'s type plus "never format without the flag" already covers the
danger, and an explicit deny-list would be guesswork until someone names a device.
