//! The production backend: libkrun's Rust API behind [`LibkrunApi`].
//!
//! The trait is handle-shaped, because it mirrors the builder/object model the
//! launcher drives and the recording fake in the launcher's tests implements the
//! same calls. So this backend keeps libkrun's Rust values in an arena and hands
//! out their indices; taking a value out of the arena and putting the returned
//! one back is what makes the consume-and-return builder pattern look like a
//! handle call, and a handle whose slot was consumed is an error rather than a
//! silent use of a dead object.
//!
//! Two lifetime notes, because libkrun's Rust API ties values to borrowed data:
//!
//! - Values are instantiated at `'static`. Everything cang borrows is either
//!   static (the init blob) or a raw fd the launcher keeps open for the VM's
//!   lifetime (the console fds, the render-server fd), which is exactly the
//!   contract libkrun documents for those parameters.
//! - The built init [`Config`] is leaked. `Config::apply` wants `&'a self` and
//!   `&mut FsOverlay<'a>` at one lifetime and the config has to outlive the VM
//!   anyway; a cang process either runs the guest until it exits or exits after
//!   the failed launch, so process exit reclaims the few kilobytes.

use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use krun::{
    BlockDevice, ConsoleBuilder, ConsoleDevice, DiskFormat, FsDevice, FsOverlay, GpuDevice,
    LogLevel, LogOptions, LogStyle, MmioDeviceManager, NetDevice, NetFlags, Payload, TsiFlags,
    VirglRendererFlags, Vmm, VmmBuilder, VsockDevice,
};
use krun_init::{Builder as InitBuilder, Config as InitConfig};

use crate::api::{Handle, LibkrunApi};
use crate::display::headless_display_backend;
use crate::firmware;

/// One live libkrun object.
enum Slot {
    Empty,
    Payload(Payload),
    Overlay(FsOverlay<'static>),
    FsDevice(FsDevice<'static>),
    BlockDevice(BlockDevice),
    NetDevice(NetDevice),
    VsockDevice(VsockDevice),
    ConsoleBuilder(ConsoleBuilder<'static>),
    ConsoleDevice(ConsoleDevice<'static>),
    GpuDevice(GpuDevice),
    Devices(MmioDeviceManager<'static>),
    VmmBuilder(VmmBuilder<'static>),
    Vmm(Vmm<'static>),
    InitBuilder(InitBuilder),
    InitConfig(&'static InitConfig),
}

/// libkrun's Rust API, driven through [`LibkrunApi`].
#[derive(Default)]
pub struct LinkedLibkrunApi {
    slots: Vec<Slot>,
}

impl LinkedLibkrunApi {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, slot: Slot) -> Handle {
        self.slots.push(slot);
        self.slots.len() - 1
    }

    fn take(&mut self, handle: Handle, what: &str) -> Result<Slot> {
        let slot = self
            .slots
            .get_mut(handle)
            .ok_or_else(|| anyhow!("libkrun {what} handle {handle} is not live"))?;
        let taken = std::mem::replace(slot, Slot::Empty);
        if matches!(taken, Slot::Empty) {
            bail!("libkrun {what} handle {handle} was already consumed");
        }
        Ok(taken)
    }

    fn put(&mut self, handle: Handle, slot: Slot) {
        self.slots[handle] = slot;
    }

    fn take_kind<T>(
        &mut self,
        handle: Handle,
        what: &str,
        extract: impl FnOnce(Slot) -> Option<T>,
    ) -> Result<T> {
        let slot = self.take(handle, what)?;
        extract(slot)
            .ok_or_else(|| anyhow!("libkrun {what} handle {handle} holds a different object kind"))
    }
}

fn as_payload(slot: Slot) -> Option<Payload> {
    match slot {
        Slot::Payload(value) => Some(value),
        _ => None,
    }
}

fn as_overlay(slot: Slot) -> Option<FsOverlay<'static>> {
    match slot {
        Slot::Overlay(value) => Some(value),
        _ => None,
    }
}

fn as_devices(slot: Slot) -> Option<MmioDeviceManager<'static>> {
    match slot {
        Slot::Devices(value) => Some(value),
        _ => None,
    }
}

fn as_vmm_builder(slot: Slot) -> Option<VmmBuilder<'static>> {
    match slot {
        Slot::VmmBuilder(value) => Some(value),
        _ => None,
    }
}

fn as_console_builder(slot: Slot) -> Option<ConsoleBuilder<'static>> {
    match slot {
        Slot::ConsoleBuilder(value) => Some(value),
        _ => None,
    }
}

fn as_init_builder(slot: Slot) -> Option<InitBuilder> {
    match slot {
        Slot::InitBuilder(value) => Some(value),
        _ => None,
    }
}

/// Hand a taken device slot to the manager, which takes it by value.
fn attach_device(manager: &mut MmioDeviceManager<'static>, slot: Slot) -> Result<()> {
    match slot {
        Slot::FsDevice(device) => {
            manager.add(device);
        }
        Slot::BlockDevice(device) => {
            manager.add(device);
        }
        Slot::NetDevice(device) => {
            manager.add(device);
        }
        Slot::VsockDevice(device) => {
            manager.add(device);
        }
        Slot::ConsoleDevice(device) => {
            manager.add(device);
        }
        Slot::GpuDevice(device) => {
            manager.add(device);
        }
        _ => bail!("libkrun device manager add was given a non-device object"),
    }
    Ok(())
}

/// Borrow a raw fd for the VM's lifetime.
///
/// The Rust API documents that console fds must stay open and valid until the
/// VMM exits; cang's launcher owns them for exactly that long (the managed
/// console log is held by the caller, 0/1/2 are the worker's own stdio). A
/// negative fd means "no stream", which is how the C ABI mapped it too.
fn borrowed(fd: i32) -> Option<BorrowedFd<'static>> {
    (fd >= 0).then(|| unsafe { BorrowedFd::borrow_raw(fd) })
}

fn path_str<'a>(path: &'a Path, what: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{what} '{}' must be valid UTF-8", path.display()))
}

fn log_level(level: u32) -> Result<LogLevel> {
    Ok(match level {
        0 => LogLevel::Off,
        1 => LogLevel::Error,
        2 => LogLevel::Warn,
        3 => LogLevel::Info,
        4 => LogLevel::Debug,
        5 => LogLevel::Trace,
        other => bail!("libkrun log level {other} is out of range"),
    })
}

impl LibkrunApi for LinkedLibkrunApi {
    fn init_log(&mut self, level: u32) -> Result<()> {
        // The host log target is the worker's stderr, and the environment is
        // ignored so cang's log level is what libkrun uses.
        krun::init_log(
            borrowed(2),
            log_level(level)?,
            LogStyle::Never,
            LogOptions::NO_ENV,
        )?;
        Ok(())
    }

