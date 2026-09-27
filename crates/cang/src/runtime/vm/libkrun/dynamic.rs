//! Dynamic libkrun loading and symbol binding.
//!
//! cang binds libkrun through `dlopen`/`dlsym`. ABI 2 splits the surface into
//! two libraries: `libkrun.so.2` (VMM, devices, payload) and `libkrun_init.so`
//! (the guest init blob plus its config builder). Both must be present before a
//! launch can be configured.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use crate::runtime::host_tools::package_root_from_exe;

use super::api::{Handle, LibkrunApi};

const LIBKRUN_LIBRARY_ENV: &str = "CANG_LIBKRUN_LIBRARY";
const DEFAULT_LIBKRUN_NAMES: [&str; 2] = ["libkrun.so.2", "libkrun.so"];
const DEFAULT_INIT_NAMES: [&str; 2] = ["libkrun_init.so.0", "libkrun_init.so"];
const CANG_LIBKRUN_LOG_TARGET_STDERR_FD: i32 = 2;
const CANG_LIBKRUN_LOG_STYLE_NEVER: u32 = 2;
const CANG_LIBKRUN_LOG_OPTION_NO_ENV: u32 = 1;
const CANG_NET_FEATURE_CSUM: u32 = 1 << 0;
const CANG_NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const CANG_NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const CANG_NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const CANG_NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const CANG_NET_FEATURE_HOST_UFO: u32 = 1 << 14;
pub(crate) const CANG_LIBKRUN_COMPAT_NET_FEATURES: u32 = CANG_NET_FEATURE_CSUM
    | CANG_NET_FEATURE_GUEST_CSUM
    | CANG_NET_FEATURE_GUEST_TSO4
    | CANG_NET_FEATURE_GUEST_UFO
    | CANG_NET_FEATURE_HOST_TSO4
    | CANG_NET_FEATURE_HOST_UFO;
/// `KRUN_DISK_FORMAT_RAW` from `libkrun.h`.
const CANG_DISK_FORMAT_RAW: u32 = 0;
/// Tag the guest kernel resolves as its root filesystem (`KRUN_FS_ROOT_TAG`).
pub(in crate::runtime::vm::libkrun) const CANG_FS_ROOT_TAG: &str = "/dev/root";
const KRUN_RESULT_SUCCESS: u64 = 0;
/// `KRUN_PUSH_STR_TYPE_TAG` from `libkrun.h`.
const KRUN_PUSH_STR_TYPE_TAG: u32 = 16777228;
/// `KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER` from `libkrun_display.h`.
const KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER: u64 = 1;
/// `KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED` from `libkrun_display.h`.
const KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED: i32 = -2;

/// `KrunStr`: a borrowed byte range, not a NUL-terminated string.
#[repr(C)]
#[derive(Clone, Copy)]
struct KrunStr {
    data: *const c_char,
    len: usize,
}

/// `KrunBytes`: a borrowed byte range.
#[repr(C)]
#[derive(Clone, Copy)]
struct KrunBytes {
    data: *const u8,
    len: usize,
}

/// `KrunPushStrVtable`: the writer `krun_error_message` pushes text into.
#[repr(C)]
struct KrunPushStrVtable {
    drop: Option<unsafe extern "C" fn(*mut c_void)>,
    push: Option<unsafe extern "C" fn(*mut c_void, KrunStr) -> bool>,
}

/// `KrunVtableHandle`: the stack-allocated handle passed to vtable consumers.
#[repr(C)]
struct KrunVtableHandle {
    type_tag: u32,
    metadata: u32,
    vtable_ptr: *const c_void,
    user_data: *const c_void,
    vtable_size: u16,
}

type KrunInitLog = unsafe extern "C" fn(i32, u32, u32, u32, *mut *mut c_void) -> u64;
type KrunCheckNestedVirt = unsafe extern "C" fn() -> bool;
type KrunPayloadLoadKrunfw = unsafe extern "C" fn(*mut *mut c_void) -> *mut c_void;
type KrunPayloadAppendCmdline = unsafe extern "C" fn(*mut c_void, KrunStr);
type KrunFsOverlayNew = unsafe extern "C" fn() -> *mut c_void;
type KrunInitConfigBuilder = unsafe extern "C" fn() -> *mut c_void;
type KrunInitBuilderArgs = unsafe extern "C" fn(*mut *mut c_void, *const KrunStr, usize);
type KrunInitBuilderEnv = unsafe extern "C" fn(*mut *mut c_void, *const KrunStr, usize);
type KrunInitBuilderWorkdir = unsafe extern "C" fn(*mut *mut c_void, KrunStr);
type KrunInitBuilderRlimits = unsafe extern "C" fn(*mut *mut c_void, *const KrunStr, usize);
type KrunInitBuilderDhcp = unsafe extern "C" fn(*mut *mut c_void, bool);
type KrunInitBuilderBuild = unsafe extern "C" fn(*mut *mut c_void) -> *mut c_void;
type KrunInitConfigApplyIn = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut *mut c_void,
) -> u64;
type KrunMmioDeviceManagerNew = unsafe extern "C" fn() -> *mut c_void;
type KrunMmioDeviceManagerAdd = unsafe extern "C" fn(*mut c_void, *mut c_void);
type KrunFsDeviceNew = unsafe extern "C" fn(KrunStr, KrunStr, *mut *mut c_void) -> *mut c_void;
type KrunFsDeviceSetOverlay = unsafe extern "C" fn(*mut c_void, *mut c_void);
type KrunBlockDeviceNew =
    unsafe extern "C" fn(KrunStr, KrunStr, u32, *mut *mut c_void) -> *mut c_void;
type KrunBlockDeviceSetReadOnly = unsafe extern "C" fn(*mut c_void, bool);
type KrunNetDeviceNewUnixstreamFd =
    unsafe extern "C" fn(KrunStr, i32, KrunBytes, u32, u32, *mut *mut c_void) -> *mut c_void;
