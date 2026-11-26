//! The build cache's configuration, for a project that brings the cache itself.
//!
//! The image no longer ships `kache`: a project that wants a compiler cache adds
//! it to its own `nix develop` shell (which is also where `RUSTC_WRAPPER` and the
//! C/C++ shims are set up), and everything that cache needs to find is already
//! configured here. cang filters an image's environment down to a short allowlist
//! (`runtime::launch::config::guest_env`), so image `Env` entries never reach the
//! guest process - hence these are re-exported:
//!
//! - `KACHE_CACHE_DIR=~/.cargo/kache`, the store: `~/.cargo` is the mount cang
//!   already grafts (`<state-root>/cargo`), so the cache needs no mount of its
//!   own, and a `--workspace-disk` launch has already bound that path to the
//!   tree's own `cargo/` on btrfs, where a restore can clone blocks;
//! - `KACHE_RUNTIME_DIR=/tmp/kache`, because the daemon's socket, locks and
//!   session state are machine-local: leaving them under the store would put a
//!   unix socket on the share;
//! - `KACHE_MAX_SIZE=60GiB`, the headroom `sccache` measured (a 5.6 GiB fill),
//!   rather than kache's `5% of the disk, floored at 5 GiB` default.
//!
//! Nothing is started here and no `RUSTC_WRAPPER` is set: the daemon is started
//! by the first cache-enabled compile in the shell that configured the wrapper, so
//! a guest that never builds never runs it. A variable that is already set wins.

use std::env;

pub(in crate::guest_init) const KACHE_CACHE_DIR_ENV: &str = "KACHE_CACHE_DIR";
pub(in crate::guest_init) const KACHE_RUNTIME_DIR_ENV: &str = "KACHE_RUNTIME_DIR";
pub(in crate::guest_init) const KACHE_MAX_SIZE_ENV: &str = "KACHE_MAX_SIZE";
/// The cargo home cang mounts; a project's cache goes inside it.
pub(in crate::guest_init) const KACHE_CACHE_DIR: &str = "/home/dev/.cargo/kache";
/// Sockets, locks, logs and session markers: the guest's own tmpfs, never the
/// store, which is a virtio-fs share on a default launch.
pub(in crate::guest_init) const KACHE_RUNTIME_DIR: &str = "/tmp/kache";
/// `sccache` was capped at 60 GiB after a measured 5.6 GiB fill; kache's default
/// budget is 5% of the store's disk floored at 5 GiB, which a Rust workspace
/// outgrows the same way.
pub(in crate::guest_init) const KACHE_MAX_SIZE: &str = "60GiB";

/// Exports the three variables, unless the caller or the image already set them.
pub(in crate::guest_init) fn export_configuration() {
    for (key, value) in configuration() {
        if env::var_os(key).is_none() {
            unsafe { env::set_var(key, value) };
        }
    }
}

/// The variables, as `(name, value)`, so the wiring can be tested without an
/// environment.
const fn configuration() -> [(&'static str, &'static str); 3] {
    [
        (KACHE_CACHE_DIR_ENV, KACHE_CACHE_DIR),
        (KACHE_RUNTIME_DIR_ENV, KACHE_RUNTIME_DIR),
        (KACHE_MAX_SIZE_ENV, KACHE_MAX_SIZE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store has to be the cargo home cang already mounts, and the daemon's
    /// runtime state has to stay off it.
    #[test]
    fn the_store_is_the_cargo_home_and_the_runtime_dir_is_not() {
        assert_eq!(KACHE_CACHE_DIR, "/home/dev/.cargo/kache");
        assert_eq!(KACHE_RUNTIME_DIR, "/tmp/kache");
        assert!(!KACHE_RUNTIME_DIR.starts_with("/home/dev/.cargo"));
        assert_eq!(KACHE_MAX_SIZE, "60GiB");
    }

    /// `RUSTC_WRAPPER` belongs to the shell that installs the cache, not to the
    /// guest's boot environment: nothing here may point a plain image's compiler
    /// at a cache that is not there.
    #[test]
    fn the_configuration_names_no_wrapper() {
        let keys: [&str; 3] = [
            KACHE_CACHE_DIR_ENV,
            KACHE_RUNTIME_DIR_ENV,
            KACHE_MAX_SIZE_ENV,
        ];
        assert!(!keys.contains(&"RUSTC_WRAPPER"));
        assert_eq!(configuration().len(), keys.len());
        for (key, value) in configuration() {
            assert!(keys.contains(&key), "{key} is not one of the three");
            assert!(!value.is_empty(), "{key} has no value");
        }
    }
}
