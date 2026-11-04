# 47 - virtio-pci + MSI-X for the share: what the fork already has, what one reply's exits cost, and what the transport would take

**Read-only research, 2026-11-04.** No build, no VM arm, no code change. Fork read at
`deps/libkrun` **v2.0.0-cang.17** (`18e0e371 fs: identify a cached inode by its object, not by (ino, dev)`),
firmware `deps/libkrunfw` **v5.6.2-cang.4** (`KERNEL_VERSION = linux-7.2.10`, `Makefile:1`).
Guest kernel sources are upstream **v7.2** fetched from
`https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/<path>?h=v7.2`
and quoted by `grep -n` line number. Facts already measured elsewhere are cited, not re-measured:
the 516 us per create and the 11-43 replies per notification come from notes 44/46, and the only
per-exit cost figure available (1.81 us) is note 22's third-party number, not this host's.

## Verdict

1. **The fork has a real virtio-pci transport, and it is already exercised end to end with the
   fs device - but it is INTx only, has no MSI-X, and registers no notify ioeventfd.**
   `devices/src/virtio/pci.rs` is a 1725-line modern virtio-pci transport; `libkrun/src/vmm/device_manager/kvm/pci.rs`
   is its KVM host manager; `PciDeviceManager` is a public API device manager; and
   `tests/test_cases/src/test_virtio_pci.rs` boots a VM with `FsDevice` on the PCI bus and asserts
   the guest's `virtio-pci` driver binds (`:110-123`). What it cannot do: MSI-X
   (both MSI-X vector registers read back `VIRTIO_MSI_NO_VECTOR`, `pci.rs:684-686`, `:706-708`), and a
   kick on PCI costs a userspace exit (no `register_ioevent` anywhere in `kvm/pci.rs`).
2. **So PCI as it exists today is a wash at best and slightly worse than MMIO**: MMIO pays
   *2 userspace exits + 1 eventfd write* per reply; PCI+INTx pays *2 userspace exits + 2 `KVM_IRQ_LINE`
   ioctls* (the kick is no longer absorbed, and INTx asserts/deasserts through `vm.set_irq_line`).
   **Only PCI + MSI-X + a notify ioeventfd is cheaper than what cang runs now.**
3. **What MSI-X removes is exactly and only the guest's `INTERRUPT_STATUS` read and `INTERRUPT_ACK`
   write** - two userspace exits per used-ring batch, guest-visible and definite. Everything else on
   the completion path (device-side notification, wakeup, used-ring stores) is unchanged, and the
   injection is *already* exit-free today through an irqfd, so MSI-X cannot improve on it.
4. **Saving: 0.06-0.17 % of the measured 516 us per create, 0.3-0.75 % of the 8.2 s iteration**, and a
   hard ceiling of 3-8 % that only exists if every reply were its own interrupt (it is not: 11-43
   replies per notification, notes 44/46). **Effort: ~8 fork files + ~5 cang files, ~700-900 lines, one
   fork tag, plus a boot-path change (ACPI) and a mixed-transport manager for the GPU/DAX devices.**
   Ratio is bad; the same effort spent on [ticket 18](../tickets/18-workspace-target-on-block.md) buys
   orders of magnitude more. Recorded here so the option stops being re-proposed as cheap.

## 1 What transport code exists in this fork

### 1.1 A PCI root, a virtio-pci transport, and a KVM host manager

Three layers, all present:

| layer | file | what it is |
|---|---|---|
| PCI root / config / BAR window | `deps/libkrun/src/devices/src/pci.rs` (504 lines) | `PciRoot`, `ConfigMechanism1` (0xcf8/0xcfc), `Ecam`, `BarWindow`, `PciFunction` trait (`:77-82`), `PciIntxLine` trait (`:93-95`) |
| virtio-pci function | `deps/libkrun/src/devices/src/virtio/pci.rs` (1725 lines) | `VirtioPciTransport`: modern common-cfg / notify / ISR / device-cfg regions in BAR0, capability chain, INTx |
| KVM wiring | `deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/pci.rs` (271 lines) | `PciHostManager::register_virtio_device` (`:102-139`), INTx line = `vm.set_irq_line` (`:57-63`) |

