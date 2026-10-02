# Graphics and audio

Display and audio passthrough for cang tasks. Host prerequisites for
`--gpu=drm`, `--wayland`, and `--waypipe` are listed in the
[README](../README.md#prerequisites).

## Pulse TCP audio

```ini
# ~/.config/pipewire/pipewire-pulse.conf.d/cang-tcp.conf
pulse.properties = {
    server.address = [
        "unix:native"
        "tcp:127.0.0.1:4713"
    ]
}
```

Restart the host `pipewire-pulse` user service after adding the drop-in. Use
`--pulse=tcp:localhost:PORT` or `--pulse=tcp:127.0.0.1:PORT` to create a
private per-task bridge to the selected host IPv4-loopback listener. The guest
receives `PULSE_SERVER=unix:/run/user/<uid>/cang-pulse`; each guest Pulse
connection crosses a dedicated libkrun vsock channel and connects only to the
configured host `127.0.0.1:PORT`. Cang does not enable passt host-loopback
mapping or expose other host-loopback ports. Task launch succeeds if the host
listener is unavailable, and a later guest connection can succeed after the
host service starts or restarts.

Other literal IPv4 and bracketed IPv6 endpoints remain direct guest TCP
endpoints and are exported canonically as `PULSE_SERVER=tcp:IP:PORT`. Cang
does not start host or guest `pipewire`/`pipewire-pulse`, proxy native PipeWire
`pipewire-0`, forward a Pulse cookie, or test the connection before launch.
`cang exec` inherits the endpoint selected when the task was launched and
cannot change it.

Host `pipewire-pulse` configuration owns authorization. A permitted Pulse
client may gain playback, capture, stream inspection, or server-control access,
so configure the listener's access policy for the trust granted to the task.
For combined software-only Waypipe playback, mpv 0.41.0 also needs
`--gpu-sw=yes`, for example:

```bash
./result/bin/cang \
  --waypipe=/tmp/cang-waypipe.sock \
  --pulse=tcp:localhost:4713 \
  -- mpv --gpu-sw=yes '/workspace/'*.mp4
```

## Remote Waypipe

```bash
# Workstation: connect Waypipe to the local compositor.
waypipe --socket "$XDG_RUNTIME_DIR/cang-waypipe.sock" client

# Workstation: keep an authenticated reverse Unix-socket forward open.
ssh -R /tmp/cang-waypipe.sock:"$XDG_RUNTIME_DIR/cang-waypipe.sock" cang-host

# cang host: launch a Waypipe-capable task with the initial target.
./result/bin/cang \
  --workspace=/home/dev/foo \
  --waypipe=/tmp/cang-waypipe.sock \
  -- gui-application

# Reuse the running Waypipe server for another GUI command.
./result/bin/cang --waypipe exec TASK -- another-gui-application

# Replace the target, restart the guest Waypipe server, then run a command.
./result/bin/cang --waypipe=/tmp/other-waypipe.sock exec TASK -- gui-application
```

cang validates that the selected workspace is an absolute directory and each
valued socket path is absolute and already exists as a Unix socket. The guest
command is optional; when omitted, cang starts the normal interactive fish
login shell. Valueless `--waypipe` launches the task capability without an
active target. A valueless Waypipe exec reuses the running server and display.
A valued Waypipe exec serially changes the target, terminates and reaps the
running server, starts a fresh server on the stable display name, waits for
readiness, and only then starts the command. This is replacement, not protocol
reconnection: existing GUI applications connected to the old server lose their
Wayland connection and normally exit. If `--workspace` is omitted, cang uses
the current working directory. cang does not start SSH or the workstation
Waypipe client and does not create, unlink, or clean up the forwarded socket.
Without `--gpu=drm`, the mode passes `--no-gpu` to Waypipe. The cang guest
image provides Mesa software rendering for this path: OpenGL/EGL applications
use llvmpipe and Vulkan applications use lavapipe on the guest CPU. When
`--gpu=drm` is also selected, guest-init omits Waypipe's `--no-gpu`, does not
force the software-renderer environment, and preserves the DRM-scoped Mesa
OpenGL/EGL and Vulkan discovery paths, including `GBM_BACKENDS_PATH`
(`/usr/lib/cang-mesa-runtime/lib/gbm`). The frame transfer is then dma-buf
rather than shared memory: a client that allocates GBM buffers presents them
without a copy, and `ffmpeg`/`mpv` played through the display reach the guest's
hardware VA-API path. For a media client, `mpv --hwdec=vaapi --vo=gpu
--gpu-api=opengl` reports `Using hardware decoding (vaapi)` and a
`vaapi[yuv420p]` video output; the GL video output is required because mpv's
default Vulkan output (and `--use-angle=vulkan` in Chromium) hits venus dma-buf
format-modifier failures. `--waypipe` remains mutually exclusive with
`--wayland`.