    fn check_nested_virt(&mut self) -> Result<bool> {
        Ok(krun::check_nested_virt())
    }

    fn payload_load_krunfw(&mut self) -> Result<Handle> {
        // libkrun opens the firmware by soname; hand it an already-loaded copy
        // from the package when there is one. See `crate::firmware`.
        match firmware::preload() {
            Some(path) => tracing::debug!(firmware = %path.display(), "libkrunfw preloaded"),
            None => tracing::debug!("no package-relative libkrunfw to preload"),
        }
        let payload = Payload::load_krunfw()?;
        Ok(self.push(Slot::Payload(payload)))
    }

    fn payload_append_cmdline(&mut self, handle: Handle, fragment: &str) -> Result<()> {
        let mut payload = self.take_kind(handle, "payload", as_payload)?;
        payload.append_cmdline(fragment);
        self.put(handle, Slot::Payload(payload));
        Ok(())
    }

    fn fs_overlay_new(&mut self) -> Result<Handle> {
        Ok(self.push(Slot::Overlay(FsOverlay::new())))
    }

    fn init_config_builder(&mut self) -> Result<Handle> {
        Ok(self.push(Slot::InitBuilder(InitConfig::builder())))
    }

    fn init_builder_args(&mut self, handle: Handle, args: &[String]) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.put(handle, Slot::InitBuilder(builder.args(&args)));
        Ok(handle)
    }

