---
label: wayfinder:task
title: Reproduce the addon failure inside a real cang guest before the mechanism is chosen
status: closed
blocked_by: []
claimed_by: pi research child-3 (2026-09-29)
---

## Question

Unit of work, not a decision: boot a real cang microVM from the current tree and
show, with fresh evidence, what `require('sharp')` and
`require('onnxruntime-node')` do inside it - the failure the mechanism ticket is
being decided on. Ticket 01 established the failure by reproducing the guest's
loader condition on the host (bun + the guest's `pkgs.mimalloc` preload, `/etc`
masked); this ticket is the same claim made on the real target, plus the container
image build that every later verification needs anyway.

Scope:

- Isolated config/state and a hermetic container storage directory, house recipe
  from `tools/chromium-cang-smoke/chromium-smoke.sh`; the 40G btrfs loop image at
  `/home/dev/cang/disk` is the place for both (the host home filesystem is small).
- Record: the exact launch, the image digest, the `pi`/`bun` identity in the
  guest, both modules' outcomes verbatim, and `LD_DEBUG=libs` evidence for how
  `libstdc++.so.6` was sought (and what the guest's `/etc/ld-nix.so.preload`
  contained at the time).
- Do not change the repository or the image for this ticket; it is a measurement.

Known risks to name in the answer if they bite: `nix build .#container` failed
earlier in the run (a `syn` compile failure while the host disk was full, and the
image wrapper checks fail if the store DB check misses references), and a cold
image build can take a long time.

Resolved when the in-guest outcome is recorded as evidence, whether or not it
matches ticket 01's prediction.

## Resolution (2026-09-29) - confirmed in a real guest

Image digest `sha256:b821b52eb4a1cc88d7a4f94512b87bc0debe75255b14fd729b1f537599dfcb9d`,
built from HEAD `fd8c0a1` (i.e. **before** the ticket-05 fix, as the ticket asked),
launched `cang --mem 4 --alloc {mimalloc|hardened} --seccomp=off --landlock=off
--guest-init <musl> -- sh /workspace/probe.sh`. Full write-up:
`../notes/04-live-guest-failure.md`, raw evidence `../notes/04-raw-*.txt`.

| runner | module | default allocator | `--alloc=hardened` |
|---|---|---|---|
| `bun` (the `pi` runtime) | `sharp` | **FAIL** `ERR_DLOPEN_FAILED: libstdc++.so.6: cannot open shared object file` | **LOAD_OK** |
| `bun` | `onnxruntime-node` | **FAIL** `libstdc++.so.6: cannot open shared object file` | **LOAD_OK** |
| `node` (control) | both | LOAD_OK | LOAD_OK |

Ticket 01's prediction reproduced on the target, including the `node` control
that would have declared the guest healthy.

`LD_DEBUG=libs` in the guest shows the failing search - the addon's own `$ORIGIN`
chain, then the loader's system path (its own store `lib` plus
`xgcc-15.3.0-libgcc`), with `libstdc++.so.6` nowhere, although the image does
contain `gcc-15.3.0-lib/lib/libstdc++.so.6`. Under `--alloc=hardened` the same
lookup succeeds because `libhardened_malloc.so` NEEDs libstdc++ and pulls it into
the global scope before the addon opens (`/proc/self/maps` already shows
`libstdc++.so.6.0.34` at process start).

Corrections and refinements this run adds:

- **A preloaded object's RUNPATH is not a rescue.** The mimalloc preload's own
  RUNPATH does contain `gcc-15.3.0-lib/lib`, but it only serves mimalloc's own
  `DT_NEEDED`; the later `dlopen` of the addon still fails. Ticket 01 had flagged
  this as uncertain.
- **The guest does have `/lib` and `/usr/lib`** (compatibility farms: bash,
  clang, mesa/dri, musl stubs; `/usr/lib` holds only the cang fontconfig/mesa/
  software-renderer payloads). Neither contains `libstdc++.so.6` and neither is
  on the loader's system search path, so ticket 01's operative claim stands.
- **The loader looks for its cache at its own store path** -
  `search cache=/nix/store/...-glibc-2.42-84/etc/ld.so.cache` - not at
  `/etc/ld.so.cache`, and no such file exists. That rules the "ship an
  `/etc/ld.so.cache`" mechanism out; see the correction in ticket 02's note.
- **Two image-build flakes**, both survivable by a plain retry: the
  `cang-guest-init` musl checkPhase test
  `guest_init::components::podman::service::tests::rootless_info_verification_preserves_failure_stderr`
  (assertion at `service_tests.rs:61`), and a mirror `HTTP error 416` on a
  `.nar.zst` fetch that propagated into the `missingImageConfigNixDbRefs` image
  check. Verbatim in `../notes/04-raw-nix-build-attempt1.txt`.

Caveat recorded by the run: the probe drives `bun -e "require(...)"` from the
`pi` tree's `node_modules` - the same resolution path an extension uses - rather
than launching the `pi` binary itself, which needs a TTY/session.
