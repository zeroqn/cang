//! Dynamic libkrun loading and symbol binding.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use crate::runtime::host_tools::package_root_from_exe;

use super::api::LibkrunApi;

const LIBKRUN_LIBRARY_ENV: &str = "CANG_LIBKRUN_LIBRARY";
const DEFAULT_LIBKRUN_NAMES: [&str; 2] = ["libkrun.so.2", "libkrun.so"];
const CANG_LIBKRUN_LOG_TARGET_STDERR_FD: i32 = 2;
const CANG_LIBKRUN_LOG_STYLE_NEVER: u32 = 2;
const CANG_LIBKRUN_LOG_OPTION_NO_ENV: u32 = 1;
const CANG_NET_FEATURE_CSUM: u32 = 1 << 0;
const CANG_NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const CANG_NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const CANG_NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const CANG_NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const CANG_NET_FEATURE_HOST_UFO: u32 = 1 << 14;
const CANG_PASST_MAC: [u8; 6] = [0x02, 0x4c, 0x4f, 0x46, 0x54, 0x44];
pub(crate) const CANG_LIBKRUN_COMPAT_NET_FEATURES: u32 = CANG_NET_FEATURE_CSUM
    | CANG_NET_FEATURE_GUEST_CSUM
    | CANG_NET_FEATURE_GUEST_TSO4
    | CANG_NET_FEATURE_GUEST_UFO
    | CANG_NET_FEATURE_HOST_TSO4
    | CANG_NET_FEATURE_HOST_UFO;

type KrunSetLogLevel = unsafe extern "C" fn(u32) -> i32;
type KrunInitLog = unsafe extern "C" fn(i32, u32, u32, u32) -> i32;
type KrunCreateCtx = unsafe extern "C" fn() -> i32;
type KrunFreeCtx = unsafe extern "C" fn(u32) -> i32;
type KrunSetVmConfig = unsafe extern "C" fn(u32, u8, u32) -> i32;
type KrunSetGpuOptions3 = unsafe extern "C" fn(u32, u32, u64, i32) -> i32;
type KrunCheckNestedVirt = unsafe extern "C" fn() -> i32;
type KrunSetNestedVirt = unsafe extern "C" fn(u32, bool) -> i32;
type KrunSetRoot = unsafe extern "C" fn(u32, *const c_char) -> i32;
type KrunAddDisk = unsafe extern "C" fn(u32, *const c_char, *const c_char, bool) -> i32;
type KrunDisableImplicitConsole = unsafe extern "C" fn(u32) -> i32;
type KrunSetConsoleOutput = unsafe extern "C" fn(u32, *const c_char) -> i32;
type KrunAddVirtioConsoleDefault = unsafe extern "C" fn(u32, i32, i32, i32) -> i32;
type KrunAddNetUnixstream = unsafe extern "C" fn(u32, *const c_char, i32, *mut u8, u32, u32) -> i32;
type KrunAddVsockPort2 = unsafe extern "C" fn(u32, u32, *const c_char, bool) -> i32;
type KrunSetPortMap = unsafe extern "C" fn(u32, *const *const c_char) -> i32;
type KrunSetWorkdir = unsafe extern "C" fn(u32, *const c_char) -> i32;
type KrunSetExec =
    unsafe extern "C" fn(u32, *const c_char, *const *const c_char, *const *const c_char) -> i32;
type KrunSetRlimits = unsafe extern "C" fn(u32, *const *const c_char) -> i32;
type KrunSetProfilePath = unsafe extern "C" fn(u32, *const c_char) -> i32;
type KrunSetKernelCmdlineAppend = unsafe extern "C" fn(u32, *const c_char) -> i32;
type KrunStartEnter = unsafe extern "C" fn(u32) -> i32;

pub(crate) struct DynamicLibkrunApi {
    handle: *mut c_void,
    set_log_level: KrunSetLogLevel,
    init_log: Option<KrunInitLog>,
    create_ctx: KrunCreateCtx,
    free_ctx: KrunFreeCtx,
    set_vm_config: KrunSetVmConfig,
    set_gpu_options3: Option<KrunSetGpuOptions3>,
    check_nested_virt: Option<KrunCheckNestedVirt>,
    set_nested_virt: KrunSetNestedVirt,
    set_root: KrunSetRoot,
    add_disk: KrunAddDisk,
    disable_implicit_console: KrunDisableImplicitConsole,
    set_console_output: Option<KrunSetConsoleOutput>,
    add_virtio_console_default: KrunAddVirtioConsoleDefault,
    add_net_unixstream: Option<KrunAddNetUnixstream>,
    add_vsock_port2: Option<KrunAddVsockPort2>,
    set_port_map: Option<KrunSetPortMap>,
    set_workdir: KrunSetWorkdir,
    set_exec: KrunSetExec,
    set_rlimits: KrunSetRlimits,
    set_profile_path: Option<KrunSetProfilePath>,
    set_kernel_cmdline_append: Option<KrunSetKernelCmdlineAppend>,
    start_enter: KrunStartEnter,
}

