# wl_shm commit benchmark

A tiny guest-side Wayland client that commits `wl_shm` frames to a socket at a
fixed rate and reports the cost. It exists for
[wayfinder ticket 09](../../docs/wayfinder/pr-822-guest-handle/tickets/09-live-fast-path-verification.md):
`wl-cross-domain-proxy` imports a `wl_shm` pool as a guest blob, and the host's
`VIRTIO_GPU_F_CREATE_GUEST_HANDLE` advertisement decides whether the proxy
zero-copies that pool or memcpys it on every commit
(`deps/wl-cross-domain-proxy/src/source/channel/wayland.rs`, `ShmPoolHandler`
`Copy` vs `ZeroCopy`: `handle_commit` copies `stride * height` bytes for the copy
handler and returns immediately for the zero-copy one). Running this client under
`cang --wayland` with and without `--zero-copy-shm` is the measured A/B.

It is a script, not a program to build: the cang image ships `python3`, and the
Wayland wire protocol needed here (registry, `wl_shm`, `wl_compositor`,
`wl_surface`) is small enough to speak directly.

## Running it in a cang guest

```bash
# The pool is imported once per wl_shm pool, and only the copy path pays per
# commit, so use a big buffer and a fixed rate for a visible delta.
cang --gpu=drm --wayland --mem 4 --seccomp=off --landlock=off -- \
  python3 /workspace/wl-shm-bench/wl-shm-bench.py --seconds 10 --width 1920 --height 1080 --rate 120

# The same run with the fast path requested.
cang --gpu=drm --wayland --zero-copy-shm --mem 4 --seccomp=off --landlock=off -- \
  python3 /workspace/wl-shm-bench/wl-shm-bench.py --seconds 10 --width 1920 --height 1080 --rate 120
```

The workspace the client is read from is cang's current directory, mounted at
`/workspace`. `guest-init` starts the proxy and exports
`XDG_RUNTIME_DIR`/`WAYLAND_DISPLAY=wayland-0`, which is where the client
connects; the proxy's own `-- PROGRAM` form instead hands the child a connected
fd in `WAYLAND_SOCKET`, which the client also accepts.

## Output

`key=value` lines, so a host-side runner can grep them:

```
socket=/run/user/1000/wayland-0
shm_version=1 compositor_version=4
width=1920 height=1080 stride=7680 pool_bytes=8294400
frames=1200 seconds=10.000 fps=120.00
cpu_user_s=1.234 cpu_sys_s=0.456 cpu_s=1.690 cpu_pct=16.9
guest_cpu_s=1.900 guest_cpu_pct=19.0 guest_cpu_per_frame_us=1583.3
```

`cpu_*` is the client process's own user+system CPU (`os.times`).
`guest_cpu_*` is the whole guest's busy CPU (`/proc/stat`), which is where the
A/B shows up: the proxy's copy happens in the proxy process, so the same client
run costs the guest more with `--zero-copy-shm` off than on, while the client's
own CPU barely moves either way.

## Evidence to pair with it

- The proxy's `udmabuf fast path did not work: ...` debug line is absent on a
  fast-path run (`cang --log-level debug`, or the guest's proxy log).
- An `UMDABUF_CREATE` ioctl (`_IOW('u', 0x42, struct udmabuf_create)` =
  `0x40187542`, 24-byte argument) on the guest's `/dev/udmabuf` from the proxy
  process is the direct proof the pool went through udmabuf:
  `strace -f -e trace=ioctl -p <proxy-pid>`, or `strace -f -o /workspace/proxy.strace
  wl-cross-domain-proxy -- ...` in the guest.
- `VIRTGPU_PARAM_CREATE_GUEST_HANDLE` (param 10) answering 1 on the guest's
  render node, and virtio-gpu feature bits 6/7 negotiated
  (`/sys/bus/virtio/devices/virtio0/features`, a bitstring whose character `i` is
  bit `i`) - see
  [note 05](../../docs/wayfinder/pr-822-guest-handle/notes/05-libkrunfw-kernel-support.md).

## Host prerequisites

- A host Wayland compositor reachable by the render server cang starts (the
  cross-domain path is what the guest proxy talks to). cang itself does not
  create one; the smoke harnesses start `weston --backend=headless-backend.so`.
- `/dev/udmabuf` openable from inside cang's keep-id VM-worker user namespace,
  i.e. mode `0666` as `/dev/kvm` is: the host-side probe fails on a
  `crw-rw---- root kvm` node because that namespace does not map the `kvm`
  group, and then no feature bit is advertised and this client silently measures
  the copy path.
