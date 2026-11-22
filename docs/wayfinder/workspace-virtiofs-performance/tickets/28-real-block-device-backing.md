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
4. **The live arms, on real hardware** (bob attached a **32 GiB real disk,
   `/dev/sdc`**, 2026-11-22; `lsblk` showed one whole disk, no partitions, no
   filesystem, unmounted, not swap before anything was done to it). All eight arms
   below ran on that device with `cang` from `result`:

   | arm | result |
   | --- | --- |
   | `--workspace-size 4G` with the device | refused: `a block device has the size its owner gave it … (it is 32.0 GiB)` |
   | bare `--workspace-disk`, device empty | refused: `is empty; pass --workspace-format to format it with btrfs` |
   | `disk resize --workspace-disk /dev/sdc 40G` | refused: `'…' is a block device (34359738368), and cang does not resize a host device` |
   | `--workspace-format` on the empty device | formatted btrfs, guest saw `/workspace /dev/vdb btrfs` and `df` **32 G** |
   | second launch, no flag | reused it: the file written by the previous launch was still there, `fs=btrfs` |
   | `workspace init /dev/sdc --from tools/` | `cang: seeded 56 entries` |
   | `workspace serve /dev/sdc` | mount up, host read the guest-written `reuse-proof.txt`, `stop` unmounted it 3 s later |
   | `mkfs.ext4` on the device, then both spellings | refused twice by name (`holds filesystem type 'ext4'` / `already holds … will not overwrite one`), and `blkid` still read **ext4** afterwards - the device was untouched |

   Two bugs the real device found, both fixed before the commit that recorded them:
   - **`BLKGETSIZE64` was wrong**: I had `0x80087212`; `_IOC(_IOC_READ, 0x12, 114, 8)`
     is **`0x80081272`**. Every device arm failed with `failed to read the size of
     block device`, which a unit test could never have caught (no device in a test).
   - **`blkid` exit 2 was an error**: `probe_fs_type` treated "nothing detected" as a
     failure, so an *empty* device refused with `failed to detect filesystem type`
     instead of the named "this disk is empty" answer. Exit 2 now returns an empty
     type.

   Afterwards the device was left formatted **btrfs and empty**, so bob can use it
   (`--workspace-disk /dev/sdc`) or clear it with
   `sudo sh /home/dev/disp/ticket28-dev.sh wipe /dev/sdc`.

**Left open**: a device that is a host root or swap - the call is that `blkid`'s
type plus "never format without the flag" already covers the danger, and an explicit
deny-list would be guesswork until someone names a device; and the *loop-device*
variant (`ticket28-dev.sh loop 4G`) is no longer on the critical path now that real
hardware ran.
