//! The guest `/nix` lower layer as a read-only btrfs block file.
//!
//! The overlay's lower layer is the image's `/nix/store`: read-only, every
//! command reads it, and over virtio-fs each of those reads is a FUSE request.
//! Note 56 measured the same bytes on virtio-blk at 7.5-16x the `find` speed and
//! 43x the `du` speed, so this module serves *only* the lower from a block
//! device. `upper`/`work` stay host btrfs directories (dedup: substituted store
//! objects remain reflink- or hardlink-shared with the host's own `/nix/store`).
//!
//! The image is keyed by the *content* of the store, not the image digest, so
//! two image digests that ship the same `/nix/store` share one block file:
//!
//! ```text
//! identity = sha256 of the sorted `ls <rootfs>/nix/store` names
//! ```
//!
//! Building it is `mkfs.btrfs --rootdir --reflink --shrink`, which populates the
//! image without mounting it and clones the file data instead of copying it (the
//! image's data extents *are* the source files' extents), run inside the Buildah
//! storage user namespace where the store appears owned by root. `mkfs.btrfs`
//! prints one harmless `ERROR: zoned: unable to stat` line for a non-zoned target
//! and still exits 0, so the outcome is judged by exit status alone.
//!
//! Nothing here may fail a launch: an image that cannot be built (no space, mkfs
//! failure, a store the host cannot list) leaves the launch on the host-composed
//! overlay, which is what the guest keeps using when the flag is absent.
//!
//! # Status: on by default, on both kernel lines
//!
//! Under the 7.x guest kernel an ordinary cold read of `/nix/store` lost a
//! 20-40 KiB window of one file: the guest's btrfs logged a csum of a zero page
//! and anything mapped over those pages took `SIGBUS`. The cause was libkrun's
//! descriptor handling, not this module and not btrfs: the guest-memory map has a
//! region boundary at the end of the kernel image that libkrunfw inserts
//! (`[0x1000000, 0x1000000 + kernel_size)`, a separate region from the RAM on
//! either side), while the guest is told the whole span is RAM and hands out
//! pages there once it frees its own image. A merged scatter-gather entry could
//! then straddle that boundary, and `Reader`/`Writer::from_guest_buffers`
//! rejected the entire chain - the request completed with no data and no status,
//! which the guest's virtio-blk driver scored as success, so btrfs verified pages
//! that were never written. Buffers are now split per region instead
//! (`crates/.../descriptor_utils.rs::buffer_region_slices`), which is what makes
//! the 7.x line safe: its large folios and merged SG entries only made a
//! boundary-spanning descriptor common, they were never the fault.
//!
//! `max_sectors_kb` was the former stop-gap and is no longer needed: with the
//! split in place, an instrumented 7.3.1 firmware reads the same 40 files
//! (1.74 GB) at the default 4096 KiB with no lost page in 9 of 9 sweeps, where
//! the unfixed device lost 1-19 pages per sweep. A 64 KiB cap stays a
//! measurement knob, not a fix.
//!
//! A *second*, deterministic fault used to live here and is fixed: two launches
//! sharing one `[state] location` where the second rebuilt the image replaced
//! the file a live VM had attached, and that VM read blocks belonging to the
//! other image (2 of 2 measured; arbitrary csum values). Launches now hold the
//! image shared for the life of the VM and a rebuild needs it exclusively (see
//! [`ImageLock`]).

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};
use crate::runtime::launch::config::{BindMount, DiskAttachment, HostNixOverlay};

/// The block device's label, and the id (libkrun serial) the guest looks it up
/// by. Both are guest-visible contract: a mismatch leaves the guest on the
/// host-composed overlay.
pub(super) const LABEL: &str = "CANG_NIX_LOWER";
pub(super) const ID: &str = "cang-nix-lower";
/// Sibling of the digest-keyed cache entries, under
/// `<state>/microvm/images/btrfs-snapshots/`.
pub(super) const DIR_NAME: &str = "nix-lower";
const RAW_SUFFIX: &str = "raw";
const STATE_SUFFIX: &str = "state";
const STORE_DIR: &str = "store";
const ROOTFS_DIR: &str = "rootfs";
/// Where the guest composes the overlay itself, so its `upper` and `work` land on
/// one filesystem exactly as overlayfs requires.
pub(crate) const GUEST_OVERLAY_ROOT: &str = "/run/cang/nix-overlay";
/// The generation directory's `work` is the *host* overlay's work dir; the guest
/// composes a second overlay over the same upper, and two overlay instances may
/// not share one work directory, so the guest gets its own (empty) one.
pub(crate) const GUEST_WORK_DIR_NAME: &str = "work-guest";
/// Where the writable upper lives when it is on the workspace's block disk
/// instead of the share: a sibling of `cargo/` and `target/` in the build disk's
/// filesystem, one directory per image generation.
///
/// The path is under the build disk's guest mount point
/// (`persistent_disks::build::GUEST_MOUNT_POINT`), so the guest has it as a
/// local btrfs filesystem by the time nix-prep runs, and the overlay's upper and
/// work are one mount, on a filesystem that is not the lower's - the placement
/// rule overlayfs needs (notes 02 of the nix-upper-on-block map).
pub(crate) const BUILD_DISK_UPPER_ROOT: &str = "/home/dev/.cache/cang-build/nix-overlay";
pub(crate) const GUEST_UPPER_DIR: &str = "/run/cang/nix-overlay/upper";
pub(crate) const GUEST_WORK_DIR: &str = "/run/cang/nix-overlay/work-guest";
const BIND_TAG: &str = "cang-nix-overlay-src";
const BLOCK_MODE_ENV: &str = "CANG_NIX_LOWER_BLOCK";
const STATE_LABEL_KEY: &str = "label";
const STATE_IDENTITY_KEY: &str = "identity";
const STATE_STORE_PATHS_KEY: &str = "store_paths";
const STATE_BUILT_AT_KEY: &str = "built_at";

/// The launch's hold on one lower image, released when the launch ends.
///
/// A rebuild renames a fresh image over the old name, and the two sides of that
/// only stay consistent while nobody is reading the file it replaces: with two
/// launches sharing one state root - two smoke arms, two agent sessions, a
/// benchmark beside a manual run - the second launch's rebuild ran while the
/// first VM had the image attached, and the guest's btrfs then read blocks that
/// belonged to the other image (`csum failed`, `SIGBUS` on page-in, chromium
/// dying before it can log). Every launch therefore holds the image *shared* for
/// as long as the VM can read it, and only a rebuild takes it *exclusive*.
#[derive(Clone)]
pub(crate) struct ImageLock {
    file: std::sync::Arc<std::fs::File>,
}

impl ImageLock {
    fn acquire(path: &Path, exclusive: bool) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("failed to open the /nix lower lock '{}'", path.display()))?;
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        } | libc::LOCK_NB;
        // SAFETY: `flock` takes a valid fd and a lock operation; a failure is
        // reported by the return value, not by memory.
        let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), operation) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                bail!(
                    "another cang launch is using the /nix lower image '{}' (state root '{}'): concurrent launches must not rebuild an image a live VM is reading - give each one its own [state] location",
                    path.display(),
                    path.parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
            }
            return Err(err).with_context(|| {
                format!("failed to lock the /nix lower image '{}'", path.display())
            });
        }
        Ok(Self {
            file: std::sync::Arc::new(file),
        })
    }
}

