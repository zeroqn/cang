//! cang's binding to libkrun, through libkrun's own Rust API.
//!
//! cang used to `dlopen` `libkrun.so.2` and `libkrun_init.so.0` and call the C
//! ABI generated from libkrun's Rust API. ABI 2 put that C surface behind
//! libkrun's `ffi` feature, and the C names are regenerated *from* the Rust API
//! (`make gen-libkrun-bindings`), so this crate binds the Rust API directly:
//! `libkrun` and `krun-init-blob` are cargo path dependencies on `deps/libkrun`
//! and link into the binary. No libkrun shared object is loaded at runtime.
//!
//! `libkrunfw.so.5` is still opened by soname, by libkrun's own
//! `Payload::load_krunfw`, and resolves through cang's `rpath`/`LD_LIBRARY_PATH`.
//!
//! [`LibkrunApi`] is the seam the launcher drives and the recording fake
//! implements; [`LinkedLibkrunApi`] is the production implementation.

mod api;
mod display;
mod firmware;
mod linked;

pub use api::{Handle, LibkrunApi};
pub use linked::LinkedLibkrunApi;

/// Tag the guest kernel resolves as its root filesystem (`KRUN_FS_ROOT_TAG`).
pub const CANG_FS_ROOT_TAG: &str = "/dev/root";

const CANG_NET_FEATURE_CSUM: u32 = 1 << 0;
const CANG_NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
const CANG_NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
const CANG_NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
const CANG_NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
const CANG_NET_FEATURE_HOST_UFO: u32 = 1 << 14;

/// Net device feature bits cang asks for, matching what the guest's virtio-net
/// driver expects (`crates/cang-guest-init`'s driver contract).
pub const CANG_LIBKRUN_COMPAT_NET_FEATURES: u32 = CANG_NET_FEATURE_CSUM
    | CANG_NET_FEATURE_GUEST_CSUM
    | CANG_NET_FEATURE_GUEST_TSO4
    | CANG_NET_FEATURE_GUEST_UFO
    | CANG_NET_FEATURE_HOST_TSO4
    | CANG_NET_FEATURE_HOST_UFO;
