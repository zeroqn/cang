//! Making `libkrunfw.so.5` reachable for libkrun's own payload loader.
//!
//! libkrun opens the firmware by bare soname (`Payload::load_krunfw`), so the
//! dynamic loader resolves it through the caller's `DT_RUNPATH` /
//! `LD_LIBRARY_PATH`. Neither can be relied on where the load actually happens:
//! the VM worker is exec'd through `unshare --keep-id` into a user namespace with
//! a changed uid, which puts glibc in secure-execution mode, and glibc then
//! ignores both the `$ORIGIN` token in `DT_RUNPATH` and `LD_LIBRARY_PATH`. The
//! measured symptom is
//! `[ERROR krun::api::payload] could not load libkrunfw.so.5` on a
//! `--seccomp=off` boot, while the older binding - which reached `libkrun.so` by
//! *absolute* path - worked.
//!
//! So cang opens the firmware itself, by absolute path, before libkrun asks for
//! it: the package keeps it at `<exe-root>/lib/cang/libkrunfw.so.5`, and a
//! `dlopen` with `RTLD_GLOBAL` both loads it and publishes its SONAME in the
//! global scope, where libkrun's later bare-soname lookup finds the loaded object
//! instead of searching for it. A source build has no package-relative firmware;
//! there the soname lookup is all there is, and `Payload::load_krunfw` reports the
//! miss exactly as before.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

/// Explicit firmware path, mirroring what `CANG_LIBKRUN_LIBRARY` used to do for
/// the library itself: run a tree-built cang against a firmware that is not in a
/// package layout.
pub(crate) const FIRMWARE_LIBRARY_ENV: &str = "CANG_LIBKRUNFW_LIBRARY";

const FIRMWARE_NAMES: [&str; 2] = ["libkrunfw.so.5", "libkrunfw.so"];

/// Package directory holding the firmware, relative to the executable:
/// `<prefix>/bin/cang` -> `<prefix>/lib/cang`.
const PACKAGE_FIRMWARE_DIR: &str = "lib/cang";

/// Open the firmware before libkrun looks for it. Best effort: `None` means
/// nothing was preloaded, and libkrun's own lookup decides.
pub(crate) fn preload() -> Option<PathBuf> {
    let override_value = std::env::var(FIRMWARE_LIBRARY_ENV).ok();
    let executable = std::env::current_exe().ok();
    candidates(override_value.as_deref(), executable.as_deref())
        .into_iter()
        .find(|candidate| load(candidate))
}

/// Candidate firmware paths, in order: an explicit override, then the
/// package-relative names.
fn candidates(override_value: Option<&str>, executable: Option<&Path>) -> Vec<PathBuf> {
    if let Some(value) = override_value {
        let value = value.trim();
        if !value.is_empty() {
            return vec![PathBuf::from(value)];
        }
    }
    // `<prefix>/bin/cang` -> `<prefix>`
    let Some(prefix) = executable.and_then(Path::parent).and_then(Path::parent) else {
        return Vec::new();
    };
    let dir = prefix.join(PACKAGE_FIRMWARE_DIR);
    FIRMWARE_NAMES.iter().map(|name| dir.join(name)).collect()
}

fn load(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: `path` is NUL-terminated, and RTLD_GLOBAL is what keeps the library
    // loaded for the process and publishes its SONAME for libkrun's lookup.
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    !handle.is_null()
}

// From libc, which this crate already links.
const RTLD_NOW: c_int = libc::RTLD_NOW;
const RTLD_GLOBAL: c_int = libc::RTLD_GLOBAL;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_override_replaces_package_candidates() {
        let candidates = candidates(
            Some(" /tmp/libkrunfw-override.so.5 "),
            Some(Path::new("/nix/store/hash-cang/bin/cang")),
        );
        assert_eq!(
            candidates,
            vec![PathBuf::from("/tmp/libkrunfw-override.so.5")]
        );
    }

    #[test]
    fn package_candidates_follow_the_executable() {
        let candidates = candidates(None, Some(Path::new("/nix/store/hash-cang/bin/cang")));
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/nix/store/hash-cang/lib/cang/libkrunfw.so.5"),
                PathBuf::from("/nix/store/hash-cang/lib/cang/libkrunfw.so"),
            ]
        );
    }

    #[test]
    fn an_empty_override_falls_back_to_the_package() {
        let candidates = candidates(Some("   "), Some(Path::new("/opt/cang/bin/cang")));
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/opt/cang/lib/cang/libkrunfw.so.5"),
                PathBuf::from("/opt/cang/lib/cang/libkrunfw.so"),
            ]
        );
    }

    #[test]
    fn no_executable_path_means_no_candidates() {
        assert!(candidates(None, None).is_empty());
    }
}
