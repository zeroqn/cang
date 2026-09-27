---
label: wayfinder:task
title: GPU smoke the new pin
status: open
blocked_by: ["07-pin-and-adapt-cang"]
claimed_by: unclaimed
---

## Question

Prove the rebased libkrun still does GPU/wayland work on this host, honestly:

1. Run `tools/chromium-cang-smoke` (GPU and wayland modes) against the new pin
   on the btrfs-backed live-VM setup. The PR-822 guest-handle fast path is **not**
   part of this map, so the smoke cannot and must not be expected to exercise it.
2. Attribute any failure: is it introduced by the rebase, by a cherry-picked PR,
   or pre-existing? When unsure, run the same smoke against the **old pin**
   (v1.19.5-cang.1) to separate the two, rather than assuming.
3. Record the result: the smoke's verdict, the exact revision pair exercised,
   and where the baseline lives.

Standing rule for this map: never weaken a smoke assertion to force a green run;
report the failure with its attribution instead.

## Deliverable

The smoke verdict per mode, the A/B attribution when something fails, and the
location of the recorded baseline/evidence.
