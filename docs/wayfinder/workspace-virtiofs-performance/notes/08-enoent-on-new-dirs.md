# 08 - the guest VM dies with `virtio_fs_enqueue_req() failed -5`

Raw evidence collected 2026-10-25 on the harness side, while building the
baseline. This is the note ticket 08 resolves against.

## Symptom, as the harness sees it

`cang -- sh bench-inner.sh ...` returns **immediately after boot** (exit code 0,
no output at all), the guest command's work is never done, and a VM is left
behind as an orphan (`cang ps` still lists it as `running`; `cang kill <handle>`
clears it). Repeated launches into the same workspace then silently do nothing,
because a running task already owns the workspace.

A cold `cargo build` in `/workspace` sometimes dies with `ENOENT` on `target/`
directories that the build had just created - the same failure, surfacing
through cargo instead of through the VM.

## The cause, in the VMM's own log

`<state-root>/tasks/<task>/helper.stderr.log` ends with:

```
ERROR init_or_kernel] [    6.070734] virtio-fs: virtio_fs_enqueue_req() failed -5
ERROR init_or_kernel] [    6.071100] virtio-fs: virtio_fs_enqueue_req() failed -5
... (a burst of ~7 lines within 1 ms)
DEBUG krun_devices::virtio::mmio[console]] interrupt: signal_used_queue
DEBUG krun_devices::virtio::device] reset device while it's still in active state
DEBUG krun::vmm] using vmm exit code: 0
INFO  krun::vmm] Vmm is stopping.
```

So: the **guest kernel cannot enqueue FUSE requests** (`-5` = `-EIO` from
`virtio_fs_enqueue_req()`), the guest's virtio-fs connection goes to
`VIRTIO_FS_ABORTED`, the console device is reset mid-flight, and the VMM stops
with exit code **0** - which is why every layer above reports success and the
failure arrives as an empty log or a confusing `ENOENT` inside cargo.

The burst arrives ~6 s after boot, i.e. exactly when the guest's early userspace
and the first `cargo` metadata walk open hundreds of files at once on the single
request queue.

## Questions for the ticket

1. Which side is `-EIO` from - the guest driver refusing (`virtqueue_add_sgs`
   failure, queue full, a wedged queue after an earlier error) or the device
   having torn a queue down under the guest? The device half is
   `deps/libkrun/src/devices/src/virtio/fs/` (`device.rs`, `worker.rs`,
   `queue.rs`); the guest half is `fs/fuse/virtio_fs.c` in the pinned kernel.
2. Is it the **single request queue** (`NUM_QUEUES = 2`, one `FsWorker`,
   `num_request_queues = 1`, no `VIRTIO_F_MULTIQUEUE`) being saturated: does the
   device stop publishing used-ring entries while `FsWorker` is busy in a slow
   syscall, so the guest's `num_free` never recovers and the driver errors out?
3. Does the fork's device handle a **queue-full / malformed descriptor** case by
   silently dropping the queue (crosvm heritage: `iter()` returning an error
   kills the worker thread, and the guest then sees EIO forever)?
4. Why does the VMM exit **0** on this path, so cang reports success? That alone
   is a separate defect: a dead guest VM must not look like a clean exit.

## What "fixed" means

A cold `cargo build` of the benchmark tree in `/workspace` completes, three runs
in a row, with no `virtio_fs_enqueue_req` line in the task's `helper.stderr.log`;
`cang -- false` and a killed guest command report a non-zero status; and a
`cang` launch into a workspace that already has a running task fails loudly
instead of doing nothing.