type KrunVsockDeviceNew = unsafe extern "C" fn(u64, u32, *mut *mut c_void) -> *mut c_void;
type KrunVsockDeviceAddUnixPort = unsafe extern "C" fn(*mut c_void, u32, KrunStr, bool);
type KrunVsockDeviceAddPortForward =
    unsafe extern "C" fn(*mut c_void, KrunStr, *mut *mut c_void) -> u64;
type KrunConsoleDeviceBuilder = unsafe extern "C" fn() -> *mut c_void;
type KrunConsoleBuilderAddDefaultConsole =
    unsafe extern "C" fn(*mut c_void, i32, i32, i32, *mut *mut c_void) -> u64;
type KrunConsoleBuilderAddInoutPort =
    unsafe extern "C" fn(*mut c_void, KrunStr, i32, i32, *mut *mut c_void) -> u64;
type KrunConsoleBuilderBuild = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;
type KrunDisplayBackendNew =
    unsafe extern "C" fn(*const c_void, usize, *mut *mut c_void) -> *mut c_void;
type KrunGpuDeviceNew = unsafe extern "C" fn(u32, *mut c_void) -> *mut c_void;
type KrunGpuDeviceShmSize = unsafe extern "C" fn(*mut *mut c_void, usize);
type KrunGpuDeviceSetRenderServerFd =
    unsafe extern "C" fn(*mut *mut c_void, i32, *mut *mut c_void) -> u64;
type KrunVmmBuilderNew = unsafe extern "C" fn() -> *mut c_void;
type KrunVmmBuilderVcpus = unsafe extern "C" fn(*mut *mut c_void, u8, *mut *mut c_void) -> u64;
type KrunVmmBuilderRamMib = unsafe extern "C" fn(*mut *mut c_void, u32, *mut *mut c_void) -> u64;
type KrunVmmBuilderNestedVirt = unsafe extern "C" fn(*mut *mut c_void, bool);
type KrunVmmBuilderPayload = unsafe extern "C" fn(*mut *mut c_void, *mut c_void);
type KrunVmmBuilderDevices = unsafe extern "C" fn(*mut *mut c_void, *mut c_void);
type KrunVmmBuilderSetProfilePath =
    unsafe extern "C" fn(*mut *mut c_void, KrunStr, *mut *mut c_void) -> u64;
type KrunVmmBuilderDestroy = unsafe extern "C" fn(*mut c_void);
type KrunVmmBuilderBuild = unsafe extern "C" fn(*mut *mut c_void, *mut *mut c_void) -> *mut c_void;
type KrunVmmRun = unsafe extern "C" fn(*mut c_void);
type KrunErrorCode = unsafe extern "C" fn(*mut c_void) -> u32;
type KrunErrorMessage = unsafe extern "C" fn(*mut c_void, *mut KrunVtableHandle);
type KrunErrorDestroy = unsafe extern "C" fn(*mut c_void);

pub(crate) struct DynamicLibkrunApi {
    libkrun: *mut c_void,
    init: *mut c_void,
    init_log: KrunInitLog,
    check_nested_virt: KrunCheckNestedVirt,
    payload_load_krunfw: KrunPayloadLoadKrunfw,
    payload_append_cmdline: KrunPayloadAppendCmdline,
    fs_overlay_new: KrunFsOverlayNew,
    init_config_builder: KrunInitConfigBuilder,
    init_builder_args: KrunInitBuilderArgs,
    init_builder_env: KrunInitBuilderEnv,
    init_builder_workdir: KrunInitBuilderWorkdir,
    init_builder_rlimits: KrunInitBuilderRlimits,
    init_builder_dhcp: KrunInitBuilderDhcp,
    init_builder_build: KrunInitBuilderBuild,
    init_config_apply_in: KrunInitConfigApplyIn,
    mmio_device_manager_new: KrunMmioDeviceManagerNew,
    mmio_device_manager_add: KrunMmioDeviceManagerAdd,
    fs_device_new: KrunFsDeviceNew,
    fs_device_new_read_only: KrunFsDeviceNew,
    fs_device_set_overlay: KrunFsDeviceSetOverlay,
    block_device_new: KrunBlockDeviceNew,
    block_device_set_read_only: KrunBlockDeviceSetReadOnly,
    net_device_new_unixstream_fd: KrunNetDeviceNewUnixstreamFd,
    vsock_device_new: KrunVsockDeviceNew,
    vsock_device_add_unix_port: KrunVsockDeviceAddUnixPort,
    vsock_device_add_port_forward: KrunVsockDeviceAddPortForward,
    console_device_builder: KrunConsoleDeviceBuilder,
    console_builder_add_default_console: KrunConsoleBuilderAddDefaultConsole,
    console_builder_add_inout_port: KrunConsoleBuilderAddInoutPort,
    console_builder_build: KrunConsoleBuilderBuild,
    display_backend_new: KrunDisplayBackendNew,
    gpu_device_new: KrunGpuDeviceNew,
    gpu_device_shm_size: KrunGpuDeviceShmSize,
    gpu_device_set_render_server_fd: Option<KrunGpuDeviceSetRenderServerFd>,
    vmm_builder_new: KrunVmmBuilderNew,
    vmm_builder_vcpus: KrunVmmBuilderVcpus,
    vmm_builder_ram_mib: KrunVmmBuilderRamMib,
    vmm_builder_nested_virt: KrunVmmBuilderNestedVirt,
    vmm_builder_payload: KrunVmmBuilderPayload,
    vmm_builder_devices: KrunVmmBuilderDevices,
    vmm_builder_set_profile_path: Option<KrunVmmBuilderSetProfilePath>,
    vmm_builder_destroy: KrunVmmBuilderDestroy,
    vmm_builder_build: KrunVmmBuilderBuild,
    vmm_run: KrunVmmRun,
    error_code: KrunErrorCode,
    error_message: KrunErrorMessage,
    error_destroy: KrunErrorDestroy,
}