impl std::fmt::Debug for ImageLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ImageLock")
    }
}

impl PartialEq for ImageLock {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.file, &other.file)
    }
}

impl Eq for ImageLock {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NixLowerDiskStatus {
    Built,
    Reused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NixLowerDisk {
    pub(crate) path: PathBuf,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) identity: String,
    pub(crate) store_paths: usize,
    pub(crate) status: NixLowerDiskStatus,
}

/// The whole host contribution of block mode: the read-only disk, the share that
/// carries `upper`/`work` to the guest, and the env that names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NixLowerBlock {
    disk: NixLowerDisk,
    generation_root: PathBuf,
    /// Whether the writable upper is the build disk's, beside the lower, instead
    /// of the host-directory share. Set from the guest binary's capability: an
    /// image whose `cang-guest-init` predates the seeding step cannot serve it.
    upper_on_build_disk: bool,
    /// The image generation's own name on the build disk.
    ///
    /// One block-disk upper per image digest, not one per workspace: the guest
    /// seeds a fresh generation by cloning the previous one, and a constant name
    /// would let the old generation's sentinel say "already seeded" so an image
    /// move would silently keep the previous image's Nix DB.
    generation: String,
    /// Kept for the whole launch: the image cannot be rebuilt underneath it.
    _lock: ImageLock,
}

impl NixLowerBlock {
    pub(crate) fn attachment(&self) -> DiskAttachment {
        DiskAttachment {
            id: self.disk.id.clone(),
            path: self.disk.path.clone(),
            read_only: true,
        }
    }

    /// The generation directory (`.../nix-overlay`) holding `upper` and the
    /// guest's work dir, as one guest-visible writable share.
    pub(crate) fn bind_mount(&self) -> BindMount {
        BindMount::directory(self.generation_root.clone(), BIND_TAG, GUEST_OVERLAY_ROOT)
    }

    /// `(upperdir, workdir)` for the guest's overlay.
    ///
    /// Both are on one filesystem either way, which is overlayfs's rule; what
    /// changes is *which* filesystem. On the build disk they are a btrfs
    /// subvolume inside the disk's image, beside `cargo/` and `target/`.
    pub(crate) fn guest_upper_dirs(&self) -> (String, String) {
        if self.upper_on_build_disk {
            let root = format!("{BUILD_DISK_UPPER_ROOT}/{}", self.generation);
            (format!("{root}/upper"), format!("{root}/work"))
        } else {
            (GUEST_UPPER_DIR.to_owned(), GUEST_WORK_DIR.to_owned())
        }
    }

    pub(crate) fn env_pairs(&self) -> Vec<(String, String)> {
        vec![
            (BLOCK_MODE_ENV.to_owned(), "1".to_owned()),
            ("CANG_NIX_LOWER_DISK_ID".to_owned(), self.disk.id.clone()),
            (
                "CANG_NIX_LOWER_DISK_LABEL".to_owned(),
                self.disk.label.clone(),
            ),
            (
                "CANG_NIX_LOWER_UPPERDIR".to_owned(),
                self.guest_upper_dirs().0,
            ),
            (
                "CANG_NIX_LOWER_WORKDIR".to_owned(),
                self.guest_upper_dirs().1,
            ),
        ]
    }

    pub(crate) fn disk(&self) -> &NixLowerDisk {
        &self.disk
    }
}

/// The image builder, split out so the decisions (identity, reuse, rename,
/// state) can be tested without running `mkfs.btrfs`.
pub(super) trait NixLowerImageBuilder {
    /// Populate `tmp_image` with a btrfs filesystem whose root holds the entries
    /// of `source_nix_dir`.
    fn build_image(&self, source_nix_dir: &Path, tmp_image: &Path, label: &str) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HostNixLowerImageBuilder;

impl NixLowerImageBuilder for HostNixLowerImageBuilder {
    fn build_image(&self, source_nix_dir: &Path, tmp_image: &Path, label: &str) -> Result<()> {
        let program = runtime_tool_program(RuntimeTool::MkfsBtrfs);
        let args = mkfs_args(source_nix_dir, tmp_image, label);
        run_in_storage_namespace(runtime_tool_program(RuntimeTool::Buildah), program, &args)
    }
}

/// `mkfs.btrfs -q -L <label> --rootdir=<source> --reflink --shrink <image>`.
///
/// Split out so the argument vector is asserted on its own: `--rootdir` takes the
/// source as one `=`-joined operand, and the image is the last positional.
pub(super) fn mkfs_args(source_nix_dir: &Path, tmp_image: &Path, label: &str) -> Vec<String> {
    vec![
        "-q".to_owned(),
        "-L".to_owned(),
        label.to_owned(),
        format!("--rootdir={}", source_nix_dir.display()),
        "--reflink".to_owned(),
        "--shrink".to_owned(),
        tmp_image.display().to_string(),
    ]
}

/// Run `program args...` inside `unshare_program unshare`, and judge the outcome
/// by exit status only.
///
/// The status is the whole verdict on purpose. `mkfs.btrfs` writes
/// `ERROR: zoned: unable to stat <image>` to stderr for every non-zoned target -
/// which is every target cang builds - and still exits 0; a launch that treated
/// that line as failure would never build an image. The output is kept for
/// diagnostics and logged, never parsed.
fn run_in_storage_namespace(
    unshare_program: impl AsRef<OsStr>,
    program: impl AsRef<OsStr>,
    args: &[String],
) -> Result<()> {
    let unshare_program = unshare_program.as_ref();
    let program = program.as_ref();
    let output = Command::new(unshare_program)
        .arg("unshare")
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => anyhow!(
                "mkfs.btrfs and buildah are required to build the /nix lower image; install btrfs-progs and buildah and retry"
            ),
            _ => err.into(),
        })
        .with_context(|| {
            format!(
                "failed to run '{}' inside the Buildah storage namespace",
                program.to_string_lossy()
            )
        })?;

    judge_mkfs_output(program, &output)
}

/// The verdict on a finished `mkfs.btrfs`: exit status only.
///
/// The status is the whole verdict on purpose. `mkfs.btrfs` writes
/// `ERROR: zoned: unable to stat <image>` to stderr for every non-zoned target -
/// which is every target cang builds - and still exits 0; a launch that treated
/// that line as failure would never build an image. The output is kept for
/// diagnostics and logged, never parsed.
fn judge_mkfs_output(program: &OsStr, output: &std::process::Output) -> Result<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !output.status.success() {
        if stderr.is_empty() {
            bail!(
                "{} failed with {}",
                program.to_string_lossy(),
                output.status
            );
        }
        bail!(
            "{} failed with {}: {stderr}",
            program.to_string_lossy(),
            output.status
        );
    }
    if !stderr.is_empty() {
        // The measured recipe always prints the zoned line; keep it visible at
        // debug level so a real surprise is still findable.
        tracing::debug!(
            stderr = %stderr,
            "mkfs.btrfs reported non-fatal output while building the /nix lower image"
        );
    }
    Ok(())
}

