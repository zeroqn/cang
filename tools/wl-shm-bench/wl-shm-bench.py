#!/usr/bin/env python3
"""Commit wl_shm frames to a Wayland socket at a fixed rate, and report the cost.

This exists for the CREATE_GUEST_HANDLE A/B (wayfinder ticket 09): a guest
`wl-cross-domain-proxy` imports a `wl_shm` pool as a guest blob. When the host
advertises `VIRTIO_GPU_F_CREATE_GUEST_HANDLE` the proxy takes its udmabuf
zero-copy path and stops copying the pool; otherwise it copies the pool into a
sharable blob on every commit. Running this client under both launches is the
measured delta.

The pool memfd is created with `MFD_ALLOW_SEALING`, which the proxy's fast path
requires (it adds `F_SEAL_SHRINK` and refuses a memfd sealed against writing),
and the pool is page-aligned so the proxy does not have to grow it.

Usage (inside a cang guest, with the proxy serving or launching us):

    wl-shm-bench.py [--seconds 10] [--width 512] [--height 256] [--rate 120]

Environment: `WAYLAND_SOCKET` (an already-connected fd, what the proxy's
`-- PROGRAM` form sets) wins, otherwise `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`.
Output: `key=value` lines on stdout for a host-side reader.
"""

import argparse
import array
import mmap
import os
import select
import socket
import struct
import sys
import time

# --- Wayland wire protocol -------------------------------------------------

WL_DISPLAY_SYNC = 0
WL_DISPLAY_GET_REGISTRY = 1

WL_REGISTRY_BIND = 0
WL_REGISTRY_GLOBAL = 0

WL_COMPOSITOR_CREATE_SURFACE = 0
WL_SHM_CREATE_POOL = 0
WL_SHM_POOL_CREATE_BUFFER = 0
WL_SURFACE_ATTACH = 1
WL_SURFACE_DAMAGE = 2
WL_SURFACE_COMMIT = 6

WL_SHM_FORMAT_XRGB8888 = 1


def u32(value):
    return struct.pack("<I", value & 0xFFFFFFFF)


def i32(value):
    return struct.pack("<i", value)


def wl_string(value):
    raw = value.encode() + b"\0"
    return u32(len(raw)) + raw + b"\0" * ((-len(raw)) % 4)


def read_string(payload, offset):
    length = struct.unpack_from("<I", payload, offset)[0]
    value = payload[offset + 4 : offset + 4 + length - 1].decode(errors="replace")
    return value, offset + 4 + length + ((-length) % 4)


class Wayland:
    """One Wayland connection: id allocation, message framing, fd passing."""

    def __init__(self, sock):
        self.sock = sock
        self.next_id = 2  # 1 is wl_display
        self.buffer = b""
        self.received_fds = []

    def alloc(self):
        object_id = self.next_id
        self.next_id += 1
        return object_id

    def send(self, object_id, opcode, body=b"", fds=()):
        size = 8 + len(body)
        message = struct.pack("<II", object_id, (size << 16) | opcode) + body
        ancillary = []
        if fds:
            ancillary = [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))]
        self.sock.sendmsg([message], ancillary)

    def pump(self, timeout):
        """Read what has arrived within `timeout`; returns [(id, opcode, payload)]."""
        messages = []
        if timeout or not self.buffer:
            try:
                readable, _, _ = select.select([self.sock], [], [], timeout)
            except InterruptedError:
                return messages
            if not readable:
                return messages
        while True:
            data = b""
            try:
                data, ancillary, _flags, _addr = self.sock.recvmsg(
                    262144, socket.CMSG_SPACE(4 * 16)
                )
            except BlockingIOError:
                break
            if not data:
                raise ConnectionError("wayland socket closed")
            for level, kind, cdata in ancillary:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    fds = array.array("i")
                    fds.frombytes(cdata[: len(cdata) - (len(cdata) % fds.itemsize)])
                    self.received_fds.extend(fds)
            self.buffer += data
            if len(data) < 262144:
                break
        while len(self.buffer) >= 8:
            object_id, word = struct.unpack_from("<II", self.buffer)
            size = word >> 16
            opcode = word & 0xFFFF
            if size < 8 or len(self.buffer) < size:
                break
            messages.append((object_id, opcode, self.buffer[8:size]))
            self.buffer = self.buffer[size:]
        return messages

    def roundtrip(self):
        callback = self.alloc()
        self.send(1, WL_DISPLAY_SYNC, u32(callback))
        while True:
            for object_id, opcode, _payload in self.pump(2.0):
                if object_id == callback and opcode == 0:
                    return
            else:
                continue
            break