impl DynamicLibkrunApi {
    pub(crate) fn open_default() -> Result<Self> {
        let override_path = explicit_libkrun_library_override()?;
        preload_libva();
        let (libkrun, _) = open_first(libkrun_candidates(override_path.as_deref())).with_context(
            || match &override_path {
                Some(path) => format!(
                    "{LIBKRUN_LIBRARY_ENV} points to '{path}', but that libkrun library could not be loaded"
                ),
                None => "failed to load libkrun".to_owned(),
            },
        )?;
        let (init, init_name) = match open_first(init_candidates(override_path.as_deref())) {
            Ok(pair) => pair,
            Err(err) => {
                // ABI 2 injects the guest init from a separate library, so a
                // libkrun without it cannot boot. Close the handle we already
                // opened before reporting the pair as unusable.
                // SAFETY: `libkrun` came from dlopen above and is not used again.
                unsafe { libc::dlclose(libkrun) };
                return Err(err).with_context(|| {
                    format!(
                        "failed to load the libkrun guest-init library ({}): ABI 2 injects the guest init from a separate library, so cang cannot boot without it",
                        DEFAULT_INIT_NAMES.join("/")
                    )
                });
            }
        };
        // SAFETY: both handles are open dlopen handles, and every symbol below is
        // either required from its library or resolved as an optional fork
        // extension.
        unsafe { Self::bind(libkrun, init, &init_name) }
    }

    unsafe fn bind(libkrun: *mut c_void, init: *mut c_void, init_name: &str) -> Result<Self> {
        // SAFETY: every symbol is resolved from the library that exports it and
        // transmuted to a signature taken from the generated `libkrun.h` /
        // `libkrun_init.h` declarations.
        let api = unsafe {
            Self {
                libkrun,
                init,
                init_log: transmute_symbol(load_symbol(libkrun, "krun_init_log")?),
                check_nested_virt: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_check_nested_virt",
                )?),
                payload_load_krunfw: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_payload_load_krunfw",
                )?),
                payload_append_cmdline: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_payload_append_cmdline",
                )?),
                fs_overlay_new: transmute_symbol(load_symbol(libkrun, "krun_fs_overlay_new")?),
                init_config_builder: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_config_builder",
                )?),
                init_builder_args: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_args",
                )?),
                init_builder_env: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_env",
                )?),
                init_builder_workdir: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_workdir",
                )?),
                init_builder_rlimits: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_rlimits",
                )?),
                init_builder_dhcp: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_dhcp",
                )?),
                init_builder_build: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_builder_build",
                )?),
                init_config_apply_in: transmute_symbol(load_symbol_from(
                    init,
                    init_name,
                    "krun_init_config_apply_in",
                )?),
                mmio_device_manager_new: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_mmio_device_manager_new",
                )?),
                mmio_device_manager_add: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_mmio_device_manager_add",
                )?),
                fs_device_new: transmute_symbol(load_symbol(libkrun, "krun_fs_device_new")?),
                fs_device_new_read_only: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_fs_device_new_read_only",
                )?),
                fs_device_set_overlay: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_fs_device_set_overlay",
                )?),
                block_device_new: transmute_symbol(load_symbol(libkrun, "krun_block_device_new")?),
                block_device_set_read_only: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_block_device_set_read_only",
                )?),
                net_device_new_unixstream_fd: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_net_device_new_unixstream_fd",
                )?),
                vsock_device_new: transmute_symbol(load_symbol(libkrun, "krun_vsock_device_new")?),
                vsock_device_add_unix_port: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vsock_device_add_unix_port",
                )?),
                vsock_device_add_port_forward: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vsock_device_add_port_forward",
                )?),
                console_device_builder: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_console_device_builder",
                )?),
                console_builder_add_default_console: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_console_builder_add_default_console",
                )?),
                console_builder_add_inout_port: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_console_builder_add_inout_port",
                )?),
                console_builder_build: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_console_builder_build",
                )?),
                display_backend_new: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_display_backend_new",
                )?),
                gpu_device_new: transmute_symbol(load_symbol(libkrun, "krun_gpu_device_new")?),
                gpu_device_shm_size: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_gpu_device_shm_size",
                )?),
                gpu_device_set_render_server_fd: load_optional_symbol(
                    libkrun,
                    "krun_gpu_device_set_render_server_fd",
                )
                .map(|symbol| transmute_symbol(symbol)),
                vmm_builder_new: transmute_symbol(load_symbol(libkrun, "krun_vmm_builder_new")?),
                vmm_builder_vcpus: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_vcpus",
                )?),
                vmm_builder_ram_mib: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_ram_mib",
                )?),
                vmm_builder_nested_virt: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_nested_virt",
                )?),
                vmm_builder_payload: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_payload",
                )?),
                vmm_builder_devices: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_devices",
                )?),
                vmm_builder_set_profile_path: load_optional_symbol(
                    libkrun,
                    "krun_vmm_builder_set_profile_path",
                )
                .map(|symbol| transmute_symbol(symbol)),
                vmm_builder_destroy: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_destroy",
                )?),
                vmm_builder_build: transmute_symbol(load_symbol(
                    libkrun,
                    "krun_vmm_builder_build",
                )?),
                vmm_run: transmute_symbol(load_symbol(libkrun, "krun_vmm_run")?),
                error_code: transmute_symbol(load_symbol(libkrun, "krun_error_code")?),
                error_message: transmute_symbol(load_symbol(libkrun, "krun_error_message")?),
                error_destroy: transmute_symbol(load_symbol(libkrun, "krun_error_destroy")?),
            }
        };
        Ok(api)
    }

    /// Consume a `KrunError*` out-parameter: format it, free it, and return it as
    /// an `anyhow` error.
    fn take_error(&self, err: *mut c_void) -> anyhow::Error {
        if err.is_null() {
            return anyhow!("libkrun reported an error without an error object");
        }
        let mut message = String::new();
        let vtable = KrunPushStrVtable {
            drop: None,
            push: Some(push_str_into_string),
        };
        let mut handle = KrunVtableHandle {
            type_tag: KRUN_PUSH_STR_TYPE_TAG,
            metadata: 0,
            vtable_ptr: &vtable as *const KrunPushStrVtable as *const c_void,
            user_data: &mut message as *mut String as *const c_void,
            vtable_size: std::mem::size_of::<KrunPushStrVtable>() as u16,
        };
        // SAFETY: `err` came from a libkrun out-parameter and the writer handle is
        // a live stack value for the duration of the call.
        let code = unsafe {
            (self.error_message)(err, &mut handle);
            (self.error_code)(err)
        };
        // SAFETY: the error object is owned by the caller of the failing function.
        unsafe { (self.error_destroy)(err) };
        if message.is_empty() {
            anyhow!("libkrun error {code}")
        } else {
            anyhow!("libkrun error {code}: {message}")
        }
    }

    fn check_result(&self, name: &str, rc: u64, err: *mut c_void) -> Result<()> {
        if !err.is_null() {
            return Err(self.take_error(err)).with_context(|| format!("{name} failed"));
        }
        if rc != KRUN_RESULT_SUCCESS {
            bail!("{name} returned result {rc}");
        }
        Ok(())
    }

    fn checked_handle(&self, name: &str, handle: *mut c_void, err: *mut c_void) -> Result<Handle> {
        if !err.is_null() {
            return Err(self.take_error(err)).with_context(|| format!("{name} failed"));
        }
        if handle.is_null() {
            bail!("{name} returned a null handle");
        }
        Ok(handle as usize)
    }
}