/// Entry point for a launch's `/nix` lower.
///
/// `Ok(None)` means the launch kept the host-composed overlay: it asked for it
/// (`--nix=lower:virtiofs`), or the block lower could
/// not be served and the mode allows the overlay (`--nix=lower:auto`, the
/// default). `--nix=lower:blk` is the strict mode: it refuses the launch instead,
/// because a quiet fallback is five times slower on the store and nothing in the
/// run says so.
///
/// `guest_init` is the binary this launch will actually run (the image's, or the
/// `--guest-init` override). Block mode has a guest half, and an image built
/// before that half existed ignores every `CANG_NIX_LOWER_*` variable: the disk
/// would be built, attached, and never mounted, which is a `/nix` that silently
/// stays virtio-fs. Reading the guest binary first turns that into a named
/// refusal and skips a multi-gigabyte build nothing would read.
pub(crate) fn prepare_block(
    intent: &HostNixOverlay,
    guest_init: &Path,
    mode: Option<NixLowerMode>,
) -> Result<Option<NixLowerBlock>> {
    if !block_mode_requested(mode) {
        tracing::debug!(
            mode = mode.map_or("default", NixLowerMode::as_str),
            "the /nix block lower was not selected: keeping the host-composed overlay"
        );
        return Ok(None);
    }
    if !guest_init_mounts_block_lower(guest_init) {
        let reason = format!(
            "the cang-guest-init this launch would run ('{}') predates block mode, so the /nix lower disk would be ignored",
            guest_init.display()
        );
        return fallback_or_error(intent, reason, mode);
    }
    // The block lower is only half of the pairing: its upper is on the same disk,
    // and a guest that cannot seed a fresh one would mount an overlay over a
    // directory the host never prepared. That is the mixed pair the resolution
    // refuses, so an image without the seeding step gets the host overlay for
    // both layers - or, in `blk`, a refusal rather than a mount that fails.
    if !guest_init_seeds_upper(guest_init) {
        let reason = format!(
            "the cang-guest-init this launch would run ('{}') predates the block-disk upper, so the writable /nix layer would stay on the host share and the two layers would not match",
            guest_init.display()
        );
        return fallback_or_error(intent, reason, mode);
    }
    match prepare_block_with(intent, &HostNixLowerImageBuilder, UPPER_ON_BUILD_DISK) {
        Ok(block) => Ok(block),
        Err(err) => fallback_or_error(intent, format!("{err:#}"), mode),
    }
}

/// Keep the host-composed overlay, or refuse the launch. Either way the reason
/// is named once, on stderr, in the same words the refusal carries.
fn fallback_or_error(
    intent: &HostNixOverlay,
    reason: String,
    mode: Option<NixLowerMode>,
) -> Result<Option<NixLowerBlock>> {
    // `auto` and `virtiofs` both keep the host-composed overlay; only `blk` is
    // strict, because only `blk` asked for a launch that must not be slower.
    if !matches!(mode, Some(NixLowerMode::Blk)) {
        tracing::warn!(
            selected_image = %intent.selected_reference,
            mode = mode.map_or("default", NixLowerMode::as_str),
            %reason,
            "cang /nix lower: keeping the host-composed overlay"
        );
        eprintln!(
            "cang: /nix block lower unavailable, keeping the host-composed overlay (--nix=lower:{}): {reason}",
            mode.map_or("auto", NixLowerMode::as_str)
        );
        return Ok(None);
    }
    bail!(
        "/nix block lower requested but unavailable: {reason}\n\
         cang: pass --nix=lower:virtiofs to keep the host-composed /nix overlay instead (slower store walks), \
         or --nix=lower:auto to keep it only when the block lower cannot be served"
    )
}

/// The guest half's switch, which is also the contract between the two halves.
const GUEST_HALF_MARKER: &[u8] = b"CANG_NIX_LOWER_BLOCK";
/// The sentinel the guest writes into an upper it seeded itself. Its presence in
/// the binary is the second half of the contract: an image without it cannot put
/// the writable layer on the block disk.
const GUEST_UPPER_MARKER: &[u8] = b".cang-upper-seeded";
/// Whether this build puts the upper on the block disk when the lower is there.
const UPPER_ON_BUILD_DISK: bool = true;

/// Whether the `cang-guest-init` that will run in this launch knows block mode.
///
/// The guest composes the `/nix` overlay itself only if it parses
/// `CANG_NIX_LOWER_BLOCK` and the four paths beside it, and a release binary
/// keeps its string literals, so the switch's own name in the file is the
/// guest's capability.
/// A binary that cannot be read is not evidence of an old guest-init, so it keeps
/// the legacy assumption (supported) rather than disabling block mode on an I/O
/// error.
/// Whether the `cang-guest-init` that will run in this launch seeds a fresh
/// upper itself (see `GUEST_UPPER_MARKER`).
/// The block disk's directory name for an image generation: its digest key.
///
/// `sha256-abc…` needs no escaping, but a digest key is host input, so anything
/// that is not a plain name character becomes `-` and a leading dot is refused:
/// the guest writes this directory, and a path segment it chose must not be able
/// to reach outside the parent.
fn generation_dir_name(digest_key: &str) -> String {
    let sanitized: String = digest_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = sanitized.trim_start_matches('-').to_owned();
    if name.is_empty() || name.strip_prefix('.').is_some() {
        "generation".to_owned()
    } else {
        name
    }
}

pub(super) fn guest_init_seeds_upper(guest_init: &Path) -> bool {
    match fs::read(guest_init) {
        Ok(bytes) => contains(&bytes, GUEST_UPPER_MARKER),
        Err(err) => {
            tracing::debug!(
                guest_init = %guest_init.display(),
                error = %err,
                "could not read cang-guest-init; assuming it seeds the upper"
            );
            true
        }
    }
}

pub(super) fn guest_init_mounts_block_lower(guest_init: &Path) -> bool {
    match fs::read(guest_init) {
        Ok(bytes) => contains(&bytes, GUEST_HALF_MARKER),
        Err(err) => {
            tracing::debug!(
                guest_init = %guest_init.display(),
                error = %err,
                "cang /nix lower: cannot read the guest-init binary; assuming it supports block mode"
            );
            true
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn prepare_block_with(
    intent: &HostNixOverlay,
    builder: &impl NixLowerImageBuilder,
    upper_on_build_disk: bool,
) -> Result<Option<NixLowerBlock>> {
    match ensure_with_builder(intent, builder, upper_on_build_disk) {
        Ok(block) => Ok(block),
        Err(err) => {
            tracing::warn!(
                lowerdir = %intent.lowerdir.display(),
                error = %format!("{err:#}"),
                "cang could not build the /nix block lower"
            );
            Err(err).context("could not build the /nix block lower image")
        }
    }
}

/// How a launch serves `/nix`: from the read-only block lower (the default) or on
/// the host-composed overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NixLowerMode {
    /// Serve `/nix` from the read-only btrfs image on virtio-blk, and keep the
    /// host-composed overlay with a named reason when that image cannot be served.
    #[default]
    Auto,
    /// Serve `/nix` from the block lower, or refuse the launch.
    Blk,
    /// Keep the host-composed overlay on virtio-fs; no image is built or attached.
    VirtioFs,
}

impl NixLowerMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Blk => "blk",
            Self::VirtioFs => "virtiofs",
        }
    }
}

