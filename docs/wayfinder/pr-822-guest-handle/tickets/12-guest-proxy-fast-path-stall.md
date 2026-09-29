---
label: wayfinder:task
title: Why does the guest proxy stall once the CREATE_GUEST_HANDLE path is live?
status: closed
claimed_by: pi session (2026-09-29)
blocked_by: ["05-libkrunfw-kernel-support", "07-fork-vmm-port"]
claimed_by: (unclaimed)
---

## Question

Ticket 09's first completed fast-path run reaches `UDMABUF_CREATE` and
`PRIME_FD_TO_HANDLE` in the guest proxy and then never answers the client's first
`wl_display.sync`, staying in a `DRM_IOCTL_VIRTGPU_EXECBUFFER` retry loop while
the client waits. Split the layers and find the one that stops:

1. Capture the VM worker's libkrun output on a *successful* run (the supervisor
   does not forward it today) and look for the fork's warning paths - the
   cross-domain mis-route warning in particular (`a1a772a0`).
2. Compare the host side of the blob create for a guest-handle blob against a
   plain blob: the fork's `resource_create_blob` arm, what rutabaga does with the
   handle on the cross-domain context, and whether a response is ever written
   back for it.
3. Decide whether the guest proxy (upstream PR #24 code, unchanged here) is
   waiting on something the host never sends, or the host is waiting on the
   guest.

Done when the fast-path run completes a `wl_display.sync` roundtrip and the
proxy's own mode line/udmabuf evidence still shows the zero-copy handler.

## Deliverable

The diagnosis (with the layer named and the evidence), and either a fix or a
precise statement of what has to change upstream.

## Resolution (2026-09-29, pi)

**The host device rejected the blob's udmabuf create; the guest proxy then
retried forever.** The VM worker's log (which the supervisor does not forward on
a successful run; `--preserve-debug` keeps `<task>/helper.stderr.log`) named it:

```
WARN krun_devices::virtio::gpu::virtio_gpu] Failed to create udmabuf for resource 4:
  system call returned EINVAL (entries=2025, bytes=8294400,
  first=Some((GuestAddress(4602589184), 4096)))
DEBUG ... worker] Some(ResourceCreateBlob) -> ErrUnspec
```

The guest names one dma-buf entry per 4 KiB page, so an 8 MiB pool is 2025 runs;
the udmabuf driver's `list_limit` is 1024, and `UDMABUF_CREATE_LIST` rejects more
with a bare `EINVAL`. Measured on this host's `/dev/udmabuf`: 1024 runs ok, 2025
and 2048 `EINVAL`, 128 MiB `EINVAL` (`size_limit_mb` 64 MiB), 8 MiB with
`F_SEAL_SHRINK` ok (no seals / `GROW` only / `WRITE` sealed all `EINVAL`).

`deps/libkrun` `63f3737f` merges adjacent runs in the same memfd before the ioctl
(a contiguous pool becomes one item), refuses what is still over the limit with a
named error, and logs the request shape on any failure. With it the run completes,
the proxy answers `wl_display.sync`, and ticket 09 measures the A/B.

Nothing upstream is needed: the kernel's two limits are contract, not bug - the
bug was in the fork's one-item-per-page list.