It is compiled only for Linux/x86_64 (`device_manager/mod.rs:25-27`) and it is reachable from the API:
`PciDeviceManager` (`api/device_builders.rs:373-375`, `:478-517`) reports `uses_pci() == true`
(`:496-498`), and `vmm/builder.rs:1146-1152` instantiates `PciHostManager` when it does. So the answer
to "is there a PCI transport, and is it wired into a device manager" is **yes and yes** - what is
missing is everything above INTx.

The fork's own test is the strongest evidence it works, with the fs device, over ACPI:

```rust
 72             let mut devices = krun::PciDeviceManager::new();
 73             devices.add(rootfs);
 74             devices.add(console);
 ...
 87                 .payload(payload)
 88                 .acpi(true)
```
(`deps/libkrun/tests/test_cases/src/test_virtio_pci.rs:72-92`), guest side asserting the driver bound:

```rust
110             for entry in fs::read_dir("/sys/bus/pci/devices").expect("PCI sysfs is missing") {
...
121                 let driver =
122                     fs::read_link(device.join("driver")).expect("virtio-pci driver did not bind");
123                 assert_eq!(driver.file_name().unwrap(), "virtio-pci");
```
(`:110-123`)

### 1.2 MSI-X is absent from the transport, not merely unset

The transport implements the *register* names of the modern spec and reads them as "no vector":

```rust
684             (common_cfg::MSIX_CONFIG, WORD_SIZE) => {
685                 data.copy_from_slice(&VIRTIO_MSI_NO_VECTOR.to_le_bytes());
686             }
```
(`deps/libkrun/src/devices/src/virtio/pci.rs:684-686`, and again at `:706-708` for `QUEUE_MSIX_VECTOR`),
and the guest's writes to both are dropped by the catch-all arm (`:864 _ => {}` - `write_common_config`
has no `MSIX_CONFIG`/`QUEUE_MSIX_VECTOR` case at all). The config space carries only the five
vendor-specific virtio capabilities (`:438-476`), never a `PCI_CAP_ID_MSIX` capability, so the guest
never even tries: `pci_msix_vec_count()` returns `-EINVAL` when `dev->msix_cap` is 0
(`drivers/pci/msi/api.c:70-79`), and `vp_find_vqs()` falls through its three MSI-X attempts to
`vp_find_vqs_intx()` (`drivers/virtio/virtio_pci_common.c:517-545`).

The interrupt handler in the transport is INTx-only by construction: `PciInterrupt` keeps an `isr`
byte, asserts the line (`:238-249`), and *deasserts it when the guest reads the ISR*
(`:197-208`), which is the level-triggered INTx handshake, not an MSI.

### 1.3 Three things PCI mode needs that are unrelated to MSI-X

- **ACPI.** `pci_enabled && !acpi` is a hard error (`vmm/builder.rs:779-785`: `StartMicrovmError::PciRequiresAcpi`),
  and `acpi` defaults to `false` (`api/vmm_builder.rs:37-38`, `:132-143`: "When disabled (the
  default), virtio-mmio devices are passed on the kernel command line and SMP uses the MP table").
  cang never calls it - `grep -r acpi crates/cang-libkrun/src crates/cang/src` is empty - so moving the
  share to PCI also moves the whole VM from MP-table + `virtio_mmio.device=` cmdline
  (`device_manager/kvm/mmio.rs:170-187`) to a DSDT/MCFG boot (`arch/src/x86_64/acpi.rs:62-130`,
  `:226-228`). That is a boot-path change, not a device change.
- **No shared-memory devices.** PCI mode refuses any device with `shm_size` or `gpu_shm`
  (`vmm/builder.rs:794-812`). The fs device is fine today (`FsDevice::shm_size` is `None`,
  `api/device_builders.rs:846`; DAX is dropped by note 05), but `--gpu=drm` is not
  (`:2114 gpu_shm: Some(self.shm_size)`), so a full "everything on PCI" switch breaks the GPU path and
  the fs device would need a mixed-transport manager to move on its own.
