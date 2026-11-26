//! Point cargo at the build cache every guest already has.
//!
//! The image carries `kache`, and its rustc wrapper is declared in the image's
//! `Env` - but cang filters an image's environment down to a short allowlist
//! (`runtime::launch::config::guest_env`), so that entry never reaches the guest
//! process. A `cargo build` in a cang guest therefore compiled straight through
//! rustc and the cache sat unused, which is the same measured failure the
//! `sccache` component was written for. Three variables are exported here, and
//! only when kache is actually on `PATH`, so an image without it keeps the plain
//! compiler:
//!
//! - `RUSTC_WRAPPER` to the resolved `kache`, which keys the whole invocation
//!   and restores its outputs from content-addressed blobs;
//! - `KACHE_CACHE_DIR=~/.cargo/kache`, the store: `~/.cargo` is the mount cang
//!   already grafts (`<state-root>/cargo`), so the cache needs no mount of its
//!   own, and a `--workspace-disk` launch has already bound that path to the
//!   tree's own `cargo/` on btrfs, where a restore can clone blocks;
//! - `KACHE_RUNTIME_DIR` (and `KACHE_MAX_SIZE`) because the daemon's socket,
//!   locks and session state are machine-local: leaving them under the store
//!   would put a unix socket on the share, and the budget keeps the store at
//!   the 60 GiB headroom the sccache component measured rather than kache's
//!   `5% of the disk, floored at 5 GiB` default.
//!
//! Nothing starts the daemon here: kache's client starts it on the first
//! cache-enabled compile, so a guest that never builds never runs it.
//!
//! A variable that is already set wins: an image, or a caller passing
//! `RUSTC_WRAPPER`, keeps its own choice.

use std::env;
use std::path::{Path, PathBuf};

/// Names of the binaries the wrapper can be.
const KACHE_BINARY: &str = "kache";

pub(in crate::guest_init) const RUSTC_WRAPPER_ENV: &str = "RUSTC_WRAPPER";
pub(in crate::guest_init) const KACHE_CACHE_DIR_ENV: &str = "KACHE_CACHE_DIR";
pub(in crate::guest_init) const KACHE_RUNTIME_DIR_ENV: &str = "KACHE_RUNTIME_DIR";
pub(in crate::guest_init) const KACHE_MAX_SIZE_ENV: &str = "KACHE_MAX_SIZE";
/// The cargo home cang mounts; kache's store goes inside it.
pub(in crate::guest_init) const KACHE_CACHE_DIR: &str = "/home/dev/.cargo/kache";
/// Sockets, locks, logs and session markers: the guest's own tmpfs, never the
/// store, which is a virtio-fs share on a default launch.
pub(in crate::guest_init) const KACHE_RUNTIME_DIR: &str = "/tmp/kache";
/// sccache was capped at 60 GiB after a measured 5.6 GiB fill; kache's default
/// budget is 5% of the store's disk floored at 5 GiB, which a Rust workspace
/// outgrows the same way.
pub(in crate::guest_init) const KACHE_MAX_SIZE: &str = "60GiB";

/// Resolves `kache` on `search` and exports the four variables.
///
/// Called from the boot sequence with the guest's own `PATH`; returns the
/// wrapper it set so the caller can log it.
pub(in crate::guest_init) fn export_if_available() -> Option<PathBuf> {
    let search = env::var("PATH").unwrap_or_default();
    let wrapper = resolve_rustc_wrapper(
        env::var(RUSTC_WRAPPER_ENV).ok().as_deref(),
        &search,
        is_executable,
    )?;
    unsafe {
        env::set_var(RUSTC_WRAPPER_ENV, &wrapper);
        for (key, value) in [
            (KACHE_CACHE_DIR_ENV, KACHE_CACHE_DIR),
            (KACHE_RUNTIME_DIR_ENV, KACHE_RUNTIME_DIR),
            (KACHE_MAX_SIZE_ENV, KACHE_MAX_SIZE),
        ] {
            if env::var_os(key).is_none() {
                env::set_var(key, value);
            }
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
        .map(|dir| Path::new(dir).join(KACHE_BINARY))
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
        let dir = std::env::temp_dir().join(format!("cang-kache-{}-{name}", std::process::id()));
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

    /// The measured failure: the image's `RUSTC_WRAPPER` is filtered out of the
    /// guest environment, so nothing routed a compile to the cache.
    #[test]
    fn resolves_kache_from_path_when_nothing_sets_the_wrapper() {
        let dir = temp_dir("resolve");
        touch_executable(&dir.join("kache"));
        let found = resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable);
        assert_eq!(found, Some(dir.join("kache")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// An image that names its own wrapper keeps it.
    #[test]
    fn an_existing_wrapper_wins() {
        assert_eq!(
            resolve_rustc_wrapper(Some("/nix/store/x/bin/kache"), "/usr/bin", |_| true),
            None
        );
        // A blank value is not a wrapper; it falls through to resolution, and
        // nothing here resolves, so the compiler stays plain.
        assert_eq!(
            resolve_rustc_wrapper(Some("   "), "/usr/bin", |_| false),
            None
        );
    }

    /// An image without kache keeps the plain compiler: a wrapper pointing at
    /// nothing would fail every rustc invocation.
    #[test]
    fn no_wrapper_without_kache_on_path() {
        let dir = temp_dir("absent");
        assert_eq!(
            resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A directory on `PATH` holding a `kache` that cannot be executed is not a
    /// wrapper either.
    #[test]
    fn a_non_executable_kache_is_not_a_wrapper() {
        let dir = temp_dir("noexec");
        fs::write(dir.join("kache"), "not a program").expect("write");
        assert_eq!(
            resolve_rustc_wrapper(None, dir.to_str().unwrap(), is_executable),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The store has to be the cargo home cang already mounts, and the daemon's
    /// runtime state has to stay off it.
    #[test]
    fn the_store_is_the_cargo_home_and_the_runtime_dir_is_not() {
        assert_eq!(KACHE_CACHE_DIR, "/home/dev/.cargo/kache");
        assert_eq!(KACHE_RUNTIME_DIR, "/tmp/kache");
        assert!(!KACHE_RUNTIME_DIR.starts_with("/home/dev/.cargo"));
        assert_eq!(KACHE_MAX_SIZE, "60GiB");
    }

    /// The first hit on `PATH` wins, which is what the guest's own resolution
    /// would do.
    #[test]
    fn the_first_match_on_path_wins() {
        let first = temp_dir("first");
        let second = temp_dir("second");
        touch_executable(&first.join("kache"));
        touch_executable(&second.join("kache"));
        let search = format!("{}:{}", first.display(), second.display());
        assert_eq!(
            resolve_rustc_wrapper(None, &search, is_executable),
            Some(first.join("kache"))
        );
        let _ = fs::remove_dir_all(&first);
        let _ = fs::remove_dir_all(&second);
    }
}
