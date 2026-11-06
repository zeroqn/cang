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
//! # Status: opt-in, and the guest half does not currently mount
//!
//! Block mode is requested with `CANG_NIX_LOWER_BLOCK=1` and is **off by
//! default**. The reason is a hard kernel requirement, measured in the guest
//! (`7.2.10-hardened1`): kernel overlayfs refuses a virtio-fs directory as its
//! upper/work - `overlayfs: upper fs does not support tmpfile.`,
//! `overlayfs: upper fs does not support RENAME_WHITEOUT.`,
//! `overlayfs: upper fs missing required features.` (mount exits 32). So the
//! guest cannot compose a `/nix` overlay whose upper and work are the host
//! directories this design deliberately keeps (dedup with the host's own
//! `/nix/store`), and it falls back to the host-composed overlay. The host half
//! below (identity, reflinked image, read-only attach, env) works and is
//! verified: the guest mounts the image read-only at `/run/cang/nix-lower` and
//! reads it 1.9-8.2x faster than the virtio-fs lower, but only through that
//! mount, not through `/nix`.

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
pub(crate) const GUEST_UPPER_DIR: &str = "/run/cang/nix-overlay/upper";
pub(crate) const GUEST_WORK_DIR: &str = "/run/cang/nix-overlay/work-guest";
const BIND_TAG: &str = "cang-nix-overlay-src";
const BLOCK_MODE_ENV: &str = "CANG_NIX_LOWER_BLOCK";
/// The host-side switch. Block mode is **opt-in** (`CANG_NIX_LOWER_BLOCK=1`)
/// rather than the default, because the guest half is measurably impossible with
/// this guest kernel: kernel overlayfs refuses a virtio-fs directory as its upper
/// (`upper fs does not support tmpfile` / `does not support RENAME_WHITEOUT` /
/// `upper fs missing required features`), so a guest-composed `/nix` overlay over
/// the block lower cannot be mounted while `upper`/`work` stay host directories.
/// Until that is redesigned, a default-on block mode would only pay the image
/// build and fall straight back to the host-composed overlay on every launch.
pub(crate) const HOST_BLOCK_MODE_ENV: &str = BLOCK_MODE_ENV;
const STATE_LABEL_KEY: &str = "label";
const STATE_IDENTITY_KEY: &str = "identity";
const STATE_STORE_PATHS_KEY: &str = "store_paths";
const STATE_BUILT_AT_KEY: &str = "built_at";

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
                GUEST_UPPER_DIR.to_owned(),
            ),
            (
                "CANG_NIX_LOWER_WORKDIR".to_owned(),
                GUEST_WORK_DIR.to_owned(),
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

/// Best-effort entry point: the launch falls back to the host-composed overlay
/// whenever this returns `None`, and never fails because of it.
pub(crate) fn prepare_block(intent: &HostNixOverlay) -> Option<NixLowerBlock> {
    if !block_mode_requested() {
        tracing::debug!(
            "{HOST_BLOCK_MODE_ENV} is not 1: keeping the host-composed /nix overlay on every layer"
        );
        return None;
    }
    prepare_block_with(intent, &HostNixLowerImageBuilder)
}

fn prepare_block_with(
    intent: &HostNixOverlay,
    builder: &impl NixLowerImageBuilder,
) -> Option<NixLowerBlock> {
    match ensure_with_builder(intent, builder) {
        Ok(Some(block)) => Some(block),
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                lowerdir = %intent.lowerdir.display(),
                error = %format!("{err:#}"),
                "cang could not build the /nix block lower; the guest keeps the host-composed overlay"
            );
            eprintln!(
                "cang: /nix block lower unavailable, using the host-composed overlay: {err:#}"
            );
            None
        }
    }
}

fn block_mode_requested() -> bool {
    matches!(std::env::var(HOST_BLOCK_MODE_ENV).as_deref(), Ok("1"))
}