- **No notify ioeventfd.** The MMIO manager registers the kick as an ioeventfd
  (`register_ioevent(queue_evt, &IoEventAddress::Mmio(mmiobase + NOTIFY_REG_OFFSET), i)`,
  `kvm/mmio.rs:137-144`) so `kvm.ko` absorbs it; `kvm/pci.rs` registers nothing, so a PCI kick is a
  `VcpuExit::MmioWrite` into `BarWindow` → `PciRoot::write_bar` → a scan over every function under a
  mutex (`devices/src/pci.rs:161-186`, `:335-349`).

### 1.4 What cang would have to add, given all of the above

cang's seam knows one manager only:

```rust
 47     // Devices.
 48     fn mmio_device_manager_new(&mut self) -> Result<Handle>;
 49     fn mmio_device_manager_add(&mut self, devices: Handle, device: Handle) -> Result<()>;
```
(`crates/cang-libkrun/src/api.rs:47-49`), used at `crates/cang-libkrun/src/linked.rs:305-315` and
`crates/cang/src/runtime/vm/libkrun/launcher.rs:385-388`, `:888-893`. Nothing else in cang names PCI.

## 2 What the MMIO completion path costs per interrupt, exactly

### 2.1 Device side (per used-ring batch, not per reply)

The fs worker publishes whatever the pool has finished and signals once per batch, gated by the
guest's own notification rules:

```rust
601             if self.queues[queue_index]
602                 .needs_notification(&self.mem)
603                 .unwrap_or(true)
604             {
605                 self.interrupt.signal_used_queue();
606             }
```
(`deps/libkrun/src/devices/src/virtio/fs/worker.rs:601-606`; the inline path is `:677-682`). That goes
through the transport-agnostic seam `InterruptTransport` (`devices/src/virtio/device.rs:56-83`) into

```rust
120     fn try_signal_status(&self, status: u32) -> Result<(), crate::Error> {
121         let mut guard = self.0.status.lock().unwrap();
122         *guard |= status as usize;
123         self.intc()
124             .lock()
125             .unwrap()
126             .set_irq(self.irq_line(), Some(&self.0.event))
127         ...
```
(`devices/src/virtio/mmio.rs:120-129`), and with the default in-kernel IOAPIC
(`KvmIoapic`, `builder.rs:1197`) the "IRQ" is a single eventfd write:

```rust
 60     fn set_irq(
 61         &self,
 62         _irq_line: Option<u32>,
 63         interrupt_evt: Option<&EventFd>,
 64     ) -> Result<(), DeviceError> {
 65         if let Some(interrupt_evt) = interrupt_evt {
 66             if let Err(e) = interrupt_evt.write(1) {
```
(`devices/src/legacy/kvmioapic.rs:60-69`), which the manager bound to a GSI as an irqfd
(`vm.register_irqfd(mmio_device.interrupt_evt(), irq)`, `kvm/mmio.rs:146`). **Injection is therefore
already exit-free: one eventfd write, and KVM delivers the vector into the guest LAPIC in-kernel.**

### 2.2 Guest side (the three accesses, two of which exit to userspace)

From the guest driver:

```c
264 static bool vm_notify(struct virtqueue *vq)
...
270 	writel(vq->index, vm_dev->base + VIRTIO_MMIO_QUEUE_NOTIFY);
```
(`drivers/virtio/virtio_mmio.c:264-270`) and

```c
292 	/* Read and acknowledge interrupts */
293 	status = readl(vm_dev->base + VIRTIO_MMIO_INTERRUPT_STATUS);
294 	writel(status, vm_dev->base + VIRTIO_MMIO_INTERRUPT_ACK);
```
(`drivers/virtio/virtio_mmio.c:292-294`).