impl DynamicLibkrunApi {
    pub(crate) fn open_default() -> Result<Self> {
        if let Some(path) = explicit_libkrun_library_override()? {
            return Self::open(&path).with_context(|| {
                format!(
                    "{LIBKRUN_LIBRARY_ENV} points to '{}', but that libkrun library could not be loaded",
                    path
                )
            });
        }

        let mut last_error = None;
        for name in default_libkrun_candidates() {
            match Self::open(&name) {
                Ok(api) => return Ok(api),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("failed to load libkrun")))
    }

    fn open(name: &str) -> Result<Self> {
        let c_name = CString::new(name)?;

        // Pre-register libva.so.2 and libva-drm.so.2 globally before the libkrun
        // dlopen.  The VM worker's dlopen of libkrun (RTLD_NOW) triggers a chain
        // libkrun -> libvirglrenderer -> libva.so.2, and libva.so.2 has an undefined
        // symbol vaGetDisplayDRM that only libva-drm.so.2 provides.  When both are
        // loaded together as DT_NEEDED of libvirglrenderer, the RTLD_NOW per-object
        // resolution processes libva.so.2 before libva-drm.so.2, which fails.
        // Pre-loading both globally (libva-drm first) makes the symbols available
        // in the lookup scope so the libkrun dlopen finds the already-loaded copy.
        for soname in ["libva-drm.so.2", "libva.so.2"] {
            let c_soname = CString::new(soname)?;
            // SAFETY: dlopen with a NUL-terminated soname; relies on LD_LIBRARY_PATH.
            let handle =
                unsafe { libc::dlopen(c_soname.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if handle.is_null() {
                tracing::warn!(
                    "libkrun dlopen pre-register could not load {soname}: {}",
                    dlerror_string()
                );
            }
        }

        // SAFETY: dlopen is called with a NUL-terminated library name and RTLD_NOW.
        let handle = unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            bail!("failed to load {name}: {}", dlerror_string());
        }

        let nested_virt_symbols = unsafe {
            // SAFETY: symbols are resolved from a successfully opened libkrun handle.
            resolve_nested_virt_symbols(|name| Ok(load_optional_symbol(handle, name)))?
        };

        let api = unsafe {
            // SAFETY: symbols are resolved from a successfully opened libkrun handle and
            // transmuted to signatures verified against the local libkrun 1.18 header.
            Self {
                handle,
                set_log_level: std::mem::transmute::<*mut c_void, KrunSetLogLevel>(load_symbol(
                    handle,
                    "krun_set_log_level",
                )?),
                init_log: load_optional_symbol(handle, "krun_init_log")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunInitLog>(symbol)),
                create_ctx: std::mem::transmute::<*mut c_void, KrunCreateCtx>(load_symbol(
                    handle,
                    "krun_create_ctx",
                )?),
                free_ctx: std::mem::transmute::<*mut c_void, KrunFreeCtx>(load_symbol(
                    handle,
                    "krun_free_ctx",
                )?),
                set_vm_config: std::mem::transmute::<*mut c_void, KrunSetVmConfig>(load_symbol(
                    handle,
                    "krun_set_vm_config",
                )?),
                set_gpu_options3: load_optional_symbol(handle, "krun_set_gpu_options3")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunSetGpuOptions3>(symbol)),
                check_nested_virt: nested_virt_symbols
                    .check
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunCheckNestedVirt>(symbol)),
                set_nested_virt: std::mem::transmute::<*mut c_void, KrunSetNestedVirt>(
                    nested_virt_symbols.set,
                ),
                set_root: std::mem::transmute::<*mut c_void, KrunSetRoot>(load_symbol(
                    handle,
                    "krun_set_root",
                )?),
                add_disk: std::mem::transmute::<*mut c_void, KrunAddDisk>(load_symbol(
                    handle,
                    "krun_add_disk",
                )?),
                disable_implicit_console: std::mem::transmute::<
                    *mut c_void,
                    KrunDisableImplicitConsole,
                >(load_symbol(
                    handle,
                    "krun_disable_implicit_console",
                )?),
                set_console_output: load_optional_symbol(handle, "krun_set_console_output")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunSetConsoleOutput>(symbol)),
                add_virtio_console_default: std::mem::transmute::<
                    *mut c_void,
                    KrunAddVirtioConsoleDefault,
                >(load_symbol(
                    handle,
                    "krun_add_virtio_console_default",
                )?),
                add_net_unixstream: load_optional_symbol(handle, "krun_add_net_unixstream")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunAddNetUnixstream>(symbol)),
                add_vsock_port2: load_optional_symbol(handle, "krun_add_vsock_port2")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunAddVsockPort2>(symbol)),
                set_port_map: load_optional_symbol(handle, "krun_set_port_map")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunSetPortMap>(symbol)),
                set_workdir: std::mem::transmute::<*mut c_void, KrunSetWorkdir>(load_symbol(
                    handle,
                    "krun_set_workdir",
                )?),
                set_exec: std::mem::transmute::<*mut c_void, KrunSetExec>(load_symbol(
                    handle,
                    "krun_set_exec",
                )?),
                set_rlimits: std::mem::transmute::<*mut c_void, KrunSetRlimits>(load_symbol(
                    handle,
                    "krun_set_rlimits",
                )?),
                set_profile_path: load_optional_symbol(handle, "krun_set_profile_path")
                    .map(|symbol| std::mem::transmute::<*mut c_void, KrunSetProfilePath>(symbol)),
                set_kernel_cmdline_append: load_optional_symbol(
                    handle,
                    "krun_set_kernel_cmdline_append",
                )
                .map(|symbol| {
                    std::mem::transmute::<*mut c_void, KrunSetKernelCmdlineAppend>(symbol)
                }),
                start_enter: std::mem::transmute::<*mut c_void, KrunStartEnter>(load_symbol(
                    handle,
                    "krun_start_enter",
                )?),
            }
        };
        Ok(api)
    }
}

