//! Hand the VM worker cang's patched VA-API driver despite secure-execution mode.
//!
//! The VM worker is the `cang` binary re-exec'd through `unshare --keep-id`,
//! which leaves glibc in secure-execution mode. libva opens its driver with
//! `secure_getenv`, so `LIBVA_DRIVERS_PATH` is ignored there and the worker
//! always loads the *host's* system mesa - which on a virtio-gpu host is the
//! same `vl_rbsp_ue` reader that hangs the constant-QP encode (ticket 08).
//! Configuration cannot reach the driver: the path libva builds is
//! `<drivers-path>/<name>_drv_video.so` and libva opens it with `dlopen`.
//!
//! cang can still reach it, because the worker is cang's own executable: define
//! `dlopen` in the main program and the global symbol lookup that libva's PLT
//! call performs finds it first (the executable is ahead of libc in the default
//! search order). The interposer rewrites only a `*_drv_video.so` request and
//! forwards everything else to libc's real `dlopen`, resolved with
//! `dlsym(RTLD_NEXT, "dlopen")`.
//!
//! The override directory comes from `CANG_VA_DRIVER_PATH` - a plain `getenv`,
//! which secure-execution mode does not hide - and otherwise from the
//! package-relative `<exe-prefix>/lib/cang/dri`. The knob is deliberately
//! opt-in: cang does not carry mesa's ~1 GiB VA driver closure, so a host that
//! cannot apply cang's overlay points the worker at a driver it built itself.
//!
//! A rewrite only happens when that produces a *usable* VA driver. When the knob
//! is unset and the package has no `<prefix>/lib/cang/dri`, when the configured
//! directory holds no `<name>_drv_video.so`, when the candidate cannot be
//! `dlopen`ed, and when it loads but exports no `__vaDriverInit_*` entry point
//! (the symbol libva itself probes), the interposer forwards the caller's own
//! path unchanged. libva therefore falls back to the system driver and a bad
//! override cannot take the VA stack down; a rejected override is reported once
//! on stderr. The forwarding is what makes this safe to leave in place.
//!
//! The symbol has to reach `.dynsym`; `build.rs` adds
//! `-Wl,--export-dynamic-symbol=dlopen` to the `cang` bin target for that.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Directory (or `:`-separated directories) holding the VA driver cang prefers.
/// Read with ordinary `getenv`, deliberately, so secure-execution mode does not
/// hide it. Mirrors `LIBVA_DRIVERS_PATH`'s syntax.
pub(crate) const VA_DRIVER_PATH_ENV: &str = "CANG_VA_DRIVER_PATH";

/// libva asks for `<name>_drv_video.so`; only those requests are rewritten.
const DRIVER_SUFFIX: &str = "_drv_video.so";

/// Package-relative driver directory, next to `bin/cang`.
const PACKAGE_DRIVER_DIR: &str = "lib/cang/dri";

type Dlopen = unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void;

static REAL_DLOPEN: OnceLock<Dlopen> = OnceLock::new();

/// libva looks up `__vaDriverInit_<major>_<minor>` for every minor from its own
/// VA version down to 0 (`va_getDriverInitName` in libva's `va.c`), so a real
/// driver exports at least one symbol with this prefix. The interposer probes
/// the same prefix over a generous minor range before it hands a candidate back,
/// which is what rejects a valid ELF that is not a VA driver.
const VA_DRIVER_INIT_PREFIX: &str = "__vaDriverInit_1_";
const VA_DRIVER_INIT_MAX_MINOR: u32 = 64;

// libva 2.24.1 implements VA-API 1.23; the probe range must cover every minor a
// driver libva would accept can advertise.
const _: () = assert!(VA_DRIVER_INIT_MAX_MINOR >= 23);

/// Set once, so a rejected override is reported at most once per process.
static OVERRIDE_REJECTED_WARNED: OnceLock<()> = OnceLock::new();

/// Set once, so the selected override driver is reported at most once per
/// process - which is also what shows the knob reached the VM worker on each
/// launch path.
static OVERRIDE_USED_REPORTED: OnceLock<()> = OnceLock::new();

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

/// Force the interposer object into the binary even though nothing in Rust calls
/// it. `build.rs` exports the symbol; this is the reference that keeps it alive.
pub(crate) fn install() {
    let _ = dlopen as unsafe extern "C" fn(*const c_char, c_int) -> *mut c_void;
}