| # | guest access | where it lands in the fork | exits to userspace? |
|---|---|---|---|
| 1 | `writel(index, QUEUE_NOTIFY)` | ioeventfd, datamatch = queue index (`kvm/mmio.rs:137-144`); the userspace arm `0x50 => … eventfd.write(1)` (`mmio.rs:361-368`) only runs on a datamatch miss | **no** (kvm.ko answers it) |
| 2 | `readl(INTERRUPT_STATUS)` 0x60 | `VcpuExit::MmioRead` → `mmio_bus.read` (`vmm/linux/vstate.rs:1557-1561`) → `0x60 => self.interrupt.status() as u32` (`mmio.rs:287`) | **yes - exit 1** |
| 3 | `writel(status, INTERRUPT_ACK)` 0x64 | `VcpuExit::MmioWrite` → `vmm/linux/vstate.rs:1563-1567` → `0x64 => … self.interrupt.on_ack(v)` (`mmio.rs:369-375`) → `on_ack` clears and calls `clear_irq` (`:149-157`, a no-op for `KvmIoapic`) | **yes - exit 2** |
| 4 | injection | irqfd (`kvm/mmio.rs:146`) | **no** |

So per completion batch: **2 userspace MMIO exits + 1 eventfd write**, both exits taken in the *vCPU
thread* (`vstate.rs:1557-1568`), i.e. inside the `fc_vcpu` 48 % bucket of note 46's split, plus two
`Bus::read`/`Bus::write` lock acquisitions (`devices/src/bus.rs:146-170`). This confirms note 22's
correction of the count and pins the code path for it.

## 3 With MSI-X, what actually goes away

### 3.1 Guest-visible: both ISR accesses, because the MSI-X path never uses the ISR

The kernel has two disjoint handlers. INTx:

```c
106 static irqreturn_t vp_interrupt(int irq, void *opaque)
...
111 	/* reading the ISR has the effect of also clearing it so it's very
112 	 * important to save off the value. */
113 	isr = ioread8(vp_dev->isr);
```
(`drivers/virtio/virtio_pci_common.c:106-124`). MSI-X, per queue, with the vq callback bound
*directly* to the vector:

```c
362 	err = request_irq(pci_irq_vector(vp_dev->pci_dev, msix_vec),
363 			  vring_interrupt, 0,
364 			  vp_dev->msix_names[msix_vec], vq);
```
(`drivers/virtio/virtio_pci_common.c:355-364`). There is no ISR read and no ack write anywhere on that
path - one fewer memory access pair, and the two userspace exits with it. **This is the whole of the
guest-visible saving, and it is definite.**

### 3.2 Device side: nothing improves, and the notify must not regress

The MSI-X table lives in a BAR and the guest writes {address, data, vector control} into it once per
vector (`PCI_MSIX_ENTRY_*`, `include/uapi/linux/pci_regs.h:332-352`); the device then delivers by
writing that message. Because this VMM owns its own BAR decoder and guest memory map
(`read_bar_region`/`write_bar_region`, `devices/src/virtio/pci.rs:604-656`), the table can simply be
guest memory the transport reads at notification time - so table access costs **no exit at all**,
setup-time or later, and needs **no new BAR**: BAR0 is 0x8000 bytes with only 0..0x4000 occupied
(`:57`, `:63-68`), so `0x4000` is free and `kvm/pci.rs:113-117` keeps its one-window-per-function
layout.
Delivery options, both in-kernel and neither needing a guest exit:
- **irqfd + `KVM_IRQ_ROUTING_MSI`** - zero syscalls per interrupt. The fork already has every piece:
  it builds `KVM_IRQ_ROUTING_MSI` routes (`devices/src/legacy/ioapic_kvm.rs:71-94`, `:125-146`),
  ships them to the VM owner thread (`WorkerMessage::GsiRoute` → `vm.fd().set_gsi_routing(&routing)`,
  `vmm/worker.rs:58`), and registers irqfds (`kvm/mmio.rs:146`). This is QEMU's `kvm_msi_via_irqfd`.
- **`KVM_SIGNAL_MSI`** per interrupt - one ioctl where an eventfd write is today, i.e. no better than
  the existing irqfd and strictly worse than the eventfd.
Either way the *host* cost stays "one notification per batch", exactly as now.

### 3.3 What is guest-visible saving vs speculative

