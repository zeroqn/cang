---
label: wayfinder:research
title: Where `virtio_fs_enqueue_req() failed -5` comes from, and why it does not kill the guest
status: open
blocked_by: []
claimed_by: pi + diagnosis child (2026-10-25)
---

# 08 - `virtio_fs_enqueue_req() failed -5` is the guest's own shutdown race

Ticket: `tickets/08-virtiofs-enoent-on-new-dirs.md`. Supersedes the reading in
[08-enoent-on-new-dirs.md](08-enoent-on-new-dirs.md), which treated this burst as
the cause of the guest dying.

## Verdict

**The `-5` is `-EIO` returned by the *guest's* `virtqueue_add_sgs()` because the
*guest* has just marked its own virtqueues broken — `drivers/virtio/virtio.c`'s
generic `virtio_device_shutdown()`, which the kernel runs for every virtio device
on poweroff/reboot, calls `virtio_break_device(dev)` and then resets the device.
Any FUSE request the guest submits in that window (a final write, a `sync`, the
last lookup of a payload that is already exiting) is refused, and
`virtio_fs_send_req()` prints it.**

The fork's virtio-fs device is not involved in the failure: it never sees those
requests. The burst is **shutdown noise, not a fault**, and it is *present in
runs that complete their workload* — measured: 3 of 5 identical probe runs show
it (5, 5 and 3 lines) and **all 5 wrote their output file**, while 6 benchmark
arms show no burst at all and also pass.

So the map's premise ("a virtio-fs correctness bug kills the guest; fixing it is
a prerequisite of the measurement") does **not** hold for this log line. What the
harness actually hit is separate and is listed under "What is still open".

## 1. Which side produces `-EIO`, with file:line

Guest kernel, `fs/fuse/virtio_fs.c` (v7.2.10, the pinned libkrunfw kernel):

| line | what |
|---|---|
| `1395` | `static int virtio_fs_enqueue_req(struct virtio_fs_vq *fsvq, struct fuse_req *req, bool in_flight, gfp_t gfp)` |
| `1455-1459` | `if (!fsvq->connected) { ... ret = -ENOTCONN; goto out; }` - a *disconnected* queue gives **-107**, not -5 |
| `1462` | `ret = virtqueue_add_sgs(vq, sgs, out_sgs, in_sgs, req, GFP_ATOMIC);` |
| `1410-1432` | `sg_count_fuse_req()` over-count is **-E2BIG** (`:1413-1416`), `copy_args_to_argbuf()` failure is **-ENOMEM** (`:1427-1428`) |
| `1500-1544` | `virtio_fs_send_req()`: `-ENOSPC` is handled (queue it and retry from the dispatch worker, `:1523-1532`); anything else becomes `req->out.h.error = ret` and the `pr_err("virtio-fs: virtio_fs_enqueue_req() failed %d\n", ret)` at **`:1535`** - the line we see |
| `595-615` | the same message for background requests, from the dispatch worker |

Transport, `drivers/virtio/virtio_ring.c` (v7.2.10): `virtqueue_add_sgs()` →
`virtqueue_add_split()`, which returns **`-EIO` in exactly one place**:

```c
 674	if (unlikely(vq->broken)) {
 675		END_USE(vq);
 676		return -EIO;
 677	}
```

(`-ENOSPC` is the separate queue-full return at `:718`.) `vq->broken` has only
three non-debug setters: `BAD_RING` in `virtqueue_get_buf_ctx_split()`
(`:994` "id %u out of range", `:998` "id %u is not a head"), a failed
`vq->notify()` in `virtqueue_notify()` (`:3106-3112`), and
`__virtqueue_break()`/`virtio_break_device()` (`:3629-3675`).

Who breaks the ring here - `drivers/virtio/virtio.c`:

```c
 412	void virtio_device_shutdown(struct virtio_device *dev)
 413	{
 414		/*
 415		 * Some devices get wedged if you kick them after they are
 416		 * reset. Mark all vqs as broken to make sure we don't.
 417		 */
 418		virtio_break_device(dev);
 ...
 426		dev->config->reset(dev);
 427	}

 431	static void virtio_dev_shutdown(struct device *_d)
 ...
 443		/* If the driver has its own shutdown method, use that. */
 444		if (drv->shutdown) {
 445			drv->shutdown(dev);
 446			return;
 447		}
 448
 449		virtio_device_shutdown(dev);
```