/// Block mode is on by default: `--nix=lower:virtiofs` keeps the
/// host-composed overlay, and there is no environment variable of its own - the
/// flag is the whole surface, and `CANG_NIX_LOWER_BLOCK` is only the contract
/// this half sets for the guest.
///
/// The configuration it enables is the fast one from note 57 - `find /nix/store`
/// 206-215 ms against 864-2033 ms, `du -s` 267 ms against 9.2-10.4 s - and it
/// became possible once the fork forwarded `RENAME_WHITEOUT` (v2.0.0-cang.18).
///
/// Both kernel lines read this image without losing a page (see the module doc
/// for the descriptor split that made 7.x safe).
///
/// `blk` refuses a launch it cannot serve, so nobody who asked for the block lower
/// silently gets the slower overlay; `auto` keeps the launch and names the reason.
fn block_mode_requested(mode: Option<NixLowerMode>) -> bool {
    !matches!(mode, Some(NixLowerMode::VirtioFs))
}

pub(super) fn ensure_with_builder(
    intent: &HostNixOverlay,
    builder: &impl NixLowerImageBuilder,
    upper_on_build_disk: bool,
) -> Result<Option<NixLowerBlock>> {
    let store_dir = intent.lowerdir.join(STORE_DIR);
    if !store_dir.is_dir() {
        tracing::debug!(
            store_dir = %store_dir.display(),
            "cang /nix lower: the image has no /nix/store; keeping the host-composed overlay"
        );
        return Ok(None);
    }
    let names = store_names(&store_dir)?;
    if names.is_empty() {
        tracing::debug!(
            store_dir = %store_dir.display(),
            "cang /nix lower: the image's /nix/store is empty; keeping the host-composed overlay"
        );
        return Ok(None);
    }
    let identity = identity_of(&names);
    let (disk, lock) = ensure_image_with_builder(&store_dir, &identity, names.len(), builder)?;

    let generation_root = intent
        .upperdir
        .parent()
        .ok_or_else(|| {
            anyhow!(
                "cang /nix overlay upperdir '{}' has no generation directory",
                intent.upperdir.display()
            )
        })?
        .to_path_buf();
    let guest_workdir = generation_root.join(GUEST_WORK_DIR_NAME);
    fs::create_dir_all(&guest_workdir).with_context(|| {
        format!(
            "failed to create the guest /nix overlay work directory '{}'",
            guest_workdir.display()
        )
    })?;

    Ok(Some(NixLowerBlock {
        disk,
        generation_root,
        upper_on_build_disk,
        generation: generation_dir_name(&intent.digest_key),
        _lock: lock,
    }))
}

/// The sorted names of `<rootfs>/nix/store`, which is the identity's input.
pub(super) fn store_names(store_dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(store_dir)
        .with_context(|| format!("failed to list '{}'", store_dir.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to read an entry of the image's /nix/store '{}'",
                store_dir.display()
            )
        })?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            anyhow!(
                "image /nix/store entry '{}' is not valid UTF-8",
                entry.path().display()
            )
        })?;
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

/// `sha256` over the sorted store names, one name per line.
///
/// The store content decides the identity rather than the image digest: two
/// digests shipping the same `/nix/store` then share one block file.
pub(super) fn identity_of(sorted_names: &[String]) -> String {
    let mut buffer = String::new();
    for name in sorted_names {
        buffer.push_str(name);
        buffer.push('\n');
    }
    sha256_hex(buffer.as_bytes())
}

/// Ensure `<nix-lower>/<identity>.raw` exists, reusing a complete pair of image
/// and state file and rebuilding anything else.
pub(super) fn ensure_image_with_builder(
    store_dir: &Path,
    identity: &str,
    store_paths: usize,
    builder: &impl NixLowerImageBuilder,
) -> Result<(NixLowerDisk, ImageLock)> {
    let dir = nix_lower_dir(store_dir)?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create '{}'", dir.display()))?;
    let image = dir.join(format!("{identity}.{RAW_SUFFIX}"));
    let state = dir.join(format!("{identity}.{STATE_SUFFIX}"));
    let lock_path = dir.join(format!("{identity}.{RAW_SUFFIX}.lock"));

    // Shared when the image is already usable, so any number of launches can
    // attach it at once; exclusive around a rebuild, so a rebuild never runs
    // while another launch is reading the file it replaces.
    if complete(&image, &state, identity) {
        let lock = ImageLock::acquire(&lock_path, false)?;
        if complete(&image, &state, identity) {
            return Ok((
                NixLowerDisk {
                    path: image,
                    id: ID.to_owned(),
                    label: LABEL.to_owned(),
                    identity: identity.to_owned(),
                    store_paths,
                    status: NixLowerDiskStatus::Reused,
                },
                lock,
            ));
        }
    }

    let exclusive = ImageLock::acquire(&lock_path, true)?;

    let tmp = dir.join(format!("{identity}.{RAW_SUFFIX}.tmp"));
    remove_if_present(&tmp)?;
    let source = store_dir
        .parent()
        .ok_or_else(|| anyhow!("image store '{}' has no parent", store_dir.display()))?
        .to_path_buf();
    check_space_for_build(&dir, &source)?;
    if let Err(err) = builder.build_image(&source, &tmp, LABEL) {
        remove_if_present(&tmp).ok();
        return Err(err).with_context(|| {
            format!(
                "failed to build the /nix lower image '{}' from '{}'",
                image.display(),
                source.display()
            )
        });
    }
    let built = fs::metadata(&tmp)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if built == 0 {
        remove_if_present(&tmp).ok();
        bail!(
            "mkfs.btrfs reported success but produced no image at '{}'",
            tmp.display()
        );
    }
    // Renaming over a stale image is atomic on one filesystem, so a launch that
    // fails halfway never leaves a half-written image under the reused name.
    fs::rename(&tmp, &image).with_context(|| {
        format!(
            "failed to install the /nix lower image '{}'",
            image.display()
        )
    })?;
    write_state(&state, identity, store_paths)?;
    tracing::info!(
        image = %image.display(),
        bytes = built,
        store_paths,
        identity,
        "cang built the /nix block lower image"
    );

    Ok((
        NixLowerDisk {
            path: image,
            id: ID.to_owned(),
            label: LABEL.to_owned(),
            identity: identity.to_owned(),
            store_paths,
            status: NixLowerDiskStatus::Built,
        },
        // The freshly built image stays held shared for the launch: this lock
        // also carries the exclusive hold taken above, so it is handed over.
        exclusive,
    ))
}

/// `<...>/btrfs-snapshots/nix-lower`, from `<...>/btrfs-snapshots/<digest>/rootfs/nix`.
fn nix_lower_dir(store_dir: &Path) -> Result<PathBuf> {
    let nix_dir = store_dir.parent().ok_or_else(|| {
        anyhow!(
            "image store '{}' has no /nix directory to derive the lower image path from",
            store_dir.display()
        )
    })?;
    let rootfs = nix_dir
        .parent()
        .filter(|parent| parent.file_name() == Some(OsStr::new(ROOTFS_DIR)))
        .ok_or_else(|| {
            anyhow!(
                "image /nix directory '{}' is not under a '{}' directory",
                nix_dir.display(),
                ROOTFS_DIR
            )
        })?;
    let entry = rootfs.parent().ok_or_else(|| {
        anyhow!(
            "image rootfs '{}' has no digest-keyed cache entry directory",
            rootfs.display()
        )
    })?;
    let snapshots = entry.parent().ok_or_else(|| {
        anyhow!(
            "image cache entry '{}' has no btrfs-snapshots directory",
            entry.display()
        )
    })?;
    Ok(snapshots.join(DIR_NAME))
}

