---
label: wayfinder:task
title: Publish the permanent v2.0.0-cang.1 fork release
status: open
blocked_by: ["04-rebase-cang-onto-main"]
claimed_by: unclaimed
---

## Question

Turn the rebased branch into a permanent, attestation-carrying release:

1. Force-push `cang` to `zeroqn/libkrun` (bob performs or authorises the push;
   this session can push but cannot publish).
2. Confirm the fork's CI publishes the **rolling** `cang-<12hex>` prerelease
   from that push and that its assets are the expected
   `libkrun-<arch>-linux-full.tgz`.
3. Bob dispatches `publish-cang-release.yml` with `version=2.0.0-cang.1`; the
   workflow validates the version's base against the branch's `FULL_VERSION`
   (2.0.0 on main) and refuses a version already published at another commit.
4. Verify the run: the packing step must use `LIBDIR_Linux=lib64` (the known
   trap: the fork's workflow was copied from an older upstream and failed the
   tar step on Debian runners), the release is `draft: false`, and every
   required asset is present and attested.

## Deliverable

The tag, the asset list with sizes, the SRI hashes of the x86_64 and aarch64
assets, and the CI run URL - the inputs ticket 07 pins.