| claim | status |
|---|---|
| the `INTERRUPT_STATUS` read and `INTERRUPT_ACK` write disappear from the guest's IRQ path | **certain**, quoted above (`virtio_pci_common.c:355-364` vs `:106-124`) |
| those two accesses are today two userspace exits handled on the vCPU thread | **certain**, quoted (`vstate.rs:1557-1568`) |
| removing them saves 2 x (per-exit cost) per batch, ~3.6-10 us | **arithmetic**; per-exit cost on *this* host is unmeasured (note 22 §1.3 uses a third-party 1.81 us userspace-MMIO figure, and this host is itself a KVM guest) |
| MSI-X improves *injection* | **false here** - injection is already an irqfd, i.e. already exit-free (`kvm/mmio.rs:146`, `kvmioapic.rs:60-69`) |
| APICv / posted interrupts / split irqchip would add to the win | **no** - APICv virtualises APIC *register* accesses and IPIs, and the ISR read is a virtio BAR access, not an APIC one; split irqchip would make it *worse* (EOI exits) and is off by default (`vmm/resources.rs:270`), while MSI needs nothing from `KVM_CAP_SPLIT_IRQCHIP` (`devices/src/legacy/ioapic_kvm.rs:286-293`) |
| MSI-X lets the vector be affine to the waiting vCPU (`PCI_IRQ_AFFINITY`, `virtio_pci_common.c:152-158`) | **speculative and small**: there is one request queue, so 2 vectors total |
| PCI + INTx would already help | **false**, and it is a regression: kick loses its ioeventfd (one new exit), the ISR read stays, the ack goes, and INTx adds two `KVM_IRQ_LINE` ioctls (`kvm/pci.rs:57-63`, `devices/src/virtio/pci.rs:197-208`, `:238-249`) where MMIO has one eventfd write |

## 4 Verdict: how big the slice is, what it costs, and how to test it cheaply

### 4.1 Arithmetic on the numbers already measured

Basis (all cited, none re-measured): **516 us of host CPU per create** (note 46 §1 table); **9 756
interrupts per 108 480 creates = 1 per 11.1 creates / 650 per second** (note 46 §2); **~43 replies per
notification** in note 44's denser window (note 44 §5, `:95-98`); **8.2 s and ~69 k requests** per
compile iteration, the shape of note 45 (`:44-46`) and note 46 §1; note 22's 1.81 us per userspace
MMIO exit.

| step | sum | result |
|---|---|---|
| exits per create | 2 / 11.1 | **0.18** |
| exit CPU per create | 0.18 x 1.81-5 us | **0.3-0.9 us = 0.06-0.17 % of 516 us** |
| exits per iteration | 2 x (69 000 / 11.1) | ~12 400 |
| exit CPU per 8.2 s iteration | 12 400 x 1.81-5 us | **22-62 ms = 0.3-0.75 %** |
| ceiling if every reply had its own vector (no amortisation) | 69 000 x 2 x 1.81-5 us | **0.25-0.69 s = 3-8 %** |

The second row is the one that answers the brief: **this is a tenth-of-a-percent change at the
measured interrupt rate**, and a few percent even under the assumption that destroys the measurement
(one interrupt per reply). It cannot touch the 8.2 s -> 1.45 s gap. Note 46 reached "a few percent"
from the interrupt counter alone; this note pins it to two specific instructions and shows the 11x
amortisation is what kills it.

### 4.2 Effort, concretely

Fork (`deps/libkrun`, one new tag `v2.0.0-cang.18`):