def connect():
    fd = os.environ.get("WAYLAND_SOCKET")
    if fd:
        sock = socket.socket(fileno=int(fd))
        sock.setblocking(False)
        return sock, "WAYLAND_SOCKET=%s" % fd
    runtime = os.environ.get("XDG_RUNTIME_DIR") or ""
    display = os.environ.get("WAYLAND_DISPLAY") or "wayland-0"
    path = display if display.startswith("/") else os.path.join(runtime, display)
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    sock.setblocking(False)
    return sock, path


def bind_globals(connection, wanted):
    """Bind every wanted global; returns {interface: (object_id, version)}."""
    registry = connection.alloc()
    connection.send(1, WL_DISPLAY_GET_REGISTRY, u32(registry))
    bound = {}
    deadline = time.monotonic() + 5.0
    while len(bound) < len(wanted) and time.monotonic() < deadline:
        for object_id, opcode, payload in connection.pump(0.5):
            if object_id != registry or opcode != WL_REGISTRY_GLOBAL:
                continue
            name = struct.unpack_from("<I", payload)[0]
            interface, offset = read_string(payload, 4)
            version = struct.unpack_from("<I", payload, offset)[0]
            if interface in wanted and interface not in bound:
                new_id = connection.alloc()
                connection.send(
                    registry,
                    WL_REGISTRY_BIND,
                    u32(name) + wl_string(interface) + u32(version) + u32(new_id),
                )
                bound[interface] = (new_id, version)
    return bound


def main():
    parser = argparse.ArgumentParser(description="wl_shm commit benchmark")
    parser.add_argument("--seconds", type=float, default=10.0)
    parser.add_argument("--width", type=int, default=512)
    parser.add_argument("--height", type=int, default=256)
    parser.add_argument(
        "--rate", type=float, default=120.0, help="target commits/s, 0 = unthrottled"
    )
    args = parser.parse_args()

    page = mmap.PAGESIZE
    stride = args.width * 4
    pool_size = ((stride * args.height + page - 1) // page) * page

    sock, where = connect()
    connection = Wayland(sock)
    bound = bind_globals(connection, ["wl_shm", "wl_compositor"])
    for interface in ("wl_shm", "wl_compositor"):
        if interface not in bound:
            print("error=no_%s_global" % interface, flush=True)
            return 1
    shm, shm_version = bound["wl_shm"]
    compositor, compositor_version = bound["wl_compositor"]

    pool_fd = os.memfd_create("wl-shm-bench", os.MFD_ALLOW_SEALING | os.MFD_CLOEXEC)
    os.ftruncate(pool_fd, pool_size)
    mapping = mmap.mmap(
        pool_fd, pool_size, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE
    )

    pool = connection.alloc()
    connection.send(shm, WL_SHM_CREATE_POOL, u32(pool) + i32(pool_size), fds=[pool_fd])
    buffer_id = connection.alloc()
    connection.send(
        pool,
        WL_SHM_POOL_CREATE_BUFFER,
        u32(buffer_id)
        + i32(0)
        + i32(args.width)
        + i32(args.height)
        + i32(stride)
        + u32(WL_SHM_FORMAT_XRGB8888),
    )
    surface = connection.alloc()
    connection.send(compositor, WL_COMPOSITOR_CREATE_SURFACE, u32(surface))

    def present(frame):
        # Frame-dependent content, so the pool is really written each commit.
        mapping[0:4] = struct.pack("<I", 0xFF000000 | (frame & 0xFFFFFF))
        connection.send(surface, WL_SURFACE_ATTACH, u32(buffer_id) + i32(0) + i32(0))
        connection.send(
            surface,
            WL_SURFACE_DAMAGE,
            i32(0) + i32(0) + i32(args.width) + i32(args.height),
        )
        connection.send(surface, WL_SURFACE_COMMIT)

    print("socket=%s" % where, flush=True)
    print(
        "shm_version=%d compositor_version=%d" % (shm_version, compositor_version),
        flush=True,
    )
    print(
        "width=%d height=%d stride=%d pool_bytes=%d"
        % (args.width, args.height, stride, pool_size),
        flush=True,
    )

    present(0)
    connection.roundtrip()

    start_cpu = os.times()
    start = time.monotonic()
    frames = 0
    interval = 1.0 / args.rate if args.rate > 0 else 0.0
    next_slot = start
    while time.monotonic() - start < args.seconds:
        present(frames)
        frames += 1
        connection.pump(0.0)
        if interval:
            next_slot += interval
            delay = next_slot - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            elif delay < -interval * 4:
                next_slot = time.monotonic()
    elapsed = time.monotonic() - start
    end_cpu = os.times()

    user = end_cpu.user - start_cpu.user
    system = end_cpu.system - start_cpu.system
    cpu = user + system
    print("frames=%d seconds=%.3f fps=%.2f" % (frames, elapsed, frames / elapsed), flush=True)
    print(
        "cpu_user_s=%.3f cpu_sys_s=%.3f cpu_s=%.3f cpu_pct=%.1f"
        % (user, system, cpu, 100.0 * cpu / elapsed),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
