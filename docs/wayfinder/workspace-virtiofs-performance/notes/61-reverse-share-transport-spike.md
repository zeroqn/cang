# 61 - Reverse-share transport spike: what one vsock round trip costs, live

**Measured 2026-11-20** on this host (kernel `7.3.4-xanmod1`, 28 cores, `nproc=28`,
`kvm.halt_poll_ns=200000`), libkrun from a copy of `deps/libkrun` at
**`v2.0.0-cang.22` (`2fad450a`)** built here, firmware
`libkrunfw-v5.6.2-cang.6` (its `Makefile`'s `KERNEL_VERSION = linux-7.3.5`,
`KERNEL_HARDENED_VERSION = v7.3.5-hardened1`; the guest's own `uname` was not
read).
One VM at a time, no other VM on the host (`cang ps` empty before every boot).
Raw evidence: `/home/dev/cang/cang/.scratch/vsock-spike/results/` (`f2/`, `f8/`,
`attrib2/`, `attrib8/`, `loopback/`, `diag/`), each with `numbers.txt`,
`console.log`, `harness.log` (sha256 of every artifact), and the per-thread CPU
dumps.

Ticket: [22-reverse-share-transport](../tickets/22-reverse-share-transport.md).
This note is the spike note 58 §6 step 1 asked for: **transport only, no
filesystem, no mount, no cang launch, no fork change.**

## Verdict

**A single relayed operation on cang's own transport costs a 143 us median
(150 us mean) round trip, so the 250 us gate passes for one stream — but the
transport saturates at ~15.5 k ops/s aggregate across 8 concurrent streams
(483 us per op at that concurrency), its p99 is 273 us, and an 8-vCPU VM is
3x worse than a 2-vCPU one.** Note 58's borrowed 224 us floor and its
"concurrency is the only lever" both fail against the measurement: cang's
userspace muxer is *faster* per single round trip than the vhost-vsock number
it borrowed, and concurrency buys 2.3x, not 8x.

## 1. What was built, and where

All of it untracked, under `/home/dev/cang/cang/.scratch/vsock-spike/`:

| file | what it is |
|---|---|
| `libkrun-src/` | a byte copy of `deps/libkrun` at `2fad450a` (`v2.0.0-cang.22`) minus `.git`/`target`, built here — `deps/libkrun` itself was **not** touched |
| `src/launcher.c` | host driver: `krun_vmm_builder_*` + `krun_mmio_device_manager_new` + exactly three devices — virtio-console (guest stdout to the launcher's), a virtio-fs root at tag `/dev/root`, and `krun_vsock_device_new(3, 0)` with `krun_vsock_device_add_unix_port(vsock, port, sockpath, true)`; call order copied from `deps/libkrun/examples/chroot_vm.c` (the ABI-2 shape; the old `krun_set_root`/`krun_start_enter` API the `tools/virgl-guest-probe` launcher uses no longer exists in `include/libkrun.h`) |
| `src/guest-echo.c` | in-guest agent: `AF_VSOCK` `SOCK_STREAM` listener on `VMADDR_CID_ANY:<port>` (`socket`/`bind`/`listen`, thread per connection, one `read` + one `write` per request), plus an optional forked child that measures the *guest-local* vsock loopback (`VMADDR_CID_LOCAL:<port+1>`) and prints `RESULT GUEST_LOOPBACK_VSOCK ...` |
| `src/host-client.c` | host client: connects to a unix socket path, `write(size)` then `read(size)` per op, `CLOCK_MONOTONIC` around the pair, per-op samples → min/median/mean/p99/max + ops/s; `--clients N` for concurrency, `--warmup W` |
| `src/unix-echo.c` | the control arm: identical echo shape on a plain host unix socket (thread per connection) |
| `rootfs/bin/guest-echo` | the guest rootfs = one static musl binary, nothing else |
| `final.sh`, `campaign.sh`, `attrib.sh` | the campaigns (below) |
| `results/` | every raw log and number quoted here |

Artifacts actually measured (sha256 in each `results/*/harness.log`):

| artifact | path | sha256 |
|---|---|---|
| libkrun | `.scratch/vsock-spike/libkrun-src/target/release/libkrun.so` (soname `libkrun.so.2`) | `29f533f9c9ec615654af5e4d07411eb9fb1dc5aebfc181154134af9425c166aa` |
| libkrun init blob | `.scratch/vsock-spike/libkrun-src/target/release/libkrun_init.so` | `9b9e978876caae854f1d7b85a61c55018dd0421e279d213ff00feda7fbb7fbe8` |
| firmware | `/nix/store/n1nj9ajb91rj9nkzvpwa4g2ppgy23x35-libkrunfw-v5.6.2-cang.6/lib/libkrunfw.so.5.6.2` | `d7c00ad3b23a4b8237a5125579463895010635518ada8dddabfa7669c9da12ca` |
| guest agent | `/nix/store/ph8x9rl8n25iifyzkjxjgrka4xm1avs0-guest-echo-static-x86_64-unknown-linux-musl/bin/guest-echo` (copied to `rootfs/bin/`) | `dfe2259e2ac9aec8df0d87ceb85303b4eee23abab503fdf7a43027561e5d6cca` |
| launcher / client | `.scratch/vsock-spike/bin/{launcher,host-client}` (plain gcc builds, not store paths) | `e184058799b1b2549e2be50083393b5bfa1a4b1ba45c04ca3f1ee81ff97c952b` / `e2b478d4167ab1f11f0000bdc0784adf210cdb1c41bcff0919719239e5d40a9d` |

Commands, in order:

```bash
# 1. libkrun + init blob from a copy of the fork (deps/libkrun untouched)
cd /home/dev/cang/cang/deps/libkrun
tar -c --exclude=./.git --exclude=./target . | tar -x -C ../../.scratch/vsock-spike/libkrun-src
nix develop /home/dev/cang/cang --command bash -c 'cd /home/dev/cang/cang/.scratch/vsock-spike/libkrun-src \
  && export CARGO_TARGET_DIR=$PWD/target \
  && cargo build --release -p libkrun --features ffi \
  && cargo build --release -p krun-init-blob --features ffi'
# (the fork Makefile's `make all` additionally builds the workspace's
#  krun-display/krun-input members, which need libclang; -p libkrun skips them)

# 2. the guest agent, static
nix build --impure --no-link --print-out-paths -f .scratch/vsock-spike/guest-static.nix   # pkgsStatic, musl

# 3. host programs
gcc -O2 -pthread -o bin/unix-echo src/unix-echo.c
gcc -O2 -pthread -o bin/host-client src/host-client.c
gcc -O2 -Ilibkrun-src/include -Llibkrun-src/target/release -lkrun -lkrun_init \
    -Wl,-rpath,$PWD/libkrun-src/target/release -o bin/launcher src/launcher.c

# 4. one boot per campaign; the guest is up when the acceptor socket appears
export LD_LIBRARY_PATH=$PWD/lib:$PWD/libkrun-src/target/release:/nix/store/n1nj9ajb91rj9nkzvpwa4g2ppgy23x35-libkrunfw-v5.6.2-cang.6/lib
./bin/launcher rootfs results/f2/vsock.sock 50430 2 2000 64 &    # <rootfs> <sock> <guest-port> <vcpus> [loopback iters size]
./bin/host-client --path results/f2/vsock.sock --iters 5000 --size 64 --clients 1 --label VSOCK_64B_1C
./bin/unix-echo results/f2/ctl.sock &                            # control arm, same client
bash final.sh f2 2       # full vcpus=2 campaign   (results/f2/, results/f2.out)
bash final.sh f8 8       # full vcpus=8 campaign   (results/f8/, results/f8.out)
bash attrib.sh 2         # per-thread host CPU     (results/attrib2/)
bash attrib.sh 8         #                        (results/attrib8/)
```

Side finding: **`tools/virgl-guest-probe/` no longer builds against the pinned
libkrun.** Its `launcher.c` uses `krun_create_ctx`/`krun_set_root`/`krun_set_exec`/
`krun_start_enter`, none of which exists in this fork's `include/libkrun.h`
(ABI 2 exports only the `krun_vmm_builder_*` / `krun_*_device_*` shape), and its
`flake.nix` asks for `repo.packages.${system}.libkrun`, which the repo flake does
not provide (`nix eval` → "does not provide attribute 'packages.x86_64-linux.libkrun'
… Did you mean libkrunfw?"). I therefore took the *shape* from the probe and the
*calls* from `examples/chroot_vm.c`. The probe's `run-rs.sh` regression (memory
#331) is presumably still reproducible via another route, but not via this one.

## 2. The direction, verified empirically

`listen = true` **is** the reverse-share direction, as note 58 §1.3 read it: the
guest only ever called `listen()`/`accept()` on `AF_VSOCK`, and the host only
ever `connect()`ed to the unix path libkrun bound. Every byte in every arm below
crossed host→guest as a host-initiated connection that libkrun's muxer accepted
and carried to a guest listener (`unix_proxy/unix.rs:435-495`,
`new_acceptor_proxy`/`process_acceptor_event`, `muxer_thread.rs`), i.e. exactly the shape
`cang exec` uses (`launcher.rs:838`, `listen = true`). No `AF_VSOCK` socket was
ever created on the host and the guest never dialled the host. Nothing in note 58
to correct here.

## 3. The numbers

One operation = host client `write(size)` → guest echo `read` + `write` →
host client `read(size)`, i.e. one request/response with one relayed frame each
way. `median_us` is the median over all ops of that arm; the `n`-rep columns are
medians across reps of the same arm inside one boot.

**vcpus=2, 1024 MiB guest** (`results/f2/`), the primary arm set:

| arm | n | median us | median range across reps | mean us | p99 us | ops/s |
|---|---|---|---|---|---|---|
| **vsock 64 B, 1 client** | 10 | **143.4** | 131.1–178.1 | 150.3 | 272.9 | **6 648** |
| vsock 64 B, 8 clients (per op) | 3 | 482.9 | 479.8–496.2 | 513.8 | 1 177.5 | 15 492 (aggregate) |
| vsock 1 KiB, 1 client | 3 | 168.5 | 168.0–179.3 | 170.0 | 275.1 | 5 876 |
| vsock 4 KiB, 1 client | 3 | 148.2 | 141.9–153.4 | 156.6 | 270.0 | 6 379 |
| vsock 64 KiB, 1 client | 3 | 339.2 | 310.9–494.6 | 438.7 | 1 537.8 | 2 278 |
| vsock 1 op on a **fresh** connection | 10 | 521.2 | 365.9–692.8 | 521.2 | — | — |
| vsock 64 B, 1 client, **end of boot** | 3 | 137.3 | 134.8–173.9 | 140.3 | 257.1 | 7 119 |
| *control* unix 64 B, 1 client | 10 | 17.3 | 15.9–18.7 | 17.7 | 28.1 | 56 154 |
| *control* unix 64 B, 8 clients | 3 | 23.3 | 22.5–23.9 | 27.1 | 90.8 | 253 472 (aggregate) |
| *control* unix 1 KiB, 1 client | 3 | 17.1 | 16.6–17.2 | 18.5 | 26.5 | 53 832 |
| *control* unix 4 KiB, 1 client | 3 | 22.0 | 18.9–22.3 | 21.9 | 32.8 | 45 403 |
| *control* unix 64 KiB, 1 client | 3 | 29.5 | 29.4–34.6 | 31.0 | 48.4 | 32 179 |

**vcpus=8, same guest** (`results/f8/`) — the same arms, and the reason this
configuration is quoted at all:

| arm | n | median us | mean us | p99 us | ops/s |
|---|---|---|---|---|---|
| vsock 64 B, 1 client | 10 | 176.4 | 184.2 | 294.3 | 5 426 |
| vsock 64 B, 8 clients (per op) | 3 | **1 136.7** | 1 459.6 | 5 507.0 | **5 477** (aggregate) |
| vsock 4 KiB, 1 client | 3 | 182.7 | 193.5 | 417.4 | 5 163 |
| vsock 64 KiB, 1 client | 3 | 472.7 | 535.6 | 1 686.9 | 1 866 |
| *control* unix 64 B, 1 client | 10 | 17.6 | 18.1 | 31.7 | 55 085 |
| *control* unix 64 B, 8 clients | 3 | 22.9 | 25.3 | 65.0 | 263 880 (aggregate) |

**Guest-local vsock loopback** (the same echo agent talking to itself through
the guest kernel's `AF_VSOCK` loopback, no host muxer, no host client; measured
in three quiet boots, `results/loopback/`): median **87.8 / 88.2 / 89.3 us**,
mean 100.6–103.3 us, 64 B, 3 000 ops.

What each number includes:

- **vsock arm** — host client `write`+`read` syscalls, libkrun's `UnixAcceptorProxy`
  connection, the userspace muxer thread's two virtqueue transfers, both VM
  boundary crossings, the guest virtio-vsock driver, the guest echo thread's
  `read`+`write`. It excludes any filesystem: no FUSE request, no `/dev/fuse`
  syscall, no virtio-fs traffic, no cang daemon.
- **unix control** — the exact same client and the exact same echo loop
  (`src/unix-echo.c`), kernel unix-socket only, no VM at all. This is the floor
  of this measurement apparatus, not of a FUSE relay.
- **guest loopback** — both ends inside the guest, so it prices the guest half
  (kernel vsock + vCPU wakeups) with no host muxer: ~88 us of the ~143 us.

## 4. Fixed per-op vs per-byte

- Fixed cost per operation ≈ **145 us** (64 B arm). Payloads up to a few KiB are
  free within the spread: the 4 KiB arm read 148 us and the 1 KiB arm 168 us, so
  there is no resolvable per-byte cost below ~8 KiB.
- Per-byte marginal cost, from 64 B → 64 KiB: **(339 − 143) us for 65 472 B
  each way = 3.0 ns per payload byte for the round trip**, i.e. 1.5 ns/byte in
  each direction. That is ~193 MB/s of payload per direction (~386 MB/s moved),
  against ~3.3 GB/s for the host unix control (29.5 us for 64 KiB).
- A **new stream** (connect → muxer accept → guest accept → first op) costs
  **~521 us median** (366–693 us, 10 fresh connections), i.e. ~0.5 ms, one-off
  per relayed FUSE connection.

## 5. Where the time goes (host CPU, per-thread)

`attrib.sh 2`, per-thread `utime+stime` deltas across a 20 000-op 1-client 64 B
arm (`results/attrib2/`, `CLK_TCK=100`):

| thread | ticks | ms | us per op |
|---|---|---|---|
| `fc_vcpu 0` | 233 | 2 330 | 116.5 |
| `fc_vcpu 1` | 75 | 750 | 37.5 |
| launcher main (VMM event loop) | 37 | 370 | 18.5 |
| **`vsock muxer`** | **6** | **60** | **3.0** |
| total (all threads) | 351 | 3 510 | 175.5 |

Same boot, 8-client arm (40 000 ops, 2.52 s wall): 3 960 ms of launcher CPU =
**99 us host CPU per op** = 1.57 cores busy for 15.5 k ops/s.

Two readings that matter for the product:

- **cang's muxer is not the cost.** 3.0 us/op (1 stream), 4.5 us/op (8 streams),
  10 us/op (64 KiB). Whatever a reverse share pays, it is not the muxer loop.
- **The VM boundary is the cost**, and most of the vCPU figure is *not* guest
  work: this host has `kvm.halt_poll_ns=200000`, so each halted vCPU busy-polls
  for up to 200 us inside `KVM_RUN` after every HLT, and that spin is charged to
  the vCPU thread. With a ~143 us round trip the guest halts about twice per op,
  which is why two vCPU threads show ~99 % CPU while the guest is otherwise idle.
  I could not A/B this knob (host-global sysfs, needs root), so "the vCPU time is
  mostly halt-poll spin" is an inference from the pattern, not a separate
  measurement; the *latency* numbers are unaffected by the ambiguity.

## 6. Against the 250 us gate

- **Single-op latency: PASS.** 143.4 us median / 150.3 us mean at 1 stream
  (worst rep median 178 us, worst boot 191 us) — 0.57x the gate.
- **Tail: FAIL at the gate's letter.** p99 is 272.9 us (1 stream), 1 178 us
  (8 streams), and the max is 1.3–3.2 ms. If the criterion is "an operation must
  cost less than 250 us" rather than "the typical operation", the answer is no.
- **Aggregate throughput: 15.5 k ops/s** with 8 concurrent streams at 2 vCPUs
  (2.3x the single stream), 5.5 k ops/s at 8 vCPUs. Concurrency is a weak lever
  here, not the lever note 58 assumed.
- **Config sensitivity is larger than the margin**: the same binaries and guest
  gave 143 us (vcpus=2) and 176 us (vcpus=8) at 1 stream, and 15.5 k vs 5.5 k
  ops/s at 8 streams. Whatever cang's launch default is, it matters more than
  the gate's 100 us of headroom.

## 7. What contradicts note 58

1. **The price floor is better than borrowed, not worse.** Note 58 §4.2 took
   224 us from LWN's `vhost-vsock` arm and argued cang "should be worse" because
   libkrun muxes in userspace. Measured: **143 us median, 150 us mean**, 6.6 k
   ops/s per stream against the note's ~4.4 k. The muxer's own share is 3 us, so
   the extra userspace hop is not what note 58 expected it to be. Re-pricing
   §4.3's table at 143 us: host `git status` (1.5–3 k requests) 0.2–0.45 s
   rather than 0.4–0.9 s; an editor save still ~1 ms.
2. **"Concurrency is the only lever" is only 2.3x.** 8 in-flight streams gave
   15.5 k ops/s aggregate with per-op latency degraded to 483 us; 8 vCPUs made
   it 3x *worse* (5.5 k ops/s, 1 137 us per op), reproducibly (two boots). Note
   58 §4.2's "~4.4 k ops/s per in-flight stream, concurrency buys the rest" is
   the wrong shape: per-stream is 6.6 k and aggregate saturates near 15 k.
3. **The 250 us gate is passed but a per-op *ceiling* is not.** Note 58 §6 framed
   the gate as "single-op RTT > ~250 us means stop"; at 143 us the spike says
   continue — with the caveat that a relay serving a real `git status` under
   concurrency runs at ~483 us/op, so ~1.5–3 k requests becomes 0.7–1.5 s, not
   0.2–0.45 s.
4. Everything else note 58 asserted about this path survives contact: the
   direction (`listen = true`, host connects, guest listens), no new device, no
   fork change, one unix socket path under the per-user runtime dir, and the
   8 MiB `CONN_TX_BUF_SIZE` never binding (64 KiB frames are ~340 us, not
   credit-starved).

## 8. What this note did NOT measure

- **No FUSE, anywhere.** No `/dev/fuse`, no `fusermount3`, no guest FUSE server,
  no mount, no `git status`, no editor save. A real relayed op pays this
  measurement **plus** two `/dev/fuse` syscalls, the guest server's own op work,
  and (for reads/writes) payloads of 128 KiB+ — the 64 KiB arm prices the payload
  but not the FUSE frame or the server's side.
- **No cang CLI, no cang VM.** This is a hand-written launcher against the fork's
  ABI-2 C API; it does not exercise cang's seccomp/Landlock, its supervisor, its
  vsock port registry (50 430 here, chosen by note 58), or its image.
- **The guest was idle.** No virtio-fs traffic, no GPU, no waypipe/pulse/exec
  channel sharing the same vsock device, no CPU pressure inside the guest. A
  reverse share runs beside `/workspace`'s virtio-fs traffic and an actual build.
- **No concurrency above 8 streams**, and only one client process per stream.
- **No `halt_poll_ns` A/B** (host-global, needs root), so the vCPU-CPU split
  between halt polling and guest work is not isolated.
- **One host, one firmware** (`libkrunfw-v5.6.2-cang.6`, kernel line
  `linux-7.3.5` + hardened1 per its Makefile): I did not read the guest's
  `uname`, so the running kernel is inferred from the firmware pin. The guest's
  own `vsock` loopback number is only valid for that guest.
- **No `mmap`/page-fault path**: a FUSE relay serving shared mmap pays
  `filemap_fault`/`fuse_page_mkwrite` per 128 KiB page, which this ping-pong
  does not model at all.

## 9. Anomalies worth naming rather than smoothing

- **Within-boot spread of ±20 %** on the headline arm: 10 back-to-back reps of
  the identical 5 000-op 64 B arm gave medians 131.1–178.1 us in one boot (and
  134.8–173.9 in the late repeat of the same boot). It is not drift — the late
  repeat's median-of-medians is 137 us in `f2` and 176 us in `f8`, i.e. inside
  the same boot's own spread, and the first campaign gave 135/186/142/156 — and
  it is not the client (the unix control's 10 reps spanned 15.9–18.7 us). The likely
  causes are the adaptive `halt_poll_ns` and host CPU-state wakeup latency; I did
  not isolate them, so quote 143 us as the median-of-medians with that spread,
  not as a precision figure.
- **8 vCPUs is 3x worse than 2 for concurrency, reproducibly.** 1 137 us/op and
  5.5 k ops/s vs 483 us and 15.5 k. Note that this makes a *bigger* VM the wrong
  direction for a reverse share, which matters because cang's default launch is
  not 2 vCPUs.
- **`deferring proxy removal` WARNs from the muxer on every closed connection**
  and one `ERROR ... unexpected dgram pkt: 3` at boot (`results/diag/console.log`)
  are libkrun log noise, not failures; every arm completed every operation and
  the client verified the echoed bytes.