struct NestedVirtSymbols {
    check: Option<*mut c_void>,
    set: *mut c_void,
}

fn resolve_nested_virt_symbols(
    mut resolve: impl FnMut(&str) -> Result<Option<*mut c_void>>,
) -> Result<NestedVirtSymbols> {
    let check = resolve("krun_check_nested_virt")?;
    let set = resolve("krun_set_nested_virt")?.ok_or_else(|| {
        anyhow!("failed to resolve libkrun symbol krun_set_nested_virt: symbol is unavailable")
    })?;
    Ok(NestedVirtSymbols { check, set })
}

#[cfg(test)]
pub(crate) fn nested_virt_symbol_presence_for_test(
    check: Option<*mut c_void>,
    set: Option<*mut c_void>,
) -> Result<(bool, bool)> {
    let symbols = resolve_nested_virt_symbols(|name| match name {
        "krun_check_nested_virt" => Ok(check),
        "krun_set_nested_virt" => Ok(set),
        _ => Ok(None),
    })?;
    Ok((symbols.check.is_some(), !symbols.set.is_null()))
}

#[cfg(test)]
pub(crate) fn required_rlimits_symbol_presence_for_test(
    set_rlimits: Option<*mut c_void>,
) -> Result<bool> {
    let symbol = set_rlimits.ok_or_else(|| {
        anyhow!("failed to resolve libkrun symbol krun_set_rlimits: symbol is unavailable")
    })?;
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

#[cfg(test)]
pub(crate) fn planned_libkrun_load_order(override_value: Option<&str>) -> Vec<String> {
    if let Some(value) = override_value
        && !value.trim().is_empty()
    {
        return vec![value.to_owned()];
    }
    planned_default_libkrun_load_order(std::env::current_exe().ok())
}

fn default_libkrun_candidates() -> Vec<String> {
    planned_default_libkrun_load_order(std::env::current_exe().ok())
}

fn planned_default_libkrun_load_order(current_exe: Option<PathBuf>) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(exe) = current_exe
        && let Some(root) = package_root_from_exe(&exe)
    {
        candidates.extend(
            DEFAULT_LIBKRUN_NAMES
                .iter()
                .map(|name| root.join("lib/cang").join(name).display().to_string()),
        );
    }
    candidates.extend(DEFAULT_LIBKRUN_NAMES.iter().map(|name| (*name).to_owned()));
    candidates
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

impl Drop for DynamicLibkrunApi {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: handle was returned by dlopen and is owned by this struct.
            unsafe { libc::dlclose(self.handle) };
        }
    }
}

