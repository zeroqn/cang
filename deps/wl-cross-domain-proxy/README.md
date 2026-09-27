## wl-cross-domain-proxy

A simple proxy for the wayland protocol across virtio-gpu cross-domain context.

## What?

CrosWM supports wayland applications through `sommilier`, which is a (not so) little daemon,
that acts as a wayland-compositor in the guest, while connecting through the kernel to the hypervisor
connecting to a wayland-compositor on the host.

Initially it used a custom kernel module called VirtWL for this, but after some time a new effort to run
on top of virtio-gpu was developed and merged upstream. This uses what the kernel driver calls a cross-domain context.

`sommilier` has a bunch of more features though. It can proxy the wayland-protocol modifying state, hiding globals and so on
and also run Xwayland on the guest side, creating wayland objects to represent the X11 windows. All of this makes it a quite
big and highly integrated daemon that is barely tested outside of Chrome OS.

That is what lead to a project called [`wayland-proxy-virtwl`](https://github.com/talex5/wayland-proxy-virtwl), which
reimplements a bunch of the logic from `sommilier` in OCaml and also approaches the proxying in a similar way, also
supporting running Xwayland.

`wl-cross-domain-proxy` does not support arbitrary proxing and modifying of the wayland state, nor does it run Xwayland.
It is a plain wayland-proxy through virtio-gpu only and nothing more.

## Why?

While `wayland-proxy-virtwl` succeeds at being `sommilier`, but actually maintainable, while implementing a lot
of the features of it, hacking on it is not very approachable for a lot of programmers due to the language in use.

Additionally it chooses to act as both a wayland-server and wayland-client, which means it needs to explicitly
support a bunch more of the wayland-protocol state, than technically required, requiring potentially much more
code to support additional protocol extensions.

Lastly contributions to it seem to have stalled, with features for hardware-accelerated buffer sharing (dmabuf support)
being blocked on reviews for a long time.

Thus `wayland-proxy-virtwl` makes it the explicit goal of parsing as little of the wayland-protocol as possible,
while supporting proxying as many features supported by the host-compositor in question as possible.

It does not make an effort to be a generic wayland-proxy, that can filter inputs from a "parent" compositor.
It also makes no effort to proxy Xwayland. Instead users are advised to run something like `xwayland-sattlelite`
on top of `wl-cross-domain-proxy` or proxy X11 separately (e.g. through adopting `muvm-bridge`).

## How do I use this?

- Compile using `cargo build --release`
- Make sure `XDG_RUNTIME_DIR` is set
- Run the binary