and the virtio **bus** registers it for every virtio device (`:468`
`.shutdown = virtio_dev_shutdown`). `virtio_fs` has no `.shutdown` of its own,
so on `kernel_power_off()`/`kernel_restart()` the generic path breaks all of the
device's virtqueues and then writes DRIVER-OK-less status 0.

The key asymmetry: `virtio_fs`'s `fsvq->connected` is only cleared by
`virtio_fs_stop_all_queues()`, which is reached from `virtio_fs_remove()` (driver
unbind, `:1213`) and `virtio_fs_conn_destroy()` (umount, `:1654`) - **not** from
the shutdown path. So during `device_shutdown()` the FUSE connection is still
"connected", `virtio_fs_send_req()` passes its `connected` check and lands in
`virtqueue_add_sgs()` with `vq->broken == true` - `-EIO`, `pr_err`, done.

That it is *this* path and not something the fork did is visible in the device's
own log, in every burst we captured: libkrun logs the guest's status-0 write and
the worker teardown **at the same instant** as the burst. Live capture, run 1
(`<state root>/tasks/<task>/helper.stderr.log`):

```
<fs opcodes: lookup/do_open>                       <- payload's last syscall
... DEBUG krun_devices::virtio::device] reset device while it's still in active state
... DEBUG krun_devices::virtio::fs::worker] stopping worker thread
... ERROR init_or_kernel] [    5.859784] virtio-fs: virtio_fs_enqueue_req() failed -5
... ERROR init_or_kernel] [    5.860091] virtio-fs: virtio_fs_enqueue_req() failed -5
... ERROR init_or_kernel] [    5.860281] virtio-fs: virtio_fs_enqueue_req() failed -5
... ERROR init_or_kernel] [    5.860528] virtio-fs: virtio_fs_enqueue_req() failed -5
... ERROR init_or_kernel] [    5.860709] virtio-fs: virtio_fs_enqueue_req() failed -5
```

("reset device while it's still in active state" is
`deps/libkrun/src/devices/src/virtio/device.rs:261`, the status-0 arm of
`set_device_status()`; "stopping worker thread" is
`.../virtio/fs/worker.rs`'s stop-fd handler, i.e. `Fs::reset()` at
`.../virtio/fs/device.rs:255`.) The guest's own `virtio_device_shutdown()` does
exactly these two steps, in this order: break the vqs, then reset the device.

### Two mechanisms that are *not* it (checked, so nobody re-derives them)

- **`virtio_reset_device()` is not the breaker.** It only calls
  `virtio_break_device()` under `CONFIG_VIRTIO_HARDEN_NOTIFICATION`
  (`drivers/virtio/virtio.c:254-266`), and the pinned kernel config has no such
  option (`deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto` contains no
  `VIRTIO_HARDEN_NOTIFICATION`). Independently, `virtio_mmio.c` has no
  `__virtqueue_unbreak()` call site at all, so with that option on, the MMIO
  devices cang uses (`virtio_mmio`, see below) could never have worked.
- **`notify()` cannot fail.** `virtqueue_notify()` breaks the ring when
  `vq->notify()` returns false, but `vp_notify()`/`vp_notify_with_data()`
  unconditionally `return true` (`virtio_pci_common.c:52-57`,
  `virtio_pci_modern.c:677-683`).
- **No `BAD_RING` line was ever printed.** Every captured log was grepped for
  `ERROR`/`panic`/`is not a head`/`out of range`: the burst runs contain only the
  `-5` lines (and the pre-existing vsock `unexpected dgram pkt: 3` warning), so
  the used ring was never malformed by the device.