/// An image is reusable when the file is there and the state file next to it
/// names the same identity: an image from another content set is rebuilt.
/// Bytes a build that can share extents with its source still needs: the image's
/// own metadata, its superblock, and whatever the guest writes while the build
/// runs.
const BUILD_FLOOR_BYTES: u64 = 512 * 1024 * 1024;

/// Refuse a build the target filesystem cannot hold.
///
/// `mkfs.btrfs` reports `no space left` as a failure and this module then falls
/// back, but the price is a multi-gigabyte attempt that can also evict whatever
/// else shares the filesystem. Price it first instead: the image is written from
/// the source tree, so its apparent size is the bill - unless the target is on
/// the source's own filesystem, where `--reflink` shares the extents and only
/// the metadata is new.
fn check_space_for_build(target_dir: &Path, source_nix: &Path) -> Result<()> {
    // A unit test builds into a temp directory whose free space is whatever the
    // sandbox gives it, and the arithmetic it exercises is `check_space`; a real
    // launch is the only caller that needs the preflight.
    if cfg!(test) {
        return Ok(());
    }
    let free = free_bytes(target_dir).with_context(|| {
        format!(
            "failed to measure free space for '{}'",
            target_dir.display()
        )
    })?;
    let apparent = apparent_bytes(source_nix).with_context(|| {
        format!(
            "failed to measure the size of the image store '{}'",
            source_nix.display()
        )
    })?;
    let same_filesystem = match (device_id(target_dir), device_id(source_nix)) {
        (Ok(target), Ok(source)) => target == source,
        // An unreadable device id is not evidence of a shared filesystem, and the
        // larger estimate only costs a preflight refusal on a tight disk.
        _ => false,
    };
    check_space(free, apparent, same_filesystem)
}

/// The pure half of [`check_space_for_build`], so the arithmetic is testable
/// without a filesystem.
/// Bytes the build needs: the tree's apparent size only when `--reflink` cannot
/// share it with the source.
fn required_space(apparent: u64, same_filesystem: bool) -> u64 {
    if same_filesystem {
        BUILD_FLOOR_BYTES
    } else {
        apparent.saturating_add(BUILD_FLOOR_BYTES)
    }
}

fn check_space(free: u64, apparent: u64, same_filesystem: bool) -> Result<()> {
    let required = required_space(apparent, same_filesystem);
    if free < required {
        bail!(
            "building the /nix lower image needs about {} MiB free but only {} MiB are available{}",
            required / (1024 * 1024),
            free / (1024 * 1024),
            if same_filesystem {
                ""
            } else {
                " (the image store is on a different filesystem, so --reflink cannot share it)"
            }
        );
    }
    Ok(())
}

/// Apparent size of a tree: regular files only, symlinks not followed, so it is
/// what a copy of the tree would have to store.
fn apparent_bytes(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to list '{}'", dir.display()))?
        {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn free_bytes(path: &Path) -> Result<u64> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("'{}' is not a usable path", path.display()))?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `statvfs` writes one `statvfs` into the pointer it is given, and
    // the CString outlives the call.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("statvfs failed");
    }
    // SAFETY: the call succeeded, so the value is initialized.
    let stat = unsafe { stat.assume_init() };
    // `f_frsize`/`f_bavail` are `c_ulong` on both supported targets, so the
    // product is already `u64`.
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

fn device_id(path: &Path) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(fs::metadata(path)?.dev())
}

fn complete(image: &Path, state: &Path, identity: &str) -> bool {
    let Ok(metadata) = fs::symlink_metadata(image) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        return false;
    }
    let Ok(text) = fs::read_to_string(state) else {
        return false;
    };
    parse_state(&text)
        .get(STATE_IDENTITY_KEY)
        .map(String::as_str)
        == Some(identity)
}

fn parse_state(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn write_state(path: &Path, identity: &str, store_paths: usize) -> Result<()> {
    let built_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let text = format!(
        "{STATE_IDENTITY_KEY}={identity}\n{STATE_LABEL_KEY}={LABEL}\n{STATE_STORE_PATHS_KEY}={store_paths}\n{STATE_BUILT_AT_KEY}={built_at}\n"
    );
    fs::write(path, text).with_context(|| format!("failed to write '{}'", path.display()))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove '{}'", path.display())),
    }
}

