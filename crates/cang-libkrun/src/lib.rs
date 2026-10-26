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

/// Threads cang gives the virtio-fs request queue.
///
/// virtio-fs has one request queue with no multiqueue feature, so this is a pool
/// behind that queue: the device thread reads the available ring and publishes
/// used-ring entries, the pool threads run the FUSE op and write the reply. It
/// pays off for exactly one measured class - a directory scan (`READDIRPLUS`),
/// where a single guest thread already pins the one server thread at ~95% of a
/// core while the guest itself is at 9-22% - and is close to free for
/// open/read/stat, where that thread never passes ~27% (note 20/21 in the
/// workspace-virtiofs-performance map). Four keeps the win without turning every
/// share into a thread farm; `set_worker_threads(1)` is the pre-pool behaviour.
/// No CLI flag: cang sets it, a user has no reason to.
pub const CANG_FS_WORKER_THREADS: usize = 4;

/// Whether cang asks for zero-message opens on a virtio-fs share by default.
///
/// The lever is the `FUSE_NO_OPEN_SUPPORT`/`FUSE_NO_OPENDIR_SUPPORT` pair: the
/// server answers the first `FUSE_OPEN`/`FUSE_OPENDIR` with `-ENOSYS`, the guest
/// kernel stops sending them and answers its own releases, and two round trips
/// disappear from every file open and every directory open - the biggest
/// request-removing lever the guest side has, because a round trip costs the
/// same per *request* whichever opcode it carries (note 20/22 of the
/// workspace-virtiofs-performance map). What the missing handle used to provide
/// is served from a descriptor the server keeps per inode.
///
/// On by default: measured on a 2401-crate offline `cargo build` in a cang guest,
/// a 357 s arm (three runs, 354-362 s) became 140 s (four runs, 132-158 s) -
/// 2.6x, far outside the arm-to-arm drift - because the requests it removes are
/// the ones a build issues most often (OPEN+RELEASE were 16% of a 3.7 M-request
/// build) and because keeping the page cache across opens stops rustc re-reading
/// every `.rlib` over FUSE (READ went from 36% of requests to 3%). See note 23 of
/// the workspace-virtiofs-performance map.
///
/// The one semantic it gives up: a file the guest has read stays cached after the
/// open that read it, so a *host-side in-place rewrite* of that file becomes
/// visible to the guest when the attributes refresh (attr_timeout, 5 s) rather
/// than at the next `open`. Files the host rewrites by rename (every editor, `git
/// checkout`, `sed -i`) are new inodes and unaffected, as are host-added
/// directory entries. `CANG_FS_ZERO_MESSAGE_OPEN=0|1` overrides the default per
/// launch, so an A/B - or a workspace that needs strict close-to-open - needs no
/// rebuild.
pub const CANG_FS_ZERO_MESSAGE_OPEN: bool = true;

/// Env var that overrides [`CANG_FS_ZERO_MESSAGE_OPEN`] (`0`/`false` or
/// `1`/`true`; anything else is refused rather than guessed at).
pub const CANG_FS_ZERO_MESSAGE_OPEN_ENV: &str = "CANG_FS_ZERO_MESSAGE_OPEN";

/// Env var that makes the fork log the opcode mix it served when the device
/// stops (`1`/`true`).
///
/// The pinned guest kernel has neither ftrace nor debugfs, so the only place a
/// workload's opcode histogram can be counted is the server. Off by default: it
/// is a measurement switch, not a diagnostic a normal launch wants to pay for.
pub const CANG_FS_STATS_ENV: &str = "CANG_FS_STATS";

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