- **The device's reset path is correct.** `VirtioTransportState::reset()`
  (`deps/libkrun/src/devices/src/virtio/device.rs:194`) *recreates* the `Queue`s,
  so `next_avail`/`next_used`/`num_added` cannot leak into a re-activated
  device; `PciTransport::reset_queue_registers()` (`.../virtio/pci.rs:754`) is
  belt-and-braces for the PCI variant only. There is no stale-ring-index bug to
  fix here.

## 2. Why a burst at ~6 s, with one request queue and one `FsWorker`

Because ~6 s is when cang's short-lived guests *exit*, not when the filesystem
gets busy. The burst is the tail of `device_shutdown()`: the payload has already
run (`z.txt` exists in all five probe runs), the guest powers off, and the
handful of FUSE requests that were in flight at that instant are refused.

The queue shape explains the *shape* of the burst, not its cause: with
`NUM_QUEUES = 2`, `num_request_queues = 1` and a single `FsWorker` thread
(`deps/libkrun/src/devices/src/virtio/fs/worker.rs:240`, one
`server.handle_message()` per descriptor chain, inline), the guest's requests are
completed strictly one at a time, so whatever is outstanding at reset time fails
in one tight run - measured 3-5 lines inside 0.3-1.0 ms, matching the original
"~7 lines in 1 ms". The used-ring/notification logic is not involved: the device
publishes used entries in `process_queue` (`queue.add_used` then
`needs_notification` → `interrupt.signal_used_queue`), and in the burst runs the
last published completion is 0.1-0.2 ms *before* the reset.

Bursts therefore appear when the guest still has requests in flight when it
powers off (a payload that just wrote a file), and are absent when it does not.
Both teardowns are identical otherwise: in a burst-free run the log shows the
same four device resets, the block worker stop, the fs reset + "stopping worker
thread", the console reset and `using vmm exit code: 0` - just without the `-5`
lines.

## 3. Does the fork kill the worker thread (crosvm heritage)? Yes - fixed here

`deps/libkrun/src/devices/src/virtio/fs/worker.rs` did:

```rust
            let reader = Reader::new(&self.mem, head.clone())
                .map_err(FsError::QueueReader)
                .unwrap();                                  // worker.rs:237-239
            let writer = Writer::new(&self.mem, head.clone())
                .map_err(FsError::QueueWriter)
                .unwrap();                                  // worker.rs:240-242
```
and in `handle_event()` both `disable_notification(..).unwrap()` and
`enable_notification(..).unwrap()` (`worker.rs:220-231`). The descriptor chain
comes from the guest (`Queue::pop()` → `DescriptorChain::checked_new`), so a
malformed or out-of-bounds chain is *guest input*; `unwrap()` panics the single
`fs worker` thread, and since that thread is the only thing serving the share -
in cang, the guest's **root** filesystem - every later FUSE request is never
completed and the guest wedges (a hang, not `-EIO`). `handle_message()`'s own
errors were already handled, so this was the one remaining panic path per
request.

**Fix** (branch `wayfinder/fs-worker-robustness` in the `deps/libkrun`
submodule, commit `f3167a42`, on top of the pinned `79228a72`):

- a chain that cannot be turned into a `Reader`/`Writer` now fails *that*
  request: log it and publish a zero-length used entry (the guest reports a fuse
  protocol error for that request and moves on) instead of panicking;
- a guest-memory error while (un)masking notifications logs and stops serving
  that queue instead of aborting the thread;
- `needs_notification()` failure is treated as "signal the guest" rather than a
  panic.

Diff: one file, `src/devices/src/virtio/fs/worker.rs`, 52 insertions / 30
deletions. It does not change the well-formed-request path.

This is a *latent-defect* fix, not the source of the `-5` burst: it removes the
only way a guest-visible protocol error could take the whole share down
permanently. Say so plainly in review - the burst would happen with or without
it.

## 4. Why the VMM exits 0 (a dead guest looks like a clean exit)

`deps/libkrun/src/libkrun/src/vmm/mod.rs:502-525`:

```rust
 509		let vcpu_exit_code = self.vcpus_handles.iter().find_map(...).unwrap_or(FC_EXIT_CODE_OK);
 517		let vmm_exit_code = self.exit_code.load(Ordering::SeqCst);
 518		let exit_code = if vmm_exit_code != i32::MAX { vmm_exit_code } else { vcpu_exit_code as i32 };
 519		debug!("using vmm exit code: {vmm_exit_code}");
 525		self.stop(exit_code);
```