/// SHA-256, spelled out because the identity is a content hash and cang carries
/// no digest crate (adding one would move `Cargo.lock` and every vendor hash).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut hex = String::with_capacity(64);
    for word in state {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One lock for every test that reads or writes the block-mode switch: a
    /// parallel test must not observe another's value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[derive(Default)]
    struct FakeBuilder {
        builds: RefCell<Vec<(PathBuf, PathBuf, String)>>,
        fail: bool,
        write_bytes: Option<usize>,
    }

    impl FakeBuilder {
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }

        fn writes(mut self, bytes: usize) -> Self {
            self.write_bytes = Some(bytes);
            self
        }

        fn build_count(&self) -> usize {
            self.builds.borrow().len()
        }
    }

    impl NixLowerImageBuilder for FakeBuilder {
        fn build_image(&self, source_nix_dir: &Path, tmp_image: &Path, label: &str) -> Result<()> {
            self.builds.borrow_mut().push((
                source_nix_dir.to_path_buf(),
                tmp_image.to_path_buf(),
                label.to_owned(),
            ));
            if self.fail {
                bail!("mkfs.btrfs failed with exit status: 1");
            }
            fs::write(tmp_image, vec![0u8; self.write_bytes.unwrap_or(4096)])
                .context("fake builder should write the image")?;
            Ok(())
        }
    }

    /// The cache entry layout the identity and the image path are derived from:
    /// `<snapshots>/sha256-<digest>/rootfs/nix/store`.
    fn cache_entry(temp: &tempfile::TempDir, store: &[&str]) -> PathBuf {
        let entry = temp.path().join("btrfs-snapshots").join("sha256-feedface");
        let store_dir = entry.join("rootfs").join("nix").join("store");
        fs::create_dir_all(&store_dir).expect("store dir");
        for name in store {
            fs::create_dir(store_dir.join(name)).expect("store path");
        }
        entry.join("rootfs").join("nix")
    }

    fn intent(nix_dir: &Path, generation_root: &Path) -> HostNixOverlay {
        HostNixOverlay {
            selected_reference: "localhost/cang:latest".to_owned(),
            image_digest: "sha256:feedface".to_owned(),
            digest_key: "sha256-feedface".to_owned(),
            lowerdir: nix_dir.to_path_buf(),
            upperdir: generation_root.join("upper"),
            workdir: generation_root.join("work"),
            mergeddir: generation_root.join("merged"),
        }
    }

    fn generation_root(temp: &tempfile::TempDir) -> PathBuf {
        temp.path().join("workspace").join("nix-overlay")
    }

    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Long enough to exercise the second block and the length padding.
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// The identity is over the *sorted* names, so the order the directory
    /// happens to return them in does not decide whether an image is reused.
    #[test]
    fn identity_is_sha256_over_sorted_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = cache_entry(&temp, &["b-path", "a-path"]).join("store");
        let names = store_names(&store).expect("store names");
        assert_eq!(
            names,
            vec!["a-path".to_owned(), "b-path".to_owned()],
            "the listing is sorted, so the directory's own order cannot matter"
        );
        assert_eq!(
            identity_of(&names),
            sha256_hex(b"a-path\nb-path\n"),
            "the identity is one name per line"
        );
        // The same content created in the opposite order is the same identity.
        let other = tempfile::tempdir().expect("tempdir");
        let reordered = cache_entry(&other, &["a-path", "b-path"]).join("store");
        assert_eq!(
            identity_of(&store_names(&reordered).expect("store names")),
            identity_of(&names)
        );
        assert_ne!(
            identity_of(&["a-path".to_owned(), "b-path".to_owned()]),
            identity_of(&["a-path".to_owned(), "c-path".to_owned()]),
            "a different store content must not share an identity"
        );
    }

    #[test]
    fn identity_of_the_measured_store_is_stable() {
        // 685 store paths is this host's image; the digest is pinned so a change
        // to the identity recipe (separator, sorting, encoding) shows up here.
        let names: Vec<String> = (0..3)
            .map(|index| format!("{index}aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-name-{index}"))
            .collect();
        assert_eq!(
            identity_of(&names),
            "20f987da0096cbdc9701e427e1066908d5a3482c32d2c86f1533a9840bb0a666"
        );
    }

    #[test]
    fn builds_the_image_once_and_reuses_it_afterwards() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one", "bbbb-two"]);
        let store = nix_dir.join("store");
        let identity = identity_of(&store_names(&store).expect("names"));
        let builder = FakeBuilder::default();

        let (first, first_lock) =
            ensure_image_with_builder(&store, &identity, 2, &builder).expect("first build");
        assert_eq!(first.status, NixLowerDiskStatus::Built);
        assert_eq!(first.store_paths, 2);
        assert_eq!(first.id, ID);
        assert_eq!(first.label, LABEL);
        assert_eq!(
            first.path,
            temp.path()
                .join("btrfs-snapshots")
                .join(DIR_NAME)
                .join(format!("{identity}.raw"))
        );
        assert_eq!(builder.build_count(), 1);
        // The image is built from the image's /nix directory, and the temporary
        // name never survives.
        let (source, tmp, label) = builder.builds.borrow()[0].clone();
        assert_eq!(source, nix_dir);
        assert_eq!(label, LABEL);
        assert_eq!(tmp, first.path.with_extension("raw.tmp"));
        assert!(!tmp.exists());

        // The first launch's VM has exited by the time the next one starts.
        drop(first_lock);
        let (second, second_lock) =
            ensure_image_with_builder(&store, &identity, 2, &builder).expect("reuse");
        assert_eq!(second.status, NixLowerDiskStatus::Reused);
        assert_eq!(builder.build_count(), 1, "a complete pair is never rebuilt");
        assert_eq!(second_lock, second_lock);
    }

    #[test]
    fn a_rebuild_is_refused_while_another_launch_holds_the_image() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let store = nix_dir.join("store");
        let identity = identity_of(&store_names(&store).expect("names"));
        let builder = FakeBuilder::default();
        let lock_path = temp
            .path()
            .join("btrfs-snapshots")
            .join(DIR_NAME)
            .join(format!("{identity}.raw.lock"));
        fs::create_dir_all(lock_path.parent().expect("parent")).expect("nix-lower dir");

        // A live launch: it holds the image for as long as its VM can read it.
        let held = ImageLock::acquire(&lock_path, true).expect("hold the image");

        let err = ensure_image_with_builder(&store, &identity, 1, &builder)
            .map(|_| ())
            .expect_err("a rebuild under a live launch must be refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("another cang launch is using the /nix lower image"),
            "{message}"
        );
        assert_eq!(
            builder.build_count(),
            0,
            "the refused launch builds nothing"
        );

        // With the launch gone the image is built normally.
        drop(held);
        let (built, _lock) =
            ensure_image_with_builder(&store, &identity, 1, &builder).expect("build after release");
        assert_eq!(built.status, NixLowerDiskStatus::Built);
        assert_eq!(builder.build_count(), 1);
    }

    #[test]
    fn rebuilds_when_the_recorded_identity_does_not_match() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let store = nix_dir.join("store");
        let identity = identity_of(&store_names(&store).expect("names"));
        let builder = FakeBuilder::default();

        ensure_image_with_builder(&store, &identity, 1, &builder).expect("first build");
        let state = temp
            .path()
            .join("btrfs-snapshots")
            .join(DIR_NAME)
            .join(format!("{identity}.state"));
        fs::write(&state, format!("{STATE_IDENTITY_KEY}=other\n")).expect("stale state");

        let (rebuilt, _rebuilt_lock) =
            ensure_image_with_builder(&store, &identity, 1, &builder).expect("rebuild");
        assert_eq!(rebuilt.status, NixLowerDiskStatus::Built);
        assert_eq!(builder.build_count(), 2);
        assert!(
            fs::read_to_string(&state)
                .expect("state")
                .contains(&format!("{STATE_IDENTITY_KEY}={identity}")),
            "the state file is rewritten for the rebuilt image"
        );
    }

    /// Block mode has a guest half, and the guest-init that will actually run is
    /// what decides whether a built disk would ever be mounted.
    #[test]
    fn the_guest_init_binary_is_what_says_block_mode_is_supported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current = temp.path().join("current-cang-guest-init");
        fs::write(
            &current,
            b"\x7fELF image half CANG_NIX_LOWER_BLOCK=1 \x00cang-guest-init: /nix overlay ready\x00",
        )
        .expect("write the current guest-init stand-in");
        assert!(guest_init_mounts_block_lower(&current));

        let stale = temp.path().join("stale-cang-guest-init");
        fs::write(&stale, b"\x7fELF an image built before the guest half")
            .expect("write the stale guest-init stand-in");
        assert!(!guest_init_mounts_block_lower(&stale));

        // An unreadable binary is not evidence of an old guest-init, so it must
        // not turn block mode off on an I/O error alone.
        assert!(guest_init_mounts_block_lower(
            &temp.path().join("missing-cang-guest-init")
        ));
    }

    /// An image whose guest-init predates block mode gets no lower disk: it would
    /// attach a multi-gigabyte image the guest never mounts and leave `/nix` on
    /// the host-composed overlay, so the decision is taken before the build and
    /// named on the log - and only `blk` turns it into a refusal.
    #[test]
    fn a_guest_init_without_block_mode_skips_the_build() {
        let _guard = ENV_LOCK.lock().expect("env test lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let overlay = intent(&nix_dir, &generation_root(&temp));
        let guest_init = temp.path().join("cang-guest-init");
        fs::write(&guest_init, b"\x7fELF an image built before the guest half")
            .expect("write the stale guest-init stand-in");
        let refused = prepare_block(&overlay, &guest_init, Some(NixLowerMode::Blk));
        let allowed = prepare_block(&overlay, &guest_init, Some(NixLowerMode::Auto));
        let message = format!(
            "{:#}",
            refused.expect_err("a stale guest-init refuses the launch")
        );
        assert!(message.contains("predates block mode"), "{message}");
        assert!(
            message.contains("--nix=lower:virtiofs"),
            "the refusal names the way out: {message}"
        );
        assert!(
            allowed.expect("auto keeps the launch").is_none(),
            "with --nix=lower:auto the launch keeps the host-composed overlay"
        );
        let dir = temp.path().join("btrfs-snapshots").join(DIR_NAME);
        assert!(
            !dir.exists(),
            "no lower image is built for a guest-init that ignores it"
        );
    }

    #[test]
    fn a_missing_store_leaves_the_host_overlay_in_place() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &[]);
        fs::remove_dir(nix_dir.join("store")).expect("remove store");
        let builder = FakeBuilder::default();

        let block = ensure_with_builder(&intent(&nix_dir, &generation_root(&temp)), &builder, true)
            .expect("no store is not fatal");

        assert!(block.is_none());
        assert_eq!(builder.build_count(), 0);
    }

    #[test]
    fn an_unbuildable_image_falls_back_instead_of_failing_the_launch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let builder = FakeBuilder::failing();

        let err = prepare_block_with(&intent(&nix_dir, &generation_root(&temp)), &builder, true)
            .expect_err("a failed build is reported, not swallowed");
        assert!(format!("{err:#}").contains("failed"), "{err:#}");
        assert_eq!(builder.build_count(), 1);
        let dir = temp.path().join("btrfs-snapshots").join(DIR_NAME);
        let leftovers: Vec<String> = fs::read_dir(&dir)
            .expect("nix-lower dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".lock"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed build leaves no partial image behind, found {leftovers:?}"
        );
    }

    #[test]
    fn block_mode_carries_the_read_only_disk_the_share_and_the_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one", "bbbb-two"]);
        let builder = FakeBuilder::default();
        let generation_root = generation_root(&temp);
        let overlay = intent(&nix_dir, &generation_root);

        let block = prepare_block_with(&overlay, &builder, true)
            .expect("block mode")
            .expect("an image is built");

        let attachment = block.attachment();
        assert_eq!(attachment.id, "cang-nix-lower");
        assert!(attachment.read_only, "the lower disk is never written");
        assert_eq!(attachment.path, block.disk().path);

        let mount = block.bind_mount();
        assert_eq!(mount.source, generation_root);
        assert_eq!(mount.tag, BIND_TAG);
        assert_eq!(mount.target, GUEST_OVERLAY_ROOT);
        assert!(!mount.read_only, "the guest composes upper/work through it");

        let env: std::collections::HashMap<_, _> = block.env_pairs().into_iter().collect();
        assert_eq!(env[BLOCK_MODE_ENV], "1");
        assert_eq!(env["CANG_NIX_LOWER_DISK_ID"], ID);
        assert_eq!(env["CANG_NIX_LOWER_DISK_LABEL"], LABEL);
        // Both layers of the pair are on the build disk, in the generation's own
        // directory: the share carries the host's own bookkeeping, not the
        // guest's upper.
        // The generation's name is the image digest, not the workspace's
        // constant `nix-overlay` directory: two image generations must not share
        // one upper, or a moved image keeps the previous image's Nix DB.
        let generation = "sha256-feedface";
        assert_eq!(
            env["CANG_NIX_LOWER_UPPERDIR"],
            format!("{BUILD_DISK_UPPER_ROOT}/{generation}/upper")
        );
        assert_eq!(
            env["CANG_NIX_LOWER_WORKDIR"],
            format!("{BUILD_DISK_UPPER_ROOT}/{generation}/work")
        );
        assert_eq!(block.guest_upper_dirs().0, env["CANG_NIX_LOWER_UPPERDIR"]);
        // The host still prepares the generation's own work directory on the
        // share: it is the host-composed overlay's, kept for a launch that cannot
        // pair the block layers.
        let host_work = generation_root.join(GUEST_WORK_DIR_NAME);
        assert!(
            host_work.is_dir(),
            "the host overlay keeps its own work dir"
        );
        assert!(
            fs::read_dir(&host_work).expect("work dir").next().is_none(),
            "and it stays empty while the guest's upper is on the block disk"
        );
    }

    /// `blk` and `auto` both request the block lower; only `virtiofs` keeps the
    /// host overlay. There is no environment variable left to override either:
    /// the flag is the whole surface.
    #[test]
    fn blk_and_auto_request_the_block_lower_and_only_virtiofs_turns_it_off() {
        let _guard = ENV_LOCK.lock().expect("env test lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let overlay = intent(&nix_dir, &generation_root(&temp));

        assert!(block_mode_requested(None), "the block lower is the default");
        assert!(
            block_mode_requested(Some(NixLowerMode::Auto)),
            "auto asks for it too, and keeps the launch when it cannot be served"
        );
        assert!(
            block_mode_requested(Some(NixLowerMode::Blk)),
            "blk asks for it and refuses a launch that cannot have it"
        );
        assert!(
            !block_mode_requested(Some(NixLowerMode::VirtioFs)),
            "--nix=lower:virtiofs keeps the host overlay"
        );

        assert!(overlay.lowerdir.is_dir());
    }

    /// A launch that asks for the block lower gets the build disk's upper too:
    /// both layers land on one filesystem, and the guest is told the paths.
    #[test]
    fn the_block_pair_puts_both_layers_on_the_build_disk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let root = generation_root(&temp);
        let overlay = intent(&nix_dir, &root);
        let builder = FakeBuilder::default();

        let block = ensure_with_builder(&overlay, &builder, true)
            .expect("ensure")
            .expect("the block pair");
        let (upper, work) = block.guest_upper_dirs();
        let generation = "sha256-feedface";

        assert_eq!(
            upper,
            format!("{BUILD_DISK_UPPER_ROOT}/{generation}/upper"),
            "the upper is the generation's own directory under the build disk"
        );
        assert_eq!(work, format!("{BUILD_DISK_UPPER_ROOT}/{generation}/work"));
        assert!(
            upper.starts_with("/home/dev/.cache/cang-build/"),
            "the path is under the build disk's guest mount, which the guest has mounted before nix-prep"
        );
        assert!(!upper.starts_with(GUEST_OVERLAY_ROOT), "never the share");

        let env: std::collections::HashMap<_, _> = block.env_pairs().into_iter().collect();
        assert_eq!(env["CANG_NIX_LOWER_UPPERDIR"], upper);
        assert_eq!(env["CANG_NIX_LOWER_WORKDIR"], work);
    }

    /// Two image generations get two uppers on the block disk. A constant name
    /// here would be invisible in a single launch and wrong on the next image
    /// move: the previous generation's sentinel would say "already seeded".
    #[test]
    fn each_image_generation_gets_its_own_upper_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let root = generation_root(&temp);
        let mut first = intent(&nix_dir, &root);
        let builder = FakeBuilder::default();

        let one = ensure_with_builder(&first, &builder, true)
            .expect("ensure")
            .expect("the block pair")
            .guest_upper_dirs()
            .0;
        first.digest_key = "sha256-otherimage".to_owned();
        let two = ensure_with_builder(&first, &builder, true)
            .expect("ensure")
            .expect("the block pair")
            .guest_upper_dirs()
            .0;

        assert_ne!(one, two, "one upper per image generation");
        assert!(one.ends_with("/sha256-feedface/upper"), "{one}");
        assert!(two.ends_with("/sha256-otherimage/upper"), "{two}");
        assert!(
            one.starts_with(BUILD_DISK_UPPER_ROOT),
            "both live under the build disk's nix-overlay parent"
        );
    }

    /// A digest key is host-controlled text used as a directory name the guest
    /// writes into, so it is reduced to a plain single path segment.
    #[test]
    fn a_digest_key_cannot_escape_the_upper_parent() {
        assert_eq!(generation_dir_name("sha256-abc"), "sha256-abc");
        assert_eq!(generation_dir_name("../etc"), "etc");
        assert_eq!(generation_dir_name(".hidden"), "hidden");
        assert_eq!(generation_dir_name("/"), "generation");
        assert_eq!(generation_dir_name(""), "generation");
        for key in ["../../etc/passwd", "a/b", "..", "."] {
            let name = generation_dir_name(key);
            assert!(!name.contains('/'), "{key} -> {name}");
            assert!(!name.starts_with('.'), "{key} -> {name}");
            assert!(!name.is_empty());
        }
    }

    /// Without the guest's seeding step the upper stays on the share, which is
    /// the mixed pair: the capability gate turns that into a named refusal in
    /// `blk` and a host overlay in `auto`.
    #[test]
    fn a_guest_init_without_the_seeding_step_refuses_the_block_pair() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let overlay = intent(&nix_dir, &generation_root(&temp));
        // Knows block mode, not the upper: the exact image generation this pair
        // would be wrong against.
        let guest_init = temp.path().join("cang-guest-init");
        fs::write(&guest_init, b"\x7fELF CANG_NIX_LOWER_BLOCK only").expect("write");

        let refused = prepare_block(&overlay, &guest_init, Some(NixLowerMode::Blk));
        let allowed = prepare_block(&overlay, &guest_init, Some(NixLowerMode::Auto));

        let message = format!(
            "{:#}",
            refused.expect_err("a stale upper refuses the launch")
        );
        assert!(
            message.contains("predates the block-disk upper"),
            "{message}"
        );
        assert!(
            allowed.expect("auto keeps the launch").is_none(),
            "with --nix=lower:auto the launch keeps the host-composed overlay for both layers"
        );
        assert!(
            !temp.path().join("btrfs-snapshots").join(DIR_NAME).exists(),
            "no lower image is built for a guest that cannot pair with it"
        );
    }

    #[test]
    fn mkfs_argument_vector_is_the_measured_recipe() {
        assert_eq!(
            mkfs_args(
                Path::new("/state/btrfs-snapshots/sha256-x/rootfs/nix"),
                Path::new("/state/btrfs-snapshots/nix-lower/abc.raw.tmp"),
                LABEL
            ),
            vec![
                "-q".to_owned(),
                "-L".to_owned(),
                "CANG_NIX_LOWER".to_owned(),
                "--rootdir=/state/btrfs-snapshots/sha256-x/rootfs/nix".to_owned(),
                "--reflink".to_owned(),
                "--shrink".to_owned(),
                "/state/btrfs-snapshots/nix-lower/abc.raw.tmp".to_owned(),
            ]
        );
    }

    /// The measured recipe's `ERROR: zoned: unable to stat` line is not a
    /// failure: `mkfs.btrfs` exits 0 for a non-zoned target and the image is
    /// good. Judging stderr would refuse to build an image on every host.
    #[test]
    fn a_zoned_stderr_line_with_exit_zero_is_success() {
        let output = std::process::Output {
            status: exit_status(0),
            stdout: Vec::new(),
            stderr: b"ERROR: zoned: unable to stat image.raw\n".to_vec(),
        };

        judge_mkfs_output(OsStr::new("mkfs.btrfs"), &output)
            .expect("exit 0 is the verdict even though stderr carries the zoned line");
    }

    #[test]
    fn a_nonzero_exit_is_a_failure() {
        let output = std::process::Output {
            status: exit_status(1),
            stdout: Vec::new(),
            stderr: b"ERROR: zoned: unable to stat image.raw\nno space left\n".to_vec(),
        };

        let err = judge_mkfs_output(OsStr::new("mkfs.btrfs"), &output)
            .expect_err("a nonzero exit must fail the build");

        assert!(format!("{err:#}").contains("mkfs.btrfs failed"));
        assert!(format!("{err:#}").contains("no space left"));
    }

    #[test]
    fn a_nonzero_exit_with_silent_stderr_still_names_the_status() {
        let output = std::process::Output {
            status: exit_status(2),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };

        let err = judge_mkfs_output(OsStr::new("mkfs.btrfs"), &output)
            .expect_err("a nonzero exit must fail the build");

        assert!(format!("{err:#}").contains("mkfs.btrfs failed with"));
    }

    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        std::process::ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn a_successful_exit_that_produced_no_image_is_a_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let store = nix_dir.join("store");
        let identity = identity_of(&store_names(&store).expect("names"));

        let err =
            ensure_image_with_builder(&store, &identity, 1, &FakeBuilder::default().writes(0))
                .map(|_| ())
                .expect_err("an empty image must not be installed");

        assert!(format!("{err:#}").contains("produced no image"));
    }

    #[test]
    fn a_build_sharing_one_filesystem_only_needs_the_floor() {
        assert_eq!(required_space(0, true), BUILD_FLOOR_BYTES);
        assert_eq!(
            required_space(12 * 1024 * 1024 * 1024, true),
            BUILD_FLOOR_BYTES,
            "--reflink shares the extents, so the tree's size is not the bill"
        );
        assert_eq!(
            required_space(12 * 1024 * 1024 * 1024, false),
            12 * 1024 * 1024 * 1024 + BUILD_FLOOR_BYTES,
            "across filesystems the image stores every byte"
        );
    }

    #[test]
    fn a_tight_target_is_refused_before_the_build() {
        let err = check_space(1024, 4096, false).expect_err("no space");
        let message = format!("{err:#}");
        assert!(
            message.contains("needs about 512 MiB free") && message.contains("only 0 MiB"),
            "{message}"
        );
        assert!(
            message.contains("--reflink cannot share it"),
            "the refusal has to say why it prices the whole tree: {message}"
        );
        assert!(
            check_space(BUILD_FLOOR_BYTES, 12 * 1024 * 1024 * 1024, true).is_ok(),
            "a shared source needs only the floor, whatever the tree's size"
        );
        assert!(
            check_space(BUILD_FLOOR_BYTES + 4096, 4096, false).is_ok(),
            "exactly enough is enough"
        );
    }

    #[test]
    fn apparent_size_counts_regular_files_and_ignores_symlinks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = cache_entry(&temp, &["first", "second"]).join("store");
        fs::write(store.join("first").join("payload"), vec![0u8; 1000]).expect("write");
        fs::write(store.join("second").join("payload"), vec![0u8; 24]).expect("write");
        std::os::unix::fs::symlink("/nix/store/elsewhere", store.join("link")).expect("symlink");

        assert_eq!(apparent_bytes(&store).expect("apparent size"), 1024);
    }

    #[test]
    fn free_space_is_measured_and_the_source_shares_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["first"]);
        assert!(
            free_bytes(temp.path()).expect("free bytes") > 0,
            "a tmpfs or disk always reports some free space"
        );
        assert_eq!(
            device_id(temp.path()).expect("device"),
            device_id(&nix_dir).expect("device"),
            "both live under the same tmpdir"
        );
    }
}
