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
//! package-relative `<exe-prefix>/lib/cang/dri`. A request whose file is absent
//! from every override directory falls through to the caller's original path, so
//! an unpatched package (no `lib/cang/dri`) behaves exactly as before.
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

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
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
/// # Safety
///
/// `filename` must be null or a NUL-terminated C string, as `dlopen(3)` requires.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void {
    let real = real_dlopen();
    if let Some(rewritten) = intercept_path(filename) {
        // SAFETY: `rewritten` is NUL-terminated.
        let handle = unsafe { real(rewritten.as_ptr(), flags) };
        if !handle.is_null() {
            return handle;
        }
    }
    // SAFETY: forwarding the caller's own arguments to libc's dlopen.
    unsafe { real(filename, flags) }
}

/// The replacement path for a driver request, if one exists.
fn intercept_path(filename: *const c_char) -> Option<CString> {
    if filename.is_null() {
        return None;
    }
    // SAFETY: dlopen filenames are NUL-terminated C strings.
    let requested = unsafe { CStr::from_ptr(filename) }.to_str().ok()?;
    let path = overridden_path(requested, &override_dirs())?;
    CString::new(path.as_os_str().as_encoded_bytes()).ok()
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
}