impl LibkrunApi for DynamicLibkrunApi {
    fn create_ctx(&mut self) -> Result<u32> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        let rc = unsafe { (self.create_ctx)() };
        if rc < 0 {
            bail!("krun_create_ctx returned {rc}");
        }
        u32::try_from(rc).context("krun_create_ctx returned invalid context id")
    }

    fn free_ctx(&mut self, ctx_id: u32) -> Result<()> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        let rc = unsafe { (self.free_ctx)(ctx_id) };
        if rc < 0 {
            bail!("krun_free_ctx returned {rc}");
        }
        Ok(())
    }

    fn init_log(&mut self, level: u32) -> Result<i32> {
        if let Some(init_log) = self.init_log {
            // SAFETY: optional function pointer resolved from libkrun with upstream-compatible signature.
            return Ok(unsafe {
                init_log(
                    CANG_LIBKRUN_LOG_TARGET_STDERR_FD,
                    level,
                    CANG_LIBKRUN_LOG_STYLE_NEVER,
                    CANG_LIBKRUN_LOG_OPTION_NO_ENV,
                )
            });
        }
        // SAFETY: function pointer resolved from libkrun with verified signature.
        Ok(unsafe { (self.set_log_level)(level) })
    }

    fn set_vm_config(&mut self, ctx_id: u32, vcpus: u8, ram_mib: u32) -> Result<i32> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        Ok(unsafe { (self.set_vm_config)(ctx_id, vcpus, ram_mib) })
    }

    fn set_gpu_options3(
        &mut self,
        ctx_id: u32,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<Option<i32>> {
        Ok(self.set_gpu_options3.map(|set_gpu_options3| {
            // SAFETY: function pointer resolved from libkrun with verified signature.
            unsafe { set_gpu_options3(ctx_id, virgl_flags, shm_size, render_server_fd) }
        }))
    }

    fn check_nested_virt(&mut self) -> Result<Option<i32>> {
        let Some(check_nested_virt) = self.check_nested_virt else {
            return Ok(None);
        };
        // SAFETY: optional function pointer resolved from libkrun with upstream-compatible signature.
        Ok(Some(unsafe { check_nested_virt() }))
    }

    fn set_nested_virt(&mut self, ctx_id: u32, enabled: bool) -> Result<i32> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        Ok(unsafe { (self.set_nested_virt)(ctx_id, enabled) })
    }

    fn set_root(&mut self, ctx_id: u32, root_path: &Path) -> Result<i32> {
        let root_path = path_cstring(root_path)?;
        // SAFETY: C string lives for the duration of the call.
        Ok(unsafe { (self.set_root)(ctx_id, root_path.as_ptr()) })
    }

    fn add_disk(
        &mut self,
        ctx_id: u32,
        block_id: &str,
        disk_path: &Path,
        read_only: bool,
    ) -> Result<i32> {
        let block_id = CString::new(block_id)?;
        let disk_path = path_cstring(disk_path)?;
        // SAFETY: C strings live for the duration of the call.
        Ok(unsafe { (self.add_disk)(ctx_id, block_id.as_ptr(), disk_path.as_ptr(), read_only) })
    }

    fn disable_implicit_console(&mut self, ctx_id: u32) -> Result<i32> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        Ok(unsafe { (self.disable_implicit_console)(ctx_id) })
    }

    fn set_console_output(&mut self, ctx_id: u32, output_path: &Path) -> Result<i32> {
        let set_console_output = self.set_console_output.ok_or_else(|| {
            anyhow!("libkrun console capture failed: krun_set_console_output symbol is unavailable")
        })?;
        let output_path = path_cstring(output_path)?;
        // SAFETY: function pointer resolved from libkrun with verified signature and
        // the C string lives for the duration of the call.
        Ok(unsafe { set_console_output(ctx_id, output_path.as_ptr()) })
    }

    fn add_virtio_console_default(
        &mut self,
        ctx_id: u32,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<i32> {
        // SAFETY: function pointer resolved from libkrun with verified signature.
        Ok(unsafe { (self.add_virtio_console_default)(ctx_id, input_fd, output_fd, err_fd) })
    }

    fn add_net_unixstream(&mut self, ctx_id: u32, socket_fd: i32, flags: u32) -> Result<i32> {
        let add_net_unixstream = self.add_net_unixstream.ok_or_else(|| {
            anyhow!("libkrun passt setup failed: krun_add_net_unixstream symbol is unavailable")
        })?;
        let mut mac = CANG_PASST_MAC;
        // SAFETY: the MAC buffer lives for the duration of the call. c_path is NULL because
        // cang follows crun's socketpair/--fd passt wiring.
        Ok(unsafe {
            add_net_unixstream(
                ctx_id,
                std::ptr::null(),
                socket_fd,
                mac.as_mut_ptr(),
                CANG_LIBKRUN_COMPAT_NET_FEATURES,
                flags,
            )
        })
    }

    fn add_vsock_port(
        &mut self,
        ctx_id: u32,
        guest_port: u32,
        socket_path: &Path,
        listen: bool,
    ) -> Result<i32> {
        let add_vsock_port2 = self.add_vsock_port2.ok_or_else(|| {
            anyhow!(
                "libkrun managed attach setup failed: krun_add_vsock_port2 symbol is unavailable"
            )
        })?;
        let socket_path = path_cstring(socket_path)?;
        // SAFETY: C string lives for the duration of the call.
        Ok(unsafe { add_vsock_port2(ctx_id, guest_port, socket_path.as_ptr(), listen) })
    }

    fn set_port_map(&mut self, ctx_id: u32, port_map: &[String]) -> Result<i32> {
        let set_port_map = self.set_port_map.ok_or_else(|| {
            anyhow!("libkrun TSI publish setup failed: krun_set_port_map symbol is unavailable")
        })?;
        let port_map_strings = port_map
            .iter()
            .map(|spec| CString::new(spec.as_str()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let port_map = null_terminated_ptrs(&port_map_strings);
        // SAFETY: C strings and pointer array live for the duration of the call.
        Ok(unsafe { set_port_map(ctx_id, port_map.as_ptr()) })
    }

    fn set_workdir(&mut self, ctx_id: u32, workdir: &str) -> Result<i32> {
        let workdir = CString::new(workdir)?;
        // SAFETY: C string lives for the duration of the call.
        Ok(unsafe { (self.set_workdir)(ctx_id, workdir.as_ptr()) })
    }

    fn set_exec(
        &mut self,
        ctx_id: u32,
        exec_path: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<i32> {
        let exec_path = CString::new(exec_path)?;
        let argv_strings = argv
            .iter()
            .map(|arg| CString::new(arg.as_str()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let env_strings = env
            .iter()
            .map(|(key, value)| CString::new(format!("{key}={value}")))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let argv = null_terminated_ptrs(&argv_strings);
        let envp = null_terminated_ptrs(&env_strings);
        // SAFETY: C strings and pointer arrays live for the duration of the call.
        Ok(unsafe { (self.set_exec)(ctx_id, exec_path.as_ptr(), argv.as_ptr(), envp.as_ptr()) })
    }

    fn set_rlimits(&mut self, ctx_id: u32, rlimits: &[String]) -> Result<i32> {
        let rlimit_strings = rlimits
            .iter()
            .map(|limit| CString::new(limit.as_str()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let rlimits = null_terminated_ptrs(&rlimit_strings);
        // SAFETY: C strings and pointer array live for the duration of the call.
        Ok(unsafe { (self.set_rlimits)(ctx_id, rlimits.as_ptr()) })
    }

    fn set_profile_path(&mut self, ctx_id: u32, profile_path: &Path) -> Result<i32> {
        let Some(set_profile_path) = self.set_profile_path else {
            return Ok(0);
        };
        let profile_path = path_cstring(profile_path)?;
        // SAFETY: optional function pointer is resolved from libkrun when present, and the C
        // string lives for the duration of the call.
        Ok(unsafe { set_profile_path(ctx_id, profile_path.as_ptr()) })
    }

    fn set_kernel_cmdline_append(&mut self, ctx_id: u32, fragment: &str) -> Result<i32> {
        let Some(set_kernel_cmdline_append) = self.set_kernel_cmdline_append else {
            return Ok(0);
        };
        let fragment = CString::new(fragment)?;
        // SAFETY: optional function pointer is resolved from libkrun when present, and the C
        // string lives for the duration of the call.
        Ok(unsafe { set_kernel_cmdline_append(ctx_id, fragment.as_ptr()) })
    }

    fn start_enter(&mut self, ctx_id: u32) -> Result<i32> {
        // SAFETY: function pointer resolved from libkrun with verified signature. libkrun may
        // exit the current helper process after this call; parent cleanup runs outside it.
        Ok(unsafe { (self.start_enter)(ctx_id) })
    }
}

fn path_cstring(path: &Path) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).context("path contains interior NUL")
}

fn null_terminated_ptrs(values: &[CString]) -> Vec<*const c_char> {
    let mut ptrs = values
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    ptrs.push(std::ptr::null());
    ptrs
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