| file | change | size |
|---|---|---|
| `devices/src/virtio/pci.rs` | MSI-X capability + table state at BAR0+0x4000 + `MSIX_CONFIG`/`QUEUE_MSIX_VECTOR` wire-up (`:684-708`, `:864`) + `MsixInterrupt: InterruptHandler` replacing `PciInterrupt` (`:171-261`) | ~300-500 lines |
| `libkrun/src/vmm/device_manager/kvm/pci.rs` | notify ioeventfds for the BAR0 notify region, MSI GSI allocation, `set_gsi_routing`, `register_irqfd` | ~100-150 |
| `vmm/device_manager/irq.rs` | a second, larger GSI range - `IRQ_MAX = 23` gives only 19 lines (`arch/src/x86_64/layout.rs:28-30`), while KVM allows ~4095 routes (`ioapic_kvm.rs:88-89`) | ~20 |
| `devices/src/virtio/device.rs`, `devices/src/pci.rs` | none expected (`InterruptTransport` is already generic; the table fits in BAR0) | 0-20 |
| `tests/test_cases/src/test_virtio_pci.rs` | assert the guest took the MSI-X path | ~40 |

cang: `crates/cang-libkrun/src/api.rs` (+2 trait methods), `linked.rs` (+`Slot`, `attach_device`
dispatch `:144-170`), `crates/cang/src/runtime/vm/libkrun/launcher.rs` (`:385-388`, `:888-893`),
`launcher`/builder config for `.acpi(true)` and the transport choice, `--fs`/config surface (~120
lines) - **plus** a mixed-transport manager if the GPU or DAX devices must stay on MMIO
(`vmm/builder.rs:794-812`), and a whole-VM boot-path move from MP table + `virtio_mmio.device=` to ACPI.
No new crate is added, so no vendor hash should move - prove that with the fakeHash build rather
than assuming it - but the fork tag and pin cascade still does (`flake.lock` `libkrun-src`, the
`deps/libkrun` submodule pointer).

Total: **~700-900 lines across ~13 files, one fork tag, one boot-path change** for 0.1 % of the metric.

### 4.3 The smallest experiment that would prove the saving, in order of cost

**(a) Count the exits - 5 lines, no transport, must come first.** Two `AtomicU64`s in the
`VcpuExit::MmioRead`/`MmioWrite` arms (`vmm/linux/vstate.rs:1557-1568`), bucketed by address
(`INTERRUPT_STATUS` = base+0x60, `INTERRUPT_ACK` = base+0x64), logged at teardown the way the opcode
histogram already is (`fs/worker.rs:550-555`). One fork build, one `tools/rust-compile-bench` arm, and
the whole estimate above becomes measured: exits per iteration, exits per request, and the interrupt
amortisation factor for the *compile* shape (which notes 44/46 only have for the create storm). If
that count x 1.81 us is under ~1 % of the iteration, stop here - the answer is no.
(A zero-code alternative exists only on a host with debugfs mounted: the per-VM counter
`/sys/kernel/debug/kvm/<pid>-<n>/mmio_exits` before/after one iteration; it is not mounted in this
sandbox, so the counter in (a) is the reliable route.)

**(b) PCI+INTx as a null control - the cheapest real transport A/B.** The fs device on
`PciDeviceManager` already boots (test above), so the same bench arm run that way should reproduce the
*current* numbers within noise (2 exits either way, per §3.3). If it does not, the measurement
apparatus is lying and no MSI-X conclusion is worth having. Cost: the cang-side plumbing in §4.2
minus the MSI-X work, which is most of the cang-side effort - so run (c) first and use (b) only to
confirm that the exit count itself (a) was not an artefact.

**(c) Remove one exit without PCI - ~50 lines, proves half the lever.** `INTERRUPT_ACK` is a *write*,
so it can be absorbed by an ioeventfd exactly as the kick is: register
`IoEventAddress::Mmio(mmiobase + 0x64)` with datamatch `1` (`VIRTIO_MMIO_INT_VRING`) beside the
existing loop (`kvm/mmio.rs:137-144`), and have a reader clear the status bits the way `on_ack` does
(`mmio.rs:149-157`). Non-matching ack values fall back to the userspace exit, i.e. to today's
behaviour. A/B that against `main` on the compile iteration: the delta is exactly one exit per
interrupt, so **MSI-X's saving is 2x that delta**. If the A/B is in the noise, the transport is not
worth writing - and unlike (b) it needs no PCI, no ACPI, no kernel change.