/// libc's own `dlopen`.
///
/// This function lives in the main executable, so `RTLD_NEXT` (which is
/// `(void *)-1`) resolves the next definition in the search order - libc's -
/// rather than recursing into ourselves.
fn real_dlopen() -> Dlopen {
    *REAL_DLOPEN.get_or_init(|| {
        // SAFETY: dlsym with RTLD_NEXT and a NUL-terminated symbol name.
        let symbol = unsafe { dlsym(-1isize as *mut c_void, c"dlopen".as_ptr()) };
        if symbol.is_null() {
            // Nothing to forward to: the process cannot open libraries at all.
            std::process::abort();
        }
        // SAFETY: `symbol` is libc's `dlopen`, whose signature matches `Dlopen`.
        unsafe { std::mem::transmute::<*mut c_void, Dlopen>(symbol) }
    })
}

/// `dlopen` with one request rewritten: a VA driver cang has built (or been
/// pointed at) opens from the override directory instead of the system one.
///
/// The rewrite is only used for a candidate that actually loads as a VA driver.
/// Otherwise the caller's own path is opened, so libva falls back to the system
/// driver rather than failing on whatever the override directory held.
///
/// # Safety
///
/// `filename` must be null or a NUL-terminated C string, as `dlopen(3)` requires.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void {
    let real = real_dlopen();
    if let Some(override_request) = driver_override(filename) {
        // SAFETY: `path` is NUL-terminated.
        let handle = unsafe { real(override_request.path.as_ptr(), flags) };
        if !handle.is_null() && is_va_driver(handle) {
            report_override_used(&override_request);
            return handle;
        }
        if handle.is_null() {
            warn_override_rejected(&override_request, "cannot be loaded");
        } else {
            // SAFETY: `handle` came from `real` above and is not used again.
            unsafe { dlclose(handle) };
            warn_override_rejected(&override_request, "exports no VA driver entry point");
        }
    }
    // SAFETY: forwarding the caller's own arguments to libc's dlopen.
    unsafe { real(filename, flags) }
}

/// A VA driver request the interposer wants to serve from cang's own directory.
struct DriverOverride {
    requested: String,
    candidate: PathBuf,
    path: CString,
}

/// The replacement path for a driver request, if an override directory holds a
/// candidate file for it.
fn driver_override(filename: *const c_char) -> Option<DriverOverride> {
    if filename.is_null() {
        return None;
    }
    // SAFETY: dlopen filenames are NUL-terminated C strings.
    let requested = unsafe { CStr::from_ptr(filename) }.to_str().ok()?;
    let candidate = overridden_path(requested, &override_dirs())?;
    let path = CString::new(candidate.as_os_str().as_encoded_bytes()).ok()?;
    Some(DriverOverride {
        requested: requested.to_owned(),
        candidate,
        path,
    })
}

/// The libva driver entry point symbol for a VA-API minor version, matching
/// `va_getDriverInitName()` in libva's `va.c`.
fn va_driver_init_symbol(minor: u32) -> String {
    format!("{VA_DRIVER_INIT_PREFIX}{minor}")
}

/// Whether a loaded object is a VA driver: it exports the
/// `__vaDriverInit_<major>_<minor>` entry point libva will look for. A candidate
/// that fails this is dropped, so libva's own search continues to the system
/// driver instead of failing on a library it cannot initialise.
fn is_va_driver(handle: *mut c_void) -> bool {
    for minor in 0..=VA_DRIVER_INIT_MAX_MINOR {
        let Ok(symbol) = CString::new(va_driver_init_symbol(minor)) else {
            continue;
        };
        // SAFETY: `handle` is a live dlopen handle and `symbol` is NUL-terminated.
        if !unsafe { dlsym(handle, symbol.as_ptr()) }.is_null() {
            return true;
        }
    }
    false
}

/// Report the override driver that was selected, once per process. Its presence
/// in the worker's stderr is what shows the knob reached the VM worker on a
/// given launch path.
fn report_override_used(request: &DriverOverride) {
    if OVERRIDE_USED_REPORTED.set(()).is_err() {
        return;
    }
    eprintln!(
        "cang: using VA driver {} for {}",
        request.candidate.display(),
        request.requested
    );
}

