//! libkrun API surface used by the direct launcher.
//!
//! ABI 2 replaced libkrun's flat `krun_set_*` configuration calls with an
//! object model: the caller builds a payload, a filesystem overlay, an init
//! config and a device manager, then hands them to a VMM builder. `Handle` is
//! an opaque handle allocated by the implementation - the dynamic loader
//! (production) or the recording fake (tests).
//!
//! Methods whose C entry point takes a `*Handle` (libkrun's "consume self and
//! return self" builder pattern) **return the possibly-reboxed handle**: the
//! library owns the object and may hand back a different pointer, so the caller
//! must keep using what it is given.

use anyhow::Result;
use std::path::Path;

/// Opaque libkrun object handle.
pub(crate) type Handle = usize;

pub(crate) trait LibkrunApi {
    /// `krun_init_log`: host log target (an fd), level, style, options.
    fn init_log(&mut self, level: u32) -> Result<()>;
    /// `krun_check_nested_virt`: diagnostic only, never a gate.
    fn check_nested_virt(&mut self) -> Result<bool>;

    // Payload (the krunfw kernel the guest boots).
    fn payload_load_krunfw(&mut self) -> Result<Handle>;
    fn payload_append_cmdline(&mut self, payload: Handle, fragment: &str) -> Result<()>;

    // Init injection. `krun_init_config_apply_in` lives in `libkrun_init.so`
    // and adds the guest init blob plus its config to the overlay, and appends
    // `init=` to the payload's kernel command line.
    fn fs_overlay_new(&mut self) -> Result<Handle>;
    fn init_config_builder(&mut self) -> Result<Handle>;
    fn init_builder_args(&mut self, builder: Handle, args: &[String]) -> Result<Handle>;
    fn init_builder_env(&mut self, builder: Handle, env: &[(String, String)]) -> Result<Handle>;
    fn init_builder_workdir(&mut self, builder: Handle, workdir: &str) -> Result<Handle>;
    fn init_builder_rlimits(&mut self, builder: Handle, rlimits: &[String]) -> Result<Handle>;
    fn init_builder_dhcp(&mut self, builder: Handle, enable: bool) -> Result<Handle>;
    fn init_builder_build(&mut self, builder: Handle) -> Result<Handle>;
    fn init_config_apply_in(
        &mut self,
        config: Handle,
        overlay: Handle,
        payload: Handle,
    ) -> Result<()>;

    // Devices.
    fn mmio_device_manager_new(&mut self) -> Result<Handle>;
    fn mmio_device_manager_add(&mut self, devices: Handle, device: Handle) -> Result<()>;
    fn fs_device_new(&mut self, tag: &str, host_path: &Path, read_only: bool) -> Result<Handle>;
    fn fs_device_set_overlay(&mut self, device: Handle, overlay: Handle) -> Result<()>;
    fn block_device_new(&mut self, id: &str, path: &Path, read_only: bool) -> Result<Handle>;
    fn net_device_new_unixstream_fd(
        &mut self,
        id: &str,
        fd: i32,
        mac: [u8; 6],
        features: u32,
        flags: u32,
    ) -> Result<Handle>;
    fn vsock_device_new(&mut self, cid: u64, tsi_features: u32) -> Result<Handle>;
    fn vsock_device_add_unix_port(
        &mut self,
        device: Handle,
        port: u32,
        path: &Path,
        listen: bool,
    ) -> Result<()>;
    fn vsock_device_add_port_forward(&mut self, device: Handle, mapping: &str) -> Result<()>;
    fn console_device_builder(&mut self) -> Result<Handle>;
    fn console_builder_add_default_console(
        &mut self,
        builder: Handle,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<()>;
    fn console_builder_add_inout_port(
        &mut self,
        builder: Handle,
        name: &str,
        input_fd: Option<i32>,
        output_fd: Option<i32>,
    ) -> Result<()>;
    fn console_builder_build(&mut self, builder: Handle) -> Result<Handle>;
    fn gpu_device_new(
        &mut self,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<Handle>;

    // VMM builder and run.
    fn vmm_builder_new(&mut self) -> Result<Handle>;
    fn vmm_builder_vcpus(&mut self, builder: Handle, vcpus: u8) -> Result<Handle>;
    fn vmm_builder_ram_mib(&mut self, builder: Handle, ram_mib: u32) -> Result<Handle>;
    fn vmm_builder_nested_virt(&mut self, builder: Handle, enabled: bool) -> Result<Handle>;
    fn vmm_builder_payload(&mut self, builder: Handle, payload: Handle) -> Result<Handle>;
    fn vmm_builder_devices(&mut self, builder: Handle, devices: Handle) -> Result<Handle>;
    /// Release a builder that was never consumed by `vmm_builder_build`.
    fn vmm_builder_destroy(&mut self, builder: Handle) -> Result<()>;
    fn vmm_builder_build(&mut self, builder: Handle) -> Result<Handle>;
    /// `krun_vmm_run`: never returns while the guest is alive; a return means the
    /// VMM event loop stopped, so it is always an error for the launcher.
    fn vmm_run(&mut self, vmm: Handle) -> Result<()>;
    /// Fork-only launch profiler (ABI-2 re-add is ticket 10). A library without
    /// the symbol is a no-op, as the v1 optional symbol was.
    fn set_profile_path(&mut self, builder: Handle, profile_path: &Path) -> Result<Handle>;
}
