//! Point cargo at the build cache every guest already has.
//!
//! The image carries `sccache`, and cang exports `SCCACHE_DIR` onto the host's
//! shared cache directory, but nothing sets `RUSTC_WRAPPER` - so a `cargo build`
//! in a cang guest compiled straight through rustc and the cache sat unused
//! (verified: `sccache --show-stats` reported 0 compile requests after a build).
//! Every guest build therefore paid for compilations another guest had already
//! done.
//!
//! Two variables are set here, and only when sccache is actually on `PATH`, so an
//! image without it keeps the plain compiler:
//!
//! - `RUSTC_WRAPPER` to the resolved `sccache`, which routes every rustc
//!   invocation through the cache;
//! - `SCCACHE_IDLE_TIMEOUT=0`, so the cache server does not shut itself down
//!   after its default ten idle minutes and leave the next build starting it
//!   again.
//!
//! A variable that is already set wins: an image, or a caller passing
//! `RUSTC_WRAPPER`, keeps its own choice.

use std::env;
use std::path::{Path, PathBuf};

/// Names of the binaries the wrapper can be.
const SCCACHE_BINARY: &str = "sccache";

pub(in crate::guest_init) const RUSTC_WRAPPER_ENV: &str = "RUSTC_WRAPPER";
pub(in crate::guest_init) const SCCACHE_IDLE_TIMEOUT_ENV: &str = "SCCACHE_IDLE_TIMEOUT";

/// Resolves `sccache` on `search` and exports the two variables.
///
/// Called from the boot sequence with the guest's own `PATH`; returns the wrapper
/// it set so the caller can log it.
pub(in crate::guest_init) fn export_if_available() -> Option<PathBuf> {
    let search = env::var("PATH").unwrap_or_default();
    let wrapper = resolve_rustc_wrapper(
        env::var(RUSTC_WRAPPER_ENV).ok().as_deref(),
        &search,
        is_executable,
    )?;
    unsafe {
        env::set_var(RUSTC_WRAPPER_ENV, &wrapper);
        if env::var_os(SCCACHE_IDLE_TIMEOUT_ENV).is_none() {
            env::set_var(SCCACHE_IDLE_TIMEOUT_ENV, "0");
        }
    }
    Some(wrapper)
}

/// The `RUSTC_WRAPPER` the guest should use, or `None` to leave the compiler
/// alone.
///
/// Pure apart from `exists`, so the decisions can be tested without a PATH.
fn resolve_rustc_wrapper(
    configured: Option<&str>,
    search: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if configured.map(str::trim).is_some_and(|v| !v.is_empty()) {
        // An image that names its own wrapper (or a caller that does) keeps it.
        return None;
    }
    search
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(SCCACHE_BINARY))
        .find(|candidate| exists(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cang-sccache-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn touch_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, "#!/bin/sh\n").expect("write stub");
        let mut perms = fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).expect("chmod stub");
    }

    /// The measured failure: nothing set the wrapper, so a build never reached
    /// the cache cang had already pointed at the share.
    #[test]
    fn resolves_sccache_from_path_when_nothing_sets_the_wrapper() {
        let dir = temp_dir("resolve");
        touch_executable(&dir.join("sccache"));
        let found = resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable);
        assert_eq!(found, Some(dir.join("sccache")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// An image that names its own wrapper keeps it.
    #[test]
    fn an_existing_wrapper_wins() {
        assert_eq!(
            resolve_rustc_wrapper(Some("/nix/store/x/bin/sccache"), "/usr/bin", |_| true),
            None
        );
        // A blank value is not a wrapper; it falls through to resolution, and
        // nothing here resolves, so the compiler stays plain.
        assert_eq!(
            resolve_rustc_wrapper(Some("   "), "/usr/bin", |_| false),
            None
        );
    }

    /// An image without sccache keeps the plain compiler: a wrapper pointing at
    /// nothing would fail every rustc invocation.
    #[test]
    fn no_wrapper_without_sccache_on_path() {
        let dir = temp_dir("absent");
        assert_eq!(
            resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A directory on `PATH` holding a `sccache` that cannot be executed is not a
    /// wrapper either.
    #[test]
    fn a_non_executable_sccache_is_not_a_wrapper() {
        let dir = temp_dir("noexec");
        fs::write(dir.join("sccache"), "not a program").expect("write");
        assert_eq!(
            resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The first hit on `PATH` wins, which is what the guest's own resolution
    /// would do.
    #[test]
    fn the_first_match_on_path_wins() {
        let first = temp_dir("first");
        let second = temp_dir("second");
        touch_executable(&first.join("sccache"));
        touch_executable(&second.join("sccache"));
        let search = format!("{}:{}", first.display(), second.display());
        assert_eq!(
            resolve_rustc_wrapper(None, &search, is_executable),
            Some(first.join("sccache"))
        );
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
    }
}