/// Report a configured but unusable override once per process, so an operator
/// who set the knob can tell it did not take effect.
fn warn_override_rejected(request: &DriverOverride, reason: &str) {
    if OVERRIDE_REJECTED_WARNED.set(()).is_err() {
        return;
    }
    eprintln!(
        "cang: {VA_DRIVER_PATH_ENV} candidate {} for {} {reason}; \
         falling back to the system VA driver",
        request.candidate.display(),
        request.requested
    );
}

/// Directories to search, most specific first.
fn override_dirs() -> Vec<PathBuf> {
    if let Ok(value) = std::env::var(VA_DRIVER_PATH_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return std::env::split_paths(&value)
                .filter(|path| !path.as_os_str().is_empty())
                .collect();
        }
    }
    package_driver_dir().into_iter().collect()
}

fn package_driver_dir() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    // `<prefix>/bin/cang` -> `<prefix>`
    let prefix = executable.parent()?.parent()?;
    let dir = prefix.join(PACKAGE_DRIVER_DIR);
    dir.is_dir().then_some(dir)
}

fn overridden_path(requested: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if !requested.ends_with(DRIVER_SUFFIX) {
        return None;
    }
    let base = requested.rsplit('/').next()?;
    if base.is_empty() {
        return None;
    }
    dirs.iter()
        .map(|dir| dir.join(base))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_dir_with(driver: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(driver), b"driver").expect("write driver");
        dir
    }

    #[test]
    fn a_driver_request_is_rewritten_to_the_override_directory() {
        let dir = fake_dir_with("virtio_gpu_drv_video.so");
        let dirs = vec![dir.path().to_path_buf()];
        assert_eq!(
            overridden_path("/run/opengl-driver/lib/dri/virtio_gpu_drv_video.so", &dirs),
            Some(dir.path().join("virtio_gpu_drv_video.so"))
        );
    }

    #[test]
    fn non_driver_requests_are_left_alone() {
        let dir = fake_dir_with("virtio_gpu_drv_video.so");
        let dirs = vec![dir.path().to_path_buf()];
        assert!(overridden_path("/nix/store/x/lib/libgallium-26.1.8.so", &dirs).is_none());
        assert!(overridden_path("/run/opengl-driver/lib/dri/i915_dri.so", &dirs).is_none());
        assert!(overridden_path("libkrunfw.so.5", &dirs).is_none());
    }

    #[test]
    fn a_driver_absent_from_the_override_directory_falls_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dirs = vec![dir.path().to_path_buf()];
        assert!(
            overridden_path("/run/opengl-driver/lib/dri/virtio_gpu_drv_video.so", &dirs).is_none()
        );
    }

    #[test]
    fn the_first_existing_override_directory_wins() {
        let empty = tempfile::tempdir().expect("tempdir");
        let populated = fake_dir_with("virtio_gpu_drv_video.so");
        let dirs = vec![empty.path().to_path_buf(), populated.path().to_path_buf()];
        assert_eq!(
            overridden_path("/run/opengl-driver/lib/dri/virtio_gpu_drv_video.so", &dirs),
            Some(populated.path().join("virtio_gpu_drv_video.so"))
        );
    }

    /// The documented escape hatch: renaming the knob, the package-relative
    /// default directory, or the libva request suffix would silently break the
    /// host recipe without this.
    #[test]
    fn the_knob_name_and_package_default_are_stable() {
        assert_eq!(VA_DRIVER_PATH_ENV, "CANG_VA_DRIVER_PATH");
        assert_eq!(PACKAGE_DRIVER_DIR, "lib/cang/dri");
        assert_eq!(DRIVER_SUFFIX, "_drv_video.so");
    }

    /// The interposer accepts a candidate only when it exports one of the
    /// `__vaDriverInit_*` symbols libva itself probes, so a valid ELF that is not
    /// a VA driver is rejected rather than handed to libva.
    #[test]
    fn the_driver_init_symbols_match_what_libva_probes() {
        assert_eq!(va_driver_init_symbol(0), "__vaDriverInit_1_0");
        assert_eq!(va_driver_init_symbol(23), "__vaDriverInit_1_23");
    }
}