impl Drop for DynamicLibkrunApi {
    fn drop(&mut self) {
        // SAFETY: both handles were returned by dlopen and are owned by this struct.
        unsafe {
            if !self.init.is_null() {
                libc::dlclose(self.init);
            }
            if !self.libkrun.is_null() {
                libc::dlclose(self.libkrun);
            }
        }
    }
}

/// Push a `KrunStr` into the `String` the vtable handle carries as userdata.
unsafe extern "C" fn push_str_into_string(user_data: *mut c_void, value: KrunStr) -> bool {
    if user_data.is_null() {
        return false;
    }
    // SAFETY: user_data is the `String` installed by `take_error`, alive for the call.
    let buffer = unsafe { &mut *(user_data as *mut String) };
    if !value.data.is_null() && value.len > 0 {
        // SAFETY: the caller of `krun_error_message` guarantees a readable range.
        let bytes = unsafe { std::slice::from_raw_parts(value.data as *const u8, value.len) };
        buffer.push_str(&String::from_utf8_lossy(bytes));
    }
    true
}

unsafe extern "C" fn display_disable_scanout(_instance: *mut c_void, _scanout_id: u32) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_configure_scanout(
    _instance: *mut c_void,
    _scanout_id: u32,
    _display_width: u32,
    _display_height: u32,
    _width: u32,
    _height: u32,
    _format: u32,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_alloc_frame(
    _instance: *mut c_void,
    _scanout_id: u32,
    _buffer: *mut *mut u8,
    _buffer_size: *mut usize,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

unsafe extern "C" fn display_present_frame(
    _instance: *mut c_void,
    _scanout_id: u32,
    _frame_id: u32,
    _damage_area: *const c_void,
) -> i32 {
    KRUN_DISPLAY_ERR_METHOD_UNSUPPORTED
}

/// The `krun_display_basic_framebuffer_vtable` callbacks.
#[repr(C)]
struct KrunDisplayBasicFramebufferVtable {
    destroy: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    disable_scanout: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    configure_scanout:
        Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, u32) -> i32>,
    alloc_frame: Option<unsafe extern "C" fn(*mut c_void, u32, *mut *mut u8, *mut usize) -> i32>,
    present_frame: Option<unsafe extern "C" fn(*mut c_void, u32, u32, *const c_void) -> i32>,
}

/// `struct krun_display_backend`: the display backend a GPU device is built on.
#[repr(C)]
struct KrunDisplayBackendConfig {
    features: u64,
    create_userdata: *const c_void,
    create: Option<unsafe extern "C" fn(*mut *mut c_void, *const c_void, *const c_void) -> i32>,
    vtable: KrunDisplayBasicFramebufferVtable,
}

/// cang drives the GPU headless: venus renders through the sandboxed render
/// server and nothing ever scans out, but ABI 2 still requires a display
/// backend handle for `krun_gpu_device_new`, and libkrun rejects a backend
/// without the mandatory `BASIC_FRAMEBUFFER` methods. Advertise the feature with
/// methods that refuse every request, so a scanout that nobody sets up stays an
/// explicit error instead of a null call.
fn headless_display_backend() -> KrunDisplayBackendConfig {
    KrunDisplayBackendConfig {
        features: KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER,
        create_userdata: std::ptr::null(),
        create: None,
        vtable: KrunDisplayBasicFramebufferVtable {
            destroy: None,
            disable_scanout: Some(display_disable_scanout),
            configure_scanout: Some(display_configure_scanout),
            alloc_frame: Some(display_alloc_frame),
            present_frame: Some(display_present_frame),
        },
    }
}

#[cfg(test)]
pub(crate) fn required_symbol_presence_for_test(
    name: &'static str,
    symbol: Option<*mut c_void>,
) -> Result<bool> {
    let symbol = symbol
        .ok_or_else(|| anyhow!("failed to resolve libkrun symbol {name}: symbol is unavailable"))?;
    Ok(!symbol.is_null())
}

fn explicit_libkrun_library_override() -> Result<Option<String>> {
    match std::env::var(LIBKRUN_LIBRARY_ENV) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("{LIBKRUN_LIBRARY_ENV} must be valid UTF-8")
        }
    }
}