What **not** to do: don't treat `INTERRUPT_STATUS` as absorbable (ioeventfds are write-only, so the
read is unavoidable on any MMIO transport - it is why MSI-X is the only transport-level fix); don't
chase APICv/split-irqchip (§3.3); don't read note 22's "3-5 us per request" as a per-request budget -
that figure assumed one ISR pair per request, and note 46 §2's counter shows 11-43 replies per pair.

## Sources

| what | where |
|---|---|
| fork revision read | `deps/libkrun` `git describe --tags` = `v2.0.0-cang.17`, HEAD `18e0e371`; firmware `v5.6.2-cang.4`, `deps/libkrunfw/Makefile:1` |
| PCI root / BAR model | `deps/libkrun/src/devices/src/pci.rs:12` (256-byte config), `:77-95` (`PciFunction`, `PciIntxLine`), `:142-186` (root dispatch), `:297-349` (Ecam, BarWindow) |
| virtio-pci transport | `deps/libkrun/src/devices/src/virtio/pci.rs:57` (`VIRTIO_PCI_BAR0_SIZE`), `:63-68` (region offsets), `:171-261` (INTx `PciInterrupt`), `:432-536` (capability chain), `:604-656` (BAR decode), `:684-708` / `:864` (MSI-X registers unimplemented) |
| KVM PCI manager | `deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/pci.rs:57-63` (`set_irq_line`), `:102-139` (registration, GSI per function), `:113-117` (BAR windows) |
| PCI mode's preconditions | `deps/libkrun/src/libkrun/src/vmm/builder.rs:779-785` (ACPI required), `:794-812` (shm/gpu_shm refused), `deps/libkrun/src/libkrun/src/api/vmm_builder.rs:37-38`, `:132-143` (both default off) |
| fs device on PCI, end to end | `deps/libkrun/tests/test_cases/src/test_virtio_pci.rs:72-92`, `:110-123` |
| MMIO completion path | `deps/libkrun/src/devices/src/virtio/mmio.rs:120-129`, `:149-157`, `:287`, `:361-375`, `:406-415`; `deps/libkrun/src/devices/src/virtio/device.rs:56-83`; `deps/libkrun/src/libkrun/src/vmm/device_manager/kvm/mmio.rs:137-147`; `deps/libkrun/src/devices/src/legacy/kvmioapic.rs:60-69`; `deps/libkrun/src/libkrun/src/vmm/linux/vstate.rs:1557-1568` |
| worker signalling | `deps/libkrun/src/devices/src/virtio/fs/worker.rs:586-608`, `:663-683`, `:550-555` |
| cang's seam | `crates/cang-libkrun/src/api.rs:47-49`; `crates/cang-libkrun/src/linked.rs:144-170`, `:305-315`; `crates/cang/src/runtime/vm/libkrun/launcher.rs:385-388`, `:888-893` |
| guest kernel, tag v7.2 | `drivers/virtio/virtio_mmio.c:264-272`, `:285-307`; `drivers/virtio/virtio_pci_common.c:50-57`, `:106-124`, `:355-364`, `:517-545`; `drivers/pci/msi/api.c:70-79`; `drivers/pci/msi/msi.c:611-630` (`can_mask`); `include/uapi/linux/pci_regs.h:29`, `:235`, `:332-352`; `drivers/pci/probe.c:2654` (`pci_msix_init`) - all `git.kernel.org/.../plain/<path>?h=v7.2` |
| pinned guest kernel config | `deps/libkrunfw/config-libkrunfw_x86_64-kvm-lto:1332` (`CONFIG_PCI=y`), `:1341` (`CONFIG_PCI_MSI=y`), `:2619-2621` (`CONFIG_VIRTIO_PCI=y`) - **no kernel change is needed for PCI or MSI-X** |
| numbers used, not re-measured | note 46 §1 (516 us/create split), §2 (9 756 interrupts / 108 480 creates); note 44 §5 (`:95-98`, 6 809 interrupts, ~43 replies/notification); note 45 (`:44-46`, ~69 k lookups); note 22 §1.3 (three-exits accounting, 1.81 us userspace MMIO exit, irqfd 5.81 us, `halt_poll_ns=200000`) |