    fn init_builder_env(&mut self, handle: Handle, env: &[(String, String)]) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        let env: Vec<String> = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let env: Vec<&str> = env.iter().map(String::as_str).collect();
        self.put(handle, Slot::InitBuilder(builder.env(&env)));
        Ok(handle)
    }

    fn init_builder_workdir(&mut self, handle: Handle, workdir: &str) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        self.put(handle, Slot::InitBuilder(builder.workdir(workdir)));
        Ok(handle)
    }

    fn init_builder_rlimits(&mut self, handle: Handle, rlimits: &[String]) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        let rlimits: Vec<&str> = rlimits.iter().map(String::as_str).collect();
        self.put(handle, Slot::InitBuilder(builder.rlimits(&rlimits)));
        Ok(handle)
    }

    fn init_builder_dhcp(&mut self, handle: Handle, enable: bool) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        self.put(handle, Slot::InitBuilder(builder.dhcp(enable)));
        Ok(handle)
    }

    fn init_builder_build(&mut self, handle: Handle) -> Result<Handle> {
        let builder = self.take_kind(handle, "init builder", as_init_builder)?;
        // See the module comment: the config outlives the VM.
        let config: &'static InitConfig = Box::leak(Box::new(builder.build()));
        Ok(self.push(Slot::InitConfig(config)))
    }

    fn init_config_apply_in(
        &mut self,
        config: Handle,
        overlay: Handle,
        payload: Handle,
    ) -> Result<()> {
        let config: &'static InitConfig = match self.slots.get(config) {
            Some(Slot::InitConfig(config)) => config,
            Some(_) => bail!("libkrun init config handle {config} holds a different object kind"),
            None => bail!("libkrun init config handle {config} is not live"),
        };
        let mut overlay_value = self.take_kind(overlay, "fs overlay", as_overlay)?;
        let mut payload_value = self.take_kind(payload, "payload", as_payload)?;
        config.apply(&mut overlay_value, &mut payload_value)?;
        self.put(overlay, Slot::Overlay(overlay_value));
        self.put(payload, Slot::Payload(payload_value));
        Ok(())
    }

    fn mmio_device_manager_new(&mut self) -> Result<Handle> {
        Ok(self.push(Slot::Devices(MmioDeviceManager::new())))
    }

    fn mmio_device_manager_add(&mut self, devices: Handle, device: Handle) -> Result<()> {
        let mut manager = self.take_kind(devices, "device manager", as_devices)?;
        let device = self.take(device, "device")?;
        attach_device(&mut manager, device)?;
        self.put(devices, Slot::Devices(manager));
        Ok(())
    }

    fn fs_device_new(&mut self, tag: &str, host_path: &Path, read_only: bool) -> Result<Handle> {
        let host_path = path_str(host_path, "virtiofs host path")?;
        // Read-only is a separate constructor, not a setter.
        let device = if read_only {
            FsDevice::new_read_only(tag, host_path)?
        } else {
            FsDevice::new(tag, host_path)?
        };
        Ok(self.push(Slot::FsDevice(device)))
    }

    fn fs_device_set_overlay(&mut self, handle: Handle, overlay: Handle) -> Result<()> {
        let overlay = self.take_kind(overlay, "fs overlay", as_overlay)?;
        match self.slots.get_mut(handle) {
            Some(Slot::FsDevice(device)) => {
                device.set_overlay(overlay);
                Ok(())
            }
            Some(_) => bail!("libkrun fs device handle {handle} holds a different object kind"),
            None => bail!("libkrun fs device handle {handle} is not live"),
        }
    }

    fn block_device_new(&mut self, id: &str, path: &Path, read_only: bool) -> Result<Handle> {
        let path = path_str(path, "disk image path")?;
        let mut device = BlockDevice::new(id, path, DiskFormat::Raw)?;
        device.set_read_only(read_only);
        Ok(self.push(Slot::BlockDevice(device)))
    }

    fn net_device_new_unixstream_fd(
        &mut self,
        id: &str,
        fd: i32,
        mac: [u8; 6],
        features: u32,
        flags: u32,
    ) -> Result<Handle> {
        if fd < 0 {
            bail!("libkrun passt socket fd {fd} is not a valid descriptor");
        }
        // SAFETY: the device takes ownership of the descriptor, which is what the
        // launcher expects when it hands over the prepared passt socket.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let device = NetDevice::new_unixstream_fd(
            id,
            fd,
            &mac,
            features,
            NetFlags::from_bits_retain(flags),
        )?;
        Ok(self.push(Slot::NetDevice(device)))
    }

    fn vsock_device_new(&mut self, cid: u64, tsi_features: u32) -> Result<Handle> {
        let device = VsockDevice::new(cid, TsiFlags::from_bits_retain(tsi_features))?;
        Ok(self.push(Slot::VsockDevice(device)))
    }

    fn vsock_device_add_unix_port(
        &mut self,
        handle: Handle,
        port: u32,
        path: &Path,
        listen: bool,
    ) -> Result<()> {
        let path = path_str(path, "vsock unix socket path")?;
        match self.slots.get_mut(handle) {
            Some(Slot::VsockDevice(device)) => {
                device.add_unix_port(port, path, listen);
                Ok(())
            }
            Some(_) => bail!("libkrun vsock device handle {handle} holds a different object kind"),
            None => bail!("libkrun vsock device handle {handle} is not live"),
        }
    }

    fn vsock_device_add_port_forward(&mut self, handle: Handle, mapping: &str) -> Result<()> {
        match self.slots.get_mut(handle) {
            Some(Slot::VsockDevice(device)) => {
                device.add_port_forward(mapping)?;
                Ok(())
            }
            Some(_) => bail!("libkrun vsock device handle {handle} holds a different object kind"),
            None => bail!("libkrun vsock device handle {handle} is not live"),
        }
    }

    fn console_device_builder(&mut self) -> Result<Handle> {
        Ok(self.push(Slot::ConsoleBuilder(ConsoleDevice::builder())))
    }

    fn console_builder_add_default_console(
        &mut self,
        handle: Handle,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<()> {
        let mut builder = self.take_kind(handle, "console builder", as_console_builder)?;
        builder.add_default_console(borrowed(input_fd), borrowed(output_fd), borrowed(err_fd))?;
        self.put(handle, Slot::ConsoleBuilder(builder));
        Ok(())
    }

    fn console_builder_add_inout_port(
        &mut self,
        handle: Handle,
        name: &str,
        input_fd: Option<i32>,
        output_fd: Option<i32>,
    ) -> Result<()> {
        let mut builder = self.take_kind(handle, "console builder", as_console_builder)?;
        builder.add_inout_port(
            name,
            input_fd.and_then(borrowed),
            output_fd.and_then(borrowed),
        )?;
        self.put(handle, Slot::ConsoleBuilder(builder));
        Ok(())
    }

    fn console_builder_build(&mut self, handle: Handle) -> Result<Handle> {
        let builder = self.take_kind(handle, "console builder", as_console_builder)?;
        let device = builder.build()?;
        Ok(self.push(Slot::ConsoleDevice(device)))
    }

    fn gpu_device_new(
        &mut self,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<Handle> {
        let backend = headless_display_backend()?;
        let shm_size = usize::try_from(shm_size).context("GPU shm size does not fit usize")?;
        let mut device = GpuDevice::new(VirglRendererFlags::from_bits_retain(virgl_flags), backend)
            .shm_size(shm_size);
        if render_server_fd >= 0 {
            device = device.set_render_server_fd(render_server_fd)?;
        }
        Ok(self.push(Slot::GpuDevice(device)))
    }

    fn vmm_builder_new(&mut self) -> Result<Handle> {
        Ok(self.push(Slot::VmmBuilder(VmmBuilder::new())))
    }

    fn vmm_builder_vcpus(&mut self, handle: Handle, vcpus: u8) -> Result<Handle> {
        let builder = self.take_kind(handle, "vmm builder", as_vmm_builder)?;
        self.put(handle, Slot::VmmBuilder(builder.vcpus(vcpus)?));
        Ok(handle)
    }

    fn vmm_builder_ram_mib(&mut self, handle: Handle, ram_mib: u32) -> Result<Handle> {
        let builder = self.take_kind(handle, "vmm builder", as_vmm_builder)?;
        self.put(handle, Slot::VmmBuilder(builder.ram_mib(ram_mib)?));
        Ok(handle)
    }

    fn vmm_builder_nested_virt(&mut self, handle: Handle, enabled: bool) -> Result<Handle> {
        let builder = self.take_kind(handle, "vmm builder", as_vmm_builder)?;
        self.put(handle, Slot::VmmBuilder(builder.nested_virt(enabled)));
        Ok(handle)
    }

    fn vmm_builder_payload(&mut self, builder: Handle, payload: Handle) -> Result<Handle> {
        let value = self.take_kind(builder, "vmm builder", as_vmm_builder)?;
        let payload = self.take_kind(payload, "payload", as_payload)?;
        self.put(builder, Slot::VmmBuilder(value.payload(payload)));
        Ok(builder)
    }

    fn vmm_builder_devices(&mut self, builder: Handle, devices: Handle) -> Result<Handle> {
        let value = self.take_kind(builder, "vmm builder", as_vmm_builder)?;
        let devices = self.take_kind(devices, "device manager", as_devices)?;
        self.put(builder, Slot::VmmBuilder(value.devices(devices)));
        Ok(builder)
    }

    fn vmm_builder_destroy(&mut self, handle: Handle) -> Result<()> {
        // Dropping the builder releases the objects it owns.
        self.take(handle, "vmm builder")?;
        Ok(())
    }

    fn vmm_builder_build(&mut self, handle: Handle) -> Result<Handle> {
        let builder = self.take_kind(handle, "vmm builder", as_vmm_builder)?;
        let vmm = builder.build()?;
        Ok(self.push(Slot::Vmm(vmm)))
    }

    fn vmm_run(&mut self, handle: Handle) -> Result<()> {
        let vmm = self.take_kind(handle, "vmm", |slot| match slot {
            Slot::Vmm(value) => Some(value),
            _ => None,
        })?;
        vmm.run();
        bail!("libkrun start failed: the VMM event loop stopped without running a guest")
    }

    fn set_profile_path(&mut self, handle: Handle, profile_path: &Path) -> Result<Handle> {
        let profile_path = path_str(profile_path, "launch profile path")?;
        let builder = self.take_kind(handle, "vmm builder", as_vmm_builder)?;
        self.put(
            handle,
            Slot::VmmBuilder(builder.set_profile_path(profile_path)?),
        );
        Ok(handle)
    }
}