/// Pre-register libva.so.2 and libva-drm.so.2 globally before the libkrun
/// dlopen.  The VM worker's dlopen of libkrun (RTLD_NOW) triggers a chain
/// libkrun -> libvirglrenderer -> libva.so.2, and libva.so.2 has an undefined
/// symbol vaGetDisplayDRM that only libva-drm.so.2 provides.  When both are
/// loaded together as DT_NEEDED of libvirglrenderer, the RTLD_NOW per-object
/// resolution processes libva.so.2 before libva-drm.so.2, which fails.
/// Pre-loading both globally (libva-drm first) makes the symbols available in
/// the lookup scope so the libkrun dlopen finds the already-loaded copy.
fn preload_libva() {
    for soname in ["libva-drm.so.2", "libva.so.2"] {
        let Ok(c_soname) = CString::new(soname) else {
            continue;
        };
        // SAFETY: dlopen with a NUL-terminated soname; relies on LD_LIBRARY_PATH.
        let handle = unsafe { libc::dlopen(c_soname.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            tracing::warn!(
                "libkrun dlopen pre-register could not load {soname}: {}",
                dlerror_string()
            );
        }
    }
}

fn open_first(candidates: Vec<String>) -> Result<(*mut c_void, String)> {
    let mut last_error = None;
    for name in candidates {
        let Ok(c_name) = CString::new(name.as_str()) else {
            continue;
        };
        // SAFETY: dlopen is called with a NUL-terminated library name and RTLD_NOW.
        let handle = unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW) };
        if !handle.is_null() {
            return Ok((handle, name));
        }
        last_error = Some(anyhow!("failed to load {name}: {}", dlerror_string()));
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no library candidate to load")))
}

fn libkrun_candidates(override_value: Option<&str>) -> Vec<String> {
    if let Some(value) = override_value
        && !value.trim().is_empty()
    {
        return vec![value.to_owned()];
    }
    planned_default_libkrun_load_order(std::env::current_exe().ok())
}

fn init_candidates(override_value: Option<&str>) -> Vec<String> {
    planned_libkrun_init_load_order_for_exe(override_value, std::env::current_exe().ok())
}

fn planned_library_load_order(names: &[&str], current_exe: Option<PathBuf>) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(exe) = current_exe
        && let Some(root) = package_root_from_exe(&exe)
    {
        candidates.extend(
            names
                .iter()
                .map(|name| root.join("lib/cang").join(name).display().to_string()),
        );
    }
    candidates.extend(names.iter().map(|name| (*name).to_owned()));
    candidates
}

fn planned_default_libkrun_load_order(current_exe: Option<PathBuf>) -> Vec<String> {
    planned_library_load_order(&DEFAULT_LIBKRUN_NAMES, current_exe)
}

#[cfg(test)]
pub(crate) fn planned_libkrun_load_order(override_value: Option<&str>) -> Vec<String> {
    if let Some(value) = override_value
        && !value.trim().is_empty()
    {
        return vec![value.to_owned()];
    }
    planned_default_libkrun_load_order(std::env::current_exe().ok())
}

#[cfg(test)]
pub(crate) fn planned_libkrun_load_order_for_exe(
    override_value: Option<&str>,
    current_exe: Option<PathBuf>,
) -> Vec<String> {
    if let Some(value) = override_value
        && !value.trim().is_empty()
    {
        return vec![value.to_owned()];
    }
    planned_default_libkrun_load_order(current_exe)
}

/// The guest-init library is looked up next to an explicit libkrun override
/// first, then by soname, so a locally built pair (or a test override) can be
/// pointed at as a directory pair.
pub(crate) fn planned_libkrun_init_load_order_for_exe(
    override_value: Option<&str>,
    current_exe: Option<PathBuf>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(value) = override_value
        && !value.trim().is_empty()
        && let Some(dir) = Path::new(value).parent()
        && !dir.as_os_str().is_empty()
    {
        candidates.extend(
            DEFAULT_INIT_NAMES
                .iter()
                .map(|name| dir.join(name).display().to_string()),
        );
    }
    candidates.extend(planned_library_load_order(&DEFAULT_INIT_NAMES, current_exe));
    candidates
}

fn load_symbol_from(handle: *mut c_void, library: &str, name: &str) -> Result<*mut c_void> {
    // SAFETY: handle is an open dlopen handle.
    unsafe { load_symbol(handle, name) }
        .with_context(|| format!("{library} does not export {name}"))
}

unsafe fn transmute_symbol<T>(symbol: *mut c_void) -> T {
    // SAFETY: the caller verified the symbol comes from the library declaring it.
    unsafe { std::mem::transmute_copy::<*mut c_void, T>(&symbol) }
}

fn path_str<'a>(path: &'a Path, name: &str) -> Result<&'a str> {
    path.to_str()
        .ok_or_else(|| anyhow!("{name} '{}' must be valid UTF-8", path.display()))
}

fn krun_str(value: &str) -> KrunStr {
    KrunStr {
        data: value.as_ptr() as *const c_char,
        len: value.len(),
    }
}

unsafe fn load_optional_symbol(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
    let c_name = CString::new(name).ok()?;
    // SAFETY: handle is an open dlopen handle and c_name is NUL-terminated.
    let symbol = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
    if symbol.is_null() { None } else { Some(symbol) }
}

unsafe fn load_symbol(handle: *mut c_void, name: &str) -> Result<*mut c_void> {
    let c_name = CString::new(name)?;
    // SAFETY: handle is an open dlopen handle and c_name is NUL-terminated.
    let symbol = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
    if symbol.is_null() {
        bail!(
            "failed to resolve libkrun symbol {name}: {}",
            dlerror_string()
        );
    }
    Ok(symbol)
}

fn dlerror_string() -> String {
    // SAFETY: dlerror returns a thread-local NUL-terminated error string or NULL.
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        return "unknown dlerror".to_owned();
    }
    // SAFETY: non-null dlerror pointer is valid until the next dl* call.
    unsafe { std::ffi::CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned()
}

