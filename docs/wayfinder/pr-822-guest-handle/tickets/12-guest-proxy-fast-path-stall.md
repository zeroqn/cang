---
label: wayfinder:task
title: Why does the guest proxy stall once the CREATE_GUEST_HANDLE path is live?
status: open
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