`self.exit_code` is an `Arc<AtomicI32>` initialised to `i32::MAX`
(`.../vmm/builder.rs:1356`) and its **only** writer in the whole fork is the FUSE
ioctl handler `VIRTIO_IOC_EXIT_CODE_REQ = 0x7602`
(`.../virtio/fs/augment_fs.rs:751-757`), which no cang code sends (`grep -rn
7602 crates/` is empty) - so `exit_code` stays `i32::MAX` and the vCPU branch is
taken. A guest that powers off through the i8042 (`driver_shutdown`/`machine_restart`)
reports **no** vCPU exit code, and `unwrap_or(FC_EXIT_CODE_OK)` (`:516`) turns
that into **0**. Hence a guest that dies (or is reset by its own dead root
filesystem) is indistinguishable from one that finished: `Vmm is stopping` +
exit 0 + cang reporting success. That is the defect that made this log tail look
like "the VM dies silently"; it is worth its own ticket (cang must either make
the guest report a status through `0x7602`, or treat "guest powered off without
a reported status" as a failure).

## 5. What would actually silence the `-5` lines

The only complete fix is guest-side: give `virtio_fs` a `.shutdown` op that
quiesces first, e.g.

```c
static void virtio_fs_shutdown(struct virtio_device *vdev)
{
	virtio_fs_stop_all_queues(vdev->priv);   /* fsvq->connected = false */
	virtio_device_shutdown(vdev);            /* break + reset          */
}
```

which is exactly what `virtio_device_shutdown()`'s own doc comment asks drivers
to do ("Drivers with their own .shutdown method should quiesce their activity and
then call this"). That is a `libkrunfw` kernel patch (the same race exists under
virtiofsd/qemu, so it is upstreamable), it does not belong in this fork, and it
buys nothing but a quieter log: the requests it would refuse are already doomed
by the shutdown. Recorded here so the option is not rediscovered as a "device
bug".

## 6. Live evidence (2026-10-25/26, worktree `/home/dev/cang/cang-vfio`)

Every arm ran bounded, one at a time, with a per-arm log, and `cang kill` after.

**(a) The benchmark arm does not hit it.** Six
`tools/rust-compile-bench/run.sh --arms guest-virtiofs` runs
(`LEAVES=60 MODULES=3 LINES=10`, `BENCH_ROOT=/home/dev/cang-bench-diag`) all
completed: `build 19.6 s / 21.0 s / 20.9 s / ...`, `BENCH_MS` written by the
guest, and each task's `helper.stderr.log` was 3 lines (vsock warnings only) -
`grep -c enqueue_req` = 0.

**(b) The trivial probe hits it, and still succeeds.** Five runs of
`cang --preserve-debug --log-level debug --image ghcr.io/zeroqn/cang:latest --
sh -c 'echo hi > /workspace/z.txt'` in a fresh empty workspace:

| run | `enqueue_req` lines | `/workspace/z.txt` | guest lifetime |
|---|---|---|---|
| 1 | 5 | present | 8 s |
| 2 | 5 | present | 8 s |
| 3 | 0 | present | 8 s |
| 4 | 0 | present | 8 s |
| 5 | 3 | present | 8 s |

i.e. **the burst is orthogonal to whether the guest's work completes**. The
`-EIO` lines carry guest uptimes 5.687-5.860 s and always follow the
"reset device while it's still in active state" + "stopping worker thread" pair
by 0.1-1 ms.

**(c) A burst-free teardown, for contrast** (probe run 3): four
`reset device while it's still in active state` lines, `block::worker stopping
worker thread`, then `fs::worker stopping worker thread`, then
`using vmm exit code: 0` / `Vmm is stopping` - the same shutdown, no `-EIO`,
because nothing was in flight.

**(d) Corrected fact for the map:** cang attaches **all** of its virtio devices
through the MMIO manager (`crates/cang/src/runtime/vm/libkrun/launcher.rs:221-222`,
`:657-658`), and the device logs confirm it (`krun_devices::virtio::mmio[fs]`).
Note 01's "the fs device is PCI (`pci.rs:688`)" is wrong; the PCI-only code
paths (`reset_queue_registers`, the bus-master gate, MSI-X) never run for cang's
share.

## 7. Fix verification (live, patched device)

Built with the patched fork into a separate output:

```bash
nix build .#cang --override-input libkrun-src "git+file://$PWD/deps/libkrun" -o result-fix
# NIX_RC=0; `krun-devices` recompiled from the patched tree
# artifact attribution: grep -ac 'unreadable descriptor chain' result-fix/bin/cang -> 1
#                       (the pre-patch result/bin/cang -> 0)
```

The brief's arm, one bounded run, per-arm log, killed afterwards:

```bash
BENCH_ROOT=/home/dev/cang-bench-diag CANG=/home/dev/cang/cang-vfio/result-fix/bin/cang \
  LEAVES=60 MODULES=3 LINES=10 RUNS=1 tools/rust-compile-bench/run.sh --arms guest-virtiofs
```

```
guest-virtiofs run 1  build 19.3s  outer 28.3s
BENCH_MS=19299   RUSTC=rustc 1.95.0   CARGO=cargo 1.95.0
ARM_RC=0
tasks/<id>/helper.stderr.log: 3 lines, grep -c enqueue_req = 0
cang ps: no leftover task
```

So the patched worker serves a full cold 81-crate build and the guest still
reports through its result file. And, as predicted, the patch does **not**
change the `-5` burst - two more probes with the patched binary gave
`enqueue_req=0` in one and `enqueue_req=6` in the other, both with `z.txt`
written, exactly the pre-patch distribution. The burst is guest-shutdown noise
either way; the fix is only about not letting a guest protocol error kill the
worker thread.

The compile gate from the brief, in the devshell, in the fork's own workspace
(the libkrun crate is package `libkrun`, lib target `krun` - `-p krun` is not a
package name there):

```bash
cd deps/libkrun && nix develop /home/dev/cang/cang-vfio --command \
  cargo build -p krun-devices -p libkrun
#    Finished `dev` profile [unoptimized + debuginfo] target(s) in 30.52s
#    CARGO_RC=0
#    one pre-existing warning (unused `UdmabufDriver` import in libkrun), not from this change
```

## What did not fix it / what is not the cause

- Making the queue bigger, adding a worker pool, or enabling multiqueue: the
  failure is on the *guest* side, before the device is reached; nothing in
  `queue.rs`/`worker.rs` can prevent it.
- Any libkrunfw/guest-cache change: unrelated.
- The 08 note's earlier hypothesis "the single queue is saturated so `num_free`
  never recovers": a full ring returns `-ENOSPC` (`virtio_ring.c:718`), which
  `virtio_fs_send_req()` handles by deferring (`virtio_fs.c:1523-1532`) and
  never logs; it cannot produce `-5`.
- The worker-thread panic path: real, but it wedges the share (a hang) rather
  than producing `-EIO`; fixed for robustness, see §3.

## What is still open (for the next ticket, not this one)

1. The ticket's actual blocker was never virtio-fs: the harness's symptom is
   **`cang` returning exit 0 ~1 s after boot while the guest keeps working for
   another ~20 s**, so a bench arm's "result" looks lost unless the harness waits
   for a result *file*. That is cang-side (session/CLI lifecycle + §4), and it
   also explains "returns immediately after boot with no output at all".
2. "ENOENT on `target/` directories cargo just created" needs its own diagnosis
   with the guest-side repro the ticket asked for (mkdir + `openat` loops,
   parallel vs single-threaded). Nothing in this investigation reproduces it, and
   the six benchmark arms built cleanly, so it may have been an artefact of the
   earlier session's overlapping VMs.
3. Because of §1, the ticket's "fixed" criterion "no `virtio_fs_enqueue_req`
   line in `helper.stderr.log`" should be replaced by "the guest's result file
   appears" - the log line is a normal guest-exit artefact and grepping for it
   will otherwise keep producing false blockers.