impl LibkrunApi for DynamicLibkrunApi {
    fn init_log(&mut self, level: u32) -> Result<()> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature; `err` is a live
        // out-parameter for the call.
        let rc = unsafe {
            (self.init_log)(
                CANG_LIBKRUN_LOG_TARGET_STDERR_FD,
                level,
                CANG_LIBKRUN_LOG_STYLE_NEVER,
                CANG_LIBKRUN_LOG_OPTION_NO_ENV,
                &mut err,
            )
        };
        self.check_result("krun_init_log", rc, err)
    }

    fn check_nested_virt(&mut self) -> Result<bool> {
        // SAFETY: resolved symbol with the declared signature.
        Ok(unsafe { (self.check_nested_virt)() })
    }

    fn payload_load_krunfw(&mut self) -> Result<Handle> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature.
        let handle = unsafe { (self.payload_load_krunfw)(&mut err) };
        self.checked_handle("krun_payload_load_krunfw", handle, err)
    }

    fn payload_append_cmdline(&mut self, payload: Handle, fragment: &str) -> Result<()> {
        // SAFETY: resolved symbol with the declared signature; the fragment is
        // borrowed by the callee only for the duration of the call.
        unsafe {
            (self.payload_append_cmdline)(handle_ptr(payload), krun_str(fragment));
        }
        Ok(())
    }

    fn fs_overlay_new(&mut self) -> Result<Handle> {
        // SAFETY: resolved symbol with the declared signature.
        let overlay = unsafe { (self.fs_overlay_new)() };
        if overlay.is_null() {
            bail!("krun_fs_overlay_new returned a null handle");
        }
        Ok(overlay as usize)
    }

    fn init_config_builder(&mut self) -> Result<Handle> {
        // SAFETY: resolved symbol with the declared signature.
        let builder = unsafe { (self.init_config_builder)() };
        if builder.is_null() {
            bail!("krun_init_config_builder returned a null handle");
        }
        Ok(builder as usize)
    }

    fn init_builder_args(&mut self, builder: Handle, args: &[String]) -> Result<Handle> {
        let owned = strings_as_cstrings(args, "argument")?;
        let argv = owned.iter().map(krun_str_of).collect::<Vec<_>>();
        let call = self.init_builder_args;
        self.mutate_init_builder("krun_init_builder_args", builder, |handle| {
            // SAFETY: resolved symbol with the declared signature; `owned`/`argv`
            // live for the duration of the call.
            unsafe { call(handle, argv.as_ptr(), argv.len()) };
        })
    }

    fn init_builder_env(&mut self, builder: Handle, env: &[(String, String)]) -> Result<Handle> {
        let entries = env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        let owned = strings_as_cstrings(&entries, "environment variable")?;
        let vars = owned.iter().map(krun_str_of).collect::<Vec<_>>();
        let call = self.init_builder_env;
        self.mutate_init_builder("krun_init_builder_env", builder, |handle| {
            // SAFETY: resolved symbol with the declared signature; `owned`/`vars`
            // live for the duration of the call.
            unsafe { call(handle, vars.as_ptr(), vars.len()) };
        })
    }

    fn init_builder_workdir(&mut self, builder: Handle, workdir: &str) -> Result<Handle> {
        let call = self.init_builder_workdir;
        self.mutate_init_builder("krun_init_builder_workdir", builder, |handle| {
            // SAFETY: resolved symbol with the declared signature.
            unsafe { call(handle, krun_str(workdir)) };
        })
    }

    fn init_builder_rlimits(&mut self, builder: Handle, rlimits: &[String]) -> Result<Handle> {
        let owned = strings_as_cstrings(rlimits, "resource limit")?;
        let limits = owned.iter().map(krun_str_of).collect::<Vec<_>>();
        let call = self.init_builder_rlimits;
        self.mutate_init_builder("krun_init_builder_rlimits", builder, |handle| {
            // SAFETY: resolved symbol with the declared signature; `owned`/`limits`
            // live for the duration of the call.
            unsafe { call(handle, limits.as_ptr(), limits.len()) };
        })
    }

    fn init_builder_dhcp(&mut self, builder: Handle, enable: bool) -> Result<Handle> {
        let call = self.init_builder_dhcp;
        self.mutate_init_builder("krun_init_builder_dhcp", builder, |handle| {
            // SAFETY: resolved symbol with the declared signature.
            unsafe { call(handle, enable) };
        })
    }

    fn init_builder_build(&mut self, builder: Handle) -> Result<Handle> {
        let mut handle = handle_ptr(builder);
        // SAFETY: resolved symbol with the declared signature. `build` consumes the
        // builder, so the local handle is not used again.
        let config = unsafe { (self.init_builder_build)(&mut handle) };
        if config.is_null() {
            bail!("krun_init_builder_build returned a null handle");
        }
        Ok(config as usize)
    }

    fn init_config_apply_in(
        &mut self,
        config: Handle,
        overlay: Handle,
        payload: Handle,
    ) -> Result<()> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature. The init config
        // lives in `libkrun_init.so` but resolves the libkrun overlay/payload
        // symbols from the libkrun handle, so the VM keeps both alive.
        let rc = unsafe {
            (self.init_config_apply_in)(
                handle_ptr(config),
                self.libkrun,
                handle_ptr(overlay),
                handle_ptr(payload),
                &mut err,
            )
        };
        self.check_result("krun_init_config_apply_in", rc, err)
    }

    fn mmio_device_manager_new(&mut self) -> Result<Handle> {
        // SAFETY: resolved symbol with the declared signature.
        let devices = unsafe { (self.mmio_device_manager_new)() };
        if devices.is_null() {
            bail!("krun_mmio_device_manager_new returned a null handle");
        }
        Ok(devices as usize)
    }

    fn mmio_device_manager_add(&mut self, devices: Handle, device: Handle) -> Result<()> {
        // SAFETY: resolved symbol with the declared signature. The manager takes
        // ownership of the device handle.
        unsafe { (self.mmio_device_manager_add)(handle_ptr(devices), handle_ptr(device)) };
        Ok(())
    }

    fn fs_device_new(&mut self, tag: &str, host_path: &Path, read_only: bool) -> Result<Handle> {
        let host_path = path_str(host_path, "virtiofs host path")?;
        let mut err = std::ptr::null_mut();
        // Read-only is a separate constructor in ABI 2, not a setter.
        let (name, constructor) = if read_only {
            ("krun_fs_device_new_read_only", self.fs_device_new_read_only)
        } else {
            ("krun_fs_device_new", self.fs_device_new)
        };
        // SAFETY: resolved symbol with the declared signature; both KrunStr values
        // borrow live strings for the duration of the call.
        let handle = unsafe { constructor(krun_str(tag), krun_str(host_path), &mut err) };
        self.checked_handle(name, handle, err)
    }

    fn fs_device_set_overlay(&mut self, device: Handle, overlay: Handle) -> Result<()> {
        // SAFETY: resolved symbol with the declared signature.
        unsafe { (self.fs_device_set_overlay)(handle_ptr(device), handle_ptr(overlay)) };
        Ok(())
    }

    fn block_device_new(&mut self, id: &str, path: &Path, read_only: bool) -> Result<Handle> {
        let path = path_str(path, "disk image path")?;
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature.
        let handle = unsafe {
            (self.block_device_new)(krun_str(id), krun_str(path), CANG_DISK_FORMAT_RAW, &mut err)
        };
        let handle = self.checked_handle("krun_block_device_new", handle, err)?;
        // SAFETY: resolved symbol with the declared signature.
        unsafe { (self.block_device_set_read_only)(handle_ptr(handle), read_only) };
        Ok(handle)
    }

    fn net_device_new_unixstream_fd(
        &mut self,
        id: &str,
        fd: i32,
        mac: [u8; 6],
        features: u32,
        flags: u32,
    ) -> Result<Handle> {
        let mut err = std::ptr::null_mut();
        let mac = KrunBytes {
            data: mac.as_ptr(),
            len: mac.len(),
        };
        // SAFETY: resolved symbol with the declared signature. The callee takes
        // ownership of `fd` on success.
        let handle = unsafe {
            (self.net_device_new_unixstream_fd)(krun_str(id), fd, mac, features, flags, &mut err)
        };
        self.checked_handle("krun_net_device_new_unixstream_fd", handle, err)
    }

    fn vsock_device_new(&mut self, cid: u64, tsi_features: u32) -> Result<Handle> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature.
        let handle = unsafe { (self.vsock_device_new)(cid, tsi_features, &mut err) };
        self.checked_handle("krun_vsock_device_new", handle, err)
    }

    fn vsock_device_add_unix_port(
        &mut self,
        device: Handle,
        port: u32,
        path: &Path,
        listen: bool,
    ) -> Result<()> {
        let path = path_str(path, "vsock unix socket path")?;
        // SAFETY: resolved symbol with the declared signature.
        unsafe {
            (self.vsock_device_add_unix_port)(handle_ptr(device), port, krun_str(path), listen);
        }
        Ok(())
    }

    fn vsock_device_add_port_forward(&mut self, device: Handle, mapping: &str) -> Result<()> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature.
        let rc = unsafe {
            (self.vsock_device_add_port_forward)(handle_ptr(device), krun_str(mapping), &mut err)
        };
        self.check_result("krun_vsock_device_add_port_forward", rc, err)
    }

    fn console_device_builder(&mut self) -> Result<Handle> {
        // SAFETY: resolved symbol with the declared signature.
        let builder = unsafe { (self.console_device_builder)() };
        if builder.is_null() {
            bail!("krun_console_device_builder returned a null handle");
        }
        Ok(builder as usize)
    }

    fn console_builder_add_default_console(
        &mut self,
        builder: Handle,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<()> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature.
        let rc = unsafe {
            (self.console_builder_add_default_console)(
                handle_ptr(builder),
                input_fd,
                output_fd,
                err_fd,
                &mut err,
            )
        };
        self.check_result("krun_console_builder_add_default_console", rc, err)
    }

    fn console_builder_add_inout_port(
        &mut self,
        builder: Handle,
        name: &str,
        input_fd: Option<i32>,
        output_fd: Option<i32>,
    ) -> Result<()> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature. A `None` direction
        // is spellable in C as a negative fd.
        let rc = unsafe {
            (self.console_builder_add_inout_port)(
                handle_ptr(builder),
                krun_str(name),
                input_fd.unwrap_or(-1),
                output_fd.unwrap_or(-1),
                &mut err,
            )
        };
        self.check_result("krun_console_builder_add_inout_port", rc, err)
    }

    fn console_builder_build(&mut self, builder: Handle) -> Result<Handle> {
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature. `build` consumes the
        // builder, so the handle is not used again.
        let device = unsafe { (self.console_builder_build)(handle_ptr(builder), &mut err) };
        self.checked_handle("krun_console_builder_build", device, err)
    }

    fn gpu_device_new(
        &mut self,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<Handle> {
        let backend_config = headless_display_backend();
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature. The backend struct is
        // copied by the callee, so the stack value outliving the call is not needed.
        let backend = unsafe {
            (self.display_backend_new)(
                &backend_config as *const KrunDisplayBackendConfig as *const c_void,
                std::mem::size_of::<KrunDisplayBackendConfig>(),
                &mut err,
            )
        };
        let backend = self.checked_handle("krun_display_backend_new", backend, err)?;
        // SAFETY: resolved symbol with the declared signature; the callee takes
        // ownership of the backend handle.
        let mut device = unsafe { (self.gpu_device_new)(virgl_flags, handle_ptr(backend)) };
        if device.is_null() {
            bail!("krun_gpu_device_new returned a null handle");
        }
        let shm_size = usize::try_from(shm_size).context("GPU shm size does not fit usize")?;
        // SAFETY: resolved symbol with the declared signature; `shm_size` is a
        // pointer-to-handle builder call.
        unsafe { (self.gpu_device_shm_size)(&mut device, shm_size) };
        if render_server_fd >= 0 {
            let set_render_server_fd = self.gpu_device_set_render_server_fd.ok_or_else(|| {
                anyhow!(
                    "libkrun ABI 2 has no render-server fd entry point yet: the pinned library must be rebuilt with the fork's krun_gpu_device_set_render_server_fd (wayfinder ticket 10)"
                )
            })?;
            let mut err = std::ptr::null_mut();
            // SAFETY: resolved optional fork symbol with the declared signature.
            let rc = unsafe { set_render_server_fd(&mut device, render_server_fd, &mut err) };
            self.check_result("krun_gpu_device_set_render_server_fd", rc, err)?;
        }
        Ok(device as usize)
    }

    fn vmm_builder_new(&mut self) -> Result<Handle> {
        // SAFETY: resolved symbol with the declared signature.
        let builder = unsafe { (self.vmm_builder_new)() };
        if builder.is_null() {
            bail!("krun_vmm_builder_new returned a null handle");
        }
        Ok(builder as usize)
    }

    fn vmm_builder_vcpus(&mut self, builder: Handle, vcpus: u8) -> Result<Handle> {
        let call = self.vmm_builder_vcpus;
        self.mutate_vmm_builder("krun_vmm_builder_vcpus", builder, |handle, err| {
            // SAFETY: resolved symbol with the declared signature.
            unsafe { call(handle, vcpus, err) }
        })
    }

    fn vmm_builder_ram_mib(&mut self, builder: Handle, ram_mib: u32) -> Result<Handle> {
        let call = self.vmm_builder_ram_mib;
        self.mutate_vmm_builder("krun_vmm_builder_ram_mib", builder, |handle, err| {
            // SAFETY: resolved symbol with the declared signature.
            unsafe { call(handle, ram_mib, err) }
        })
    }

    fn vmm_builder_nested_virt(&mut self, builder: Handle, enabled: bool) -> Result<Handle> {
        let call = self.vmm_builder_nested_virt;
        self.mutate_vmm_builder("krun_vmm_builder_nested_virt", builder, |handle, _err| {
            // SAFETY: resolved symbol with the declared signature.
            unsafe { call(handle, enabled) };
            KRUN_RESULT_SUCCESS
        })
    }

    fn vmm_builder_payload(&mut self, builder: Handle, payload: Handle) -> Result<Handle> {
        let call = self.vmm_builder_payload;
        self.mutate_vmm_builder("krun_vmm_builder_payload", builder, |handle, _err| {
            // SAFETY: resolved symbol with the declared signature. The builder takes
            // ownership of the payload handle.
            unsafe { call(handle, handle_ptr(payload)) };
            KRUN_RESULT_SUCCESS
        })
    }

    fn vmm_builder_devices(&mut self, builder: Handle, devices: Handle) -> Result<Handle> {
        let call = self.vmm_builder_devices;
        self.mutate_vmm_builder("krun_vmm_builder_devices", builder, |handle, _err| {
            // SAFETY: resolved symbol with the declared signature. The builder takes
            // ownership of the device manager handle.
            unsafe { call(handle, handle_ptr(devices)) };
            KRUN_RESULT_SUCCESS
        })
    }

    fn vmm_builder_destroy(&mut self, builder: Handle) -> Result<()> {
        // SAFETY: resolved symbol with the declared signature; the handle was
        // returned by krun_vmm_builder_new and never passed to builder_build.
        unsafe { (self.vmm_builder_destroy)(handle_ptr(builder)) };
        Ok(())
    }

    fn vmm_builder_build(&mut self, builder: Handle) -> Result<Handle> {
        let mut handle = handle_ptr(builder);
        let mut err = std::ptr::null_mut();
        // SAFETY: resolved symbol with the declared signature; `build` consumes the
        // builder, so the local handle is not used again.
        let vmm = unsafe { (self.vmm_builder_build)(&mut handle, &mut err) };
        self.checked_handle("krun_vmm_builder_build", vmm, err)
    }

    fn vmm_run(&mut self, vmm: Handle) -> Result<()> {
        // SAFETY: resolved symbol with the declared signature. libkrun exits the
        // worker process from inside the guest's exit path, so a return is a stop.
        unsafe { (self.vmm_run)(handle_ptr(vmm)) };
        bail!("krun_vmm_run returned without the guest exiting")
    }

    fn set_profile_path(&mut self, builder: Handle, profile_path: &Path) -> Result<Handle> {
        let Some(set_profile_path) = self.vmm_builder_set_profile_path else {
            tracing::debug!(
                "libkrun has no krun_vmm_builder_set_profile_path symbol; launch profiling will not be recorded"
            );
            return Ok(builder);
        };
        let profile_path = path_str(profile_path, "profile path")?.to_owned();
        self.mutate_vmm_builder(
            "krun_vmm_builder_set_profile_path",
            builder,
            |handle, err| {
                // SAFETY: resolved optional fork symbol with the declared signature.
                unsafe { set_profile_path(handle, krun_str(&profile_path), err) }
            },
        )
    }
}