pub(super) fn ensure_with_builder(
    intent: &HostNixOverlay,
    builder: &impl NixLowerImageBuilder,
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
    let disk = ensure_image_with_builder(&store_dir, &identity, names.len(), builder)?;

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
) -> Result<NixLowerDisk> {
    let dir = nix_lower_dir(store_dir)?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create '{}'", dir.display()))?;
    let image = dir.join(format!("{identity}.{RAW_SUFFIX}"));
    let state = dir.join(format!("{identity}.{STATE_SUFFIX}"));

    if complete(&image, &state, identity) {
        return Ok(NixLowerDisk {
            path: image,
            id: ID.to_owned(),
            label: LABEL.to_owned(),
            identity: identity.to_owned(),
            store_paths,
            status: NixLowerDiskStatus::Reused,
        });
    }

    let tmp = dir.join(format!("{identity}.{RAW_SUFFIX}.tmp"));
    remove_if_present(&tmp)?;
    let source = store_dir
        .parent()
        .ok_or_else(|| anyhow!("image store '{}' has no parent", store_dir.display()))?
        .to_path_buf();
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

    Ok(NixLowerDisk {
        path: image,
        id: ID.to_owned(),
        label: LABEL.to_owned(),
        identity: identity.to_owned(),
        store_paths,
        status: NixLowerDiskStatus::Built,
    })
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

        let first = ensure_image_with_builder(&store, &identity, 2, &builder).expect("first build");
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

        let second = ensure_image_with_builder(&store, &identity, 2, &builder).expect("reuse");
        assert_eq!(second.status, NixLowerDiskStatus::Reused);
        assert_eq!(builder.build_count(), 1, "a complete pair is never rebuilt");
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

        let rebuilt = ensure_image_with_builder(&store, &identity, 1, &builder).expect("rebuild");
        assert_eq!(rebuilt.status, NixLowerDiskStatus::Built);
        assert_eq!(builder.build_count(), 2);
        assert!(
            fs::read_to_string(&state)
                .expect("state")
                .contains(&format!("{STATE_IDENTITY_KEY}={identity}")),
            "the state file is rewritten for the rebuilt image"
        );
    }

    #[test]
    fn a_missing_store_leaves_the_host_overlay_in_place() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &[]);
        fs::remove_dir(nix_dir.join("store")).expect("remove store");
        let builder = FakeBuilder::default();

        let block = ensure_with_builder(&intent(&nix_dir, &generation_root(&temp)), &builder)
            .expect("no store is not fatal");

        assert!(block.is_none());
        assert_eq!(builder.build_count(), 0);
    }

    #[test]
    fn an_unbuildable_image_falls_back_instead_of_failing_the_launch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let builder = FakeBuilder::failing();

        let block = prepare_block_with(&intent(&nix_dir, &generation_root(&temp)), &builder);

        assert!(block.is_none(), "a failed build must not fail the launch");
        assert_eq!(builder.build_count(), 1);
        let dir = temp.path().join("btrfs-snapshots").join(DIR_NAME);
        assert!(
            fs::read_dir(&dir).expect("nix-lower dir").next().is_none(),
            "a failed build leaves no partial image behind"
        );
    }

    #[test]
    fn block_mode_carries_the_read_only_disk_the_share_and_the_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one", "bbbb-two"]);
        let builder = FakeBuilder::default();
        let generation_root = generation_root(&temp);
        let overlay = intent(&nix_dir, &generation_root);

        let block = prepare_block_with(&overlay, &builder).expect("block mode");

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
        assert_eq!(env["CANG_NIX_LOWER_UPPERDIR"], GUEST_UPPER_DIR);
        assert_eq!(env["CANG_NIX_LOWER_WORKDIR"], GUEST_WORK_DIR);
        assert_eq!(
            env["CANG_NIX_LOWER_UPPERDIR"],
            format!("{GUEST_OVERLAY_ROOT}/upper")
        );
        assert_eq!(
            env["CANG_NIX_LOWER_WORKDIR"],
            format!("{GUEST_OVERLAY_ROOT}/{GUEST_WORK_DIR_NAME}")
        );
        // The guest's own work directory is created empty on the shared
        // generation root, next to the host overlay's.
        let guest_work = generation_root.join(GUEST_WORK_DIR_NAME);
        assert!(guest_work.is_dir());
        assert!(
            fs::read_dir(&guest_work)
                .expect("work dir")
                .next()
                .is_none()
        );
    }

    /// Block mode is opt-in: the default launch keeps the host-composed overlay
    /// and pays nothing for an image it cannot use.
    #[test]
    fn block_mode_is_off_unless_the_host_asks_for_it() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK.lock().expect("env test lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let nix_dir = cache_entry(&temp, &["aaaa-one"]);
        let overlay = intent(&nix_dir, &generation_root(&temp));
        let before = std::env::var_os(HOST_BLOCK_MODE_ENV);

        // SAFETY: a single-threaded assertion over one process env variable,
        // serialized by ENV_LOCK.
        unsafe { std::env::remove_var(HOST_BLOCK_MODE_ENV) };
        assert!(!block_mode_requested(), "block mode must default to off");

        // SAFETY: as above.
        unsafe { std::env::set_var(HOST_BLOCK_MODE_ENV, "1") };
        assert!(block_mode_requested());

        // SAFETY: as above.
        unsafe { std::env::set_var(HOST_BLOCK_MODE_ENV, "0") };
        assert!(!block_mode_requested(), "0 must be as off as absent");

        // SAFETY: restore whatever the harness had.
        unsafe {
            match before {
                Some(value) => std::env::set_var(HOST_BLOCK_MODE_ENV, value),
                None => std::env::remove_var(HOST_BLOCK_MODE_ENV),
            }
        }
        assert!(overlay.lowerdir.is_dir());
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
                .expect_err("an empty image must not be installed");

        assert!(format!("{err:#}").contains("produced no image"));
    }
}