impl DynamicLibkrunApi {
    /// Call a libkrun `*Handle` builder method and return the handle the library
    /// left behind (the object may have been re-boxed).
    fn mutate_vmm_builder(
        &mut self,
        name: &str,
        builder: Handle,
        call: impl FnOnce(*mut *mut c_void, *mut *mut c_void) -> u64,
    ) -> Result<Handle> {
        let mut handle = handle_ptr(builder);
        let mut err = std::ptr::null_mut();
        let rc = call(&mut handle, &mut err);
        self.check_result(name, rc, err)?;
        if handle.is_null() {
            bail!("{name} left a null builder handle");
        }
        Ok(handle as usize)
    }

    fn mutate_init_builder(
        &mut self,
        name: &str,
        builder: Handle,
        call: impl FnOnce(*mut *mut c_void),
    ) -> Result<Handle> {
        let mut handle = handle_ptr(builder);
        call(&mut handle);
        if handle.is_null() {
            bail!("{name} left a null init builder handle");
        }
        Ok(handle as usize)
    }
}

fn handle_ptr(handle: Handle) -> *mut c_void {
    handle as *mut c_void
}

fn strings_as_cstrings(values: &[String], label: &str) -> Result<Vec<CString>> {
    values
        .iter()
        .map(|value| CString::new(value.as_str()).context(format!("{label} contains a NUL byte")))
        .collect()
}

fn krun_str_of(value: &CString) -> KrunStr {
    KrunStr {
        data: value.as_ptr(),
        len: value.as_bytes().len(),
    }
}
