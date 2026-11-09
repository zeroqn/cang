//! Workspace-scoped host kernel-overlayfs ownership for guest `/nix`.
//!
//! Session code acquires the workspace lease and serializes the intent; the VM
//! worker materializes the mount in the keep-id user namespace (unshare
//! --map-users + --setuid 0 --keep-caps) that will also prepare the libkrun
//! root graft. Namespace root capabilities (CAP_SYS_ADMIN, CAP_DAC_OVERRIDE,
//! CAP_CHOWN) satisfy overlayfs requirements while the identity mapping
//! host_uid:host_uid:1 preserves file ownership visibility into the guest VM.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runtime::launch::components::persistent_disks::nix_lower::GUEST_WORK_DIR_NAME;
use crate::runtime::launch::config::HostNixOverlay;
use crate::runtime::session::rootfs::task::TaskRootfsHandle;

const OVERLAY_DIR: &str = "nix-overlay";
const SEED_TMP_DIR: &str = "nix-overlay.new";
const SEED_STALE_DIR: &str = "nix-overlay.previous";
const SEED_RETIRED_PREFIX: &str = "nix-overlay.retired.";
/// Where a seeded generation parks the previous one's Nix DB for the guest to
/// merge: `.cang-previous-db` directly under the overlay upper, which the guest
/// sees as `/nix/.cang-previous-db`. It sits at the upper's root rather than
/// under `var/nix` because the host owns that root and can always create there,
/// while `var/nix` is the guest's once a guest has run nix - and a snapshot
/// preserves that ownership.
pub(crate) const PARKED_DB_DIR: &str = ".cang-previous-db";
/// Where the new image generation's DB is staged for the guest to install.
///
/// The host cannot write inside `upper/var/nix` once a guest has run: those
/// directories belong to the guest's uid, and a snapshot preserves that. So the
/// host leaves both DBs at the upper's root - the one directory it owns - and the
/// guest, root inside the VM, does the swap before `nix-daemon` starts.
pub(crate) const IMAGE_DB_DIR: &str = ".cang-image-db";
const GENERATION_KEY: &str = "generation";
const UPPER_DIR: &str = "upper";
const WORK_DIR: &str = "work";
const MERGED_DIR: &str = "merged";
// The lease files are siblings of the generation directory, not children of it:
// a new image generation replaces that directory wholesale (by btrfs snapshot),
// and a lock file that moves with it would let two launches hold two locks.
const LEASE_LOCK: &str = "nix-overlay.lease.lock";
const LEASE_STATE: &str = "nix-overlay.lease.state";
const CACHE_ROOTFS_DIR: &str = "rootfs";
const NIX_DIR: &str = "nix";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NixOverlayPaths {
    pub(crate) root: PathBuf,
    pub(crate) upperdir: PathBuf,
    pub(crate) workdir: PathBuf,
    /// The work dir a *guest*-composed overlay over the same upper uses when the
    /// lower comes from a block device: overlayfs insists on upper and work
    /// sharing a filesystem, and two overlay instances may not share one work
    /// dir, so the guest gets its own beside the host's.
    pub(crate) guest_workdir: PathBuf,
    pub(crate) mergeddir: PathBuf,
    pub(crate) lease_lock: PathBuf,
    pub(crate) lease_state: PathBuf,
}

impl NixOverlayPaths {
    pub(crate) fn new(workspace_state_root: &Path) -> Self {
        let root = workspace_state_root.join(OVERLAY_DIR);
        Self {
            upperdir: root.join(UPPER_DIR),
            workdir: root.join(WORK_DIR),
            guest_workdir: root.join(GUEST_WORK_DIR_NAME),
            mergeddir: root.join(MERGED_DIR),
            lease_lock: workspace_state_root.join(LEASE_LOCK),
            lease_state: workspace_state_root.join(LEASE_STATE),
            root,
        }
    }
}

pub(crate) struct NixOverlayLease {
    intent: HostNixOverlay,
    seed: UpperSeedOutcome,
    _lock_file: File,
}

impl NixOverlayLease {
    pub(crate) fn acquire(workspace_state_root: &Path, handle: &TaskRootfsHandle) -> Result<Self> {
        let paths = NixOverlayPaths::new(workspace_state_root);
        fs::create_dir_all(workspace_state_root).with_context(|| {
            format!(
                "failed to create cang workspace state root '{}'",
                workspace_state_root.display()
            )
        })?;
        // One generation is one directory holding `upper`, `work` and `merged`,
        // and on btrfs that directory is a subvolume: a new generation is then a
        // `btrfs subvolume snapshot` of it (constant time, extents shared)
        // instead of a per-file clone, and `upper`/`work` stay on one
        // filesystem, which overlayfs requires.
        HostUpperSeedCommands.create_generation_root(&paths.root)?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&paths.lease_lock)
            .with_context(|| {
                format!(
                    "failed to open cang host /nix overlay lease '{}'",
                    paths.lease_lock.display()
                )
            })?;
        lock_exclusive(&lock_file).with_context(|| {
            format!(
                "failed to acquire cang host /nix overlay lease '{}'",
                paths.lease_lock.display()
            )
        })?;

        let intent = intent_from_task_rootfs(&paths, handle)?;
        let seed = prepare_upper_with(&paths, &intent, &HostUpperSeedCommands)?;
        record_state(&paths.lease_state, &intent)?;
        Ok(Self {
            intent,
            seed,
            _lock_file: lock_file,
        })
    }

    pub(crate) fn intent(&self) -> &HostNixOverlay {
        &self.intent
    }

    pub(crate) fn seed(&self) -> UpperSeedOutcome {
        self.seed
    }
}

pub(crate) fn materialize_in_worker(intent: &HostNixOverlay) -> Result<NixOverlayWorkerMount> {
    materialize_in_worker_with(intent, &HostNixOverlayCommands)
}

fn materialize_in_worker_with(
    intent: &HostNixOverlay,
    commands: &impl NixOverlayCommands,
) -> Result<NixOverlayWorkerMount> {
    prepare_and_mount(intent, commands)?;
    Ok(NixOverlayWorkerMount {
        intent: intent.clone(),
        mounted: true,
    })
}

fn prepare_and_mount(intent: &HostNixOverlay, commands: &impl NixOverlayCommands) -> Result<()> {
    validate_absolute_intent(intent)?;
    commands.create_dir_all(&intent.upperdir)?;
    commands.create_dir_all(&intent.workdir)?;
    commands.create_dir_all(&intent.mergeddir)?;
    commands.mount_overlay(intent)
}

#[cfg(test)]
fn unmount_with(intent: &HostNixOverlay, commands: &impl NixOverlayCommands) -> Result<()> {
    commands.unmount_overlay(intent)
}

pub(crate) struct NixOverlayWorkerMount {
    intent: HostNixOverlay,
    mounted: bool,
}

impl NixOverlayWorkerMount {
    pub(crate) fn unmount(mut self) -> Result<()> {
        if self.mounted {
            HostNixOverlayCommands.unmount_overlay(&self.intent)?;
            self.mounted = false;
        }
        Ok(())
    }
}

impl Drop for NixOverlayWorkerMount {
    fn drop(&mut self) {
        if !self.mounted {
            return;
        }
        if let Err(err) = HostNixOverlayCommands.unmount_overlay(&self.intent) {
            eprintln!(
                "cang: best-effort host /nix overlay unmount failed for '{}': {err:#}",
                self.intent.mergeddir.display()
            );
        }
    }
}

trait NixOverlayCommands {
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn mount_overlay(&self, intent: &HostNixOverlay) -> Result<()>;
    fn unmount_overlay(&self, intent: &HostNixOverlay) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct HostNixOverlayCommands;

impl NixOverlayCommands for HostNixOverlayCommands {
    fn create_dir_all(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).with_context(|| {
            format!(
                "failed to create cang host /nix overlay path '{}'",
                path.display()
            )
        })
    }

    fn mount_overlay(&self, intent: &HostNixOverlay) -> Result<()> {
        let options = overlay_options(intent);
        let status = Command::new("mount")
            .args(["-t", "overlay", "overlay", "-o", &options])
            .arg(&intent.mergeddir)
            .status()
            .with_context(|| {
                format!(
                    "failed to run cang host /nix overlay mount inside buildah-unshare VM worker namespace for '{}'",
                    intent.mergeddir.display()
                )
            })?;
        if !status.success() {
            bail!(
                "cang host /nix overlay mount inside buildah-unshare VM worker namespace lowerdir='{}' upperdir='{}' workdir='{}' merged='{}' failed with {status}; verify the helper was launched through `buildah unshare`, buildah is installed on PATH, and kernel overlayfs is allowed in the rootless Buildah user namespace",
                intent.lowerdir.display(),
                intent.upperdir.display(),
                intent.workdir.display(),
                intent.mergeddir.display()
            );
        }
        Ok(())
    }

    fn unmount_overlay(&self, intent: &HostNixOverlay) -> Result<()> {
        umount_path(&intent.mergeddir).with_context(|| {
            format!(
                "cang host /nix overlay unmount inside buildah-unshare VM worker namespace '{}' failed; retaining lease/state for diagnostics and manual cleanup with `buildah unshare umount <merged>`",
                intent.mergeddir.display()
            )
        })
    }
}

fn intent_from_task_rootfs(
    paths: &NixOverlayPaths,
    handle: &TaskRootfsHandle,
) -> Result<HostNixOverlay> {
    let cache_profile = handle.cache_profile();
    let cache_path = cache_profile.cache_path.as_ref().ok_or_else(|| {
        anyhow!(
            "cang host /nix overlay requires a digest-keyed image cache lowerdir; selected image '{}' was not cached: {}",
            handle.selected_image_reference(),
            cache_profile
                .uncached_reason
                .as_deref()
                .unwrap_or("missing-cache-profile")
        )
    })?;
    let digest_key = cache_profile.digest_key.clone().ok_or_else(|| {
        anyhow!(
            "cang host /nix overlay requires selected image cache digest key for '{}'",
            handle.selected_image_reference()
        )
    })?;
    let image_digest = handle.image_digest().ok_or_else(|| {
        anyhow!(
            "cang host /nix overlay requires selected image digest for '{}'",
            handle.selected_image_reference()
        )
    })?;
    let lowerdir = cache_path.join(CACHE_ROOTFS_DIR).join(NIX_DIR);
    if !lowerdir.is_dir() {
        bail!(
            "cang host /nix overlay lowerdir '{}' is missing; selected image cache entry for '{}' does not contain /nix",
            lowerdir.display(),
            handle.selected_image_reference()
        );
    }
    Ok(HostNixOverlay {
        selected_reference: handle.selected_image_reference().to_owned(),
        image_digest: image_digest.to_owned(),
        digest_key,
        lowerdir,
        upperdir: paths.upperdir.clone(),
        workdir: paths.workdir.clone(),
        mergeddir: paths.mergeddir.clone(),
    })
}

/// What `prepare_upper` did to the workspace overlay upper for this launch.
///
/// A workspace `/nix` overlays one image generation's `lowerdir` with an upper
/// that outlives it. Reusing the upper across generations is what keeps every
/// substituted object cached, but it also leaves the DB describing a store that
/// no longer exists: the copied-up DB still claims paths only the previous
/// image supplied, and Nix never re-substitutes a path it calls valid. The seed
/// keeps the cache and removes the contradiction — the new generation is a
/// clone of the previous upper (hardlinks, or a btrfs snapshot), and its DB is
/// the new image's, with the previous generation's registrations parked for the
/// guest to merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpperSeedOutcome {
    /// Same image generation as the recorded one; the upper is used as is.
    Reused,
    /// No upper existed yet; an empty one was created.
    Fresh,
    /// A new image generation: the previous upper was cloned, its DB parked and
    /// the new image's DB installed.
    Seeded,
}

fn prepare_upper_with(
    paths: &NixOverlayPaths,
    intent: &HostNixOverlay,
    commands: &impl UpperSeedCommands,
) -> Result<UpperSeedOutcome> {
    if !paths.upperdir.is_dir() {
        commands.create_empty_upper(&paths.upperdir)?;
        return Ok(UpperSeedOutcome::Fresh);
    }
    let prior = fs::read_to_string(&paths.lease_state)
        .ok()
        .map(|text| parse_state(&text));
    let prior_generation = prior
        .as_ref()
        .and_then(|state| state.get(GENERATION_KEY))
        .map(String::as_str);
    if prior_generation == Some(generation_of(intent).as_str()) {
        return Ok(UpperSeedOutcome::Reused);
    }
    seed_from_previous(paths, intent, commands)?;
    Ok(UpperSeedOutcome::Seeded)
}

fn generation_of(intent: &HostNixOverlay) -> String {
    format!("{}|{}", intent.image_digest, intent.lowerdir.display())
}

fn seed_from_previous(
    paths: &NixOverlayPaths,
    intent: &HostNixOverlay,
    commands: &impl UpperSeedCommands,
) -> Result<()> {
    let tmp = paths.root.with_file_name(SEED_TMP_DIR);
    let stale = paths.root.with_file_name(SEED_STALE_DIR);
    // A previous seed that died mid-swap leaves these behind; they are only ever
    // copies, so dropping them loses nothing. Reclaiming a *used* generation is
    // best effort: after a guest ran nix-daemon it holds directories owned by the
    // guest's uid, which an unprivileged host process cannot delete (a btrfs
    // subvolume delete is the exception, and the hardlink fallback renames them
    // aside instead).
    commands.remove_tree(&tmp)?;
    reclaim_stale_generations(paths, &stale, commands);

    commands.clone_generation(&paths.root, &tmp)?;

    // The new generation is a copy of the previous one, so it carries that
    // generation's DB. Park it for the guest to merge - its rows are what keep
    // every cloned store object addressable by Nix - and install the image's own.
    let upper = tmp.join(UPPER_DIR);
    let db = upper.join("var/nix/db");
    if db.is_dir() {
        let parked = upper.join(PARKED_DB_DIR);
        commands.remove_tree(&parked)?;
        fs::create_dir_all(&parked).with_context(|| {
            format!(
                "failed to create the parked /nix DB directory '{}'",
                parked.display()
            )
        })?;
        for name in ["db.sqlite", "db.sqlite-wal", "schema"] {
            let from = db.join(name);
            if from.is_file() {
                fs::copy(&from, parked.join(name)).with_context(|| {
                    format!(
                        "failed to park the previous /nix DB '{}' as '{}'",
                        from.display(),
                        parked.display()
                    )
                })?;
            }
        }
        // The live `var/nix/db` is left in place: it belongs to the guest's uid
        // and holds the previous generation's rows until the guest replaces it.
    }
    // Stage the image's DB next to the parked one; the guest installs it, wipes
    // the dead daemon's socket and temproots, and merges the parked rows.
    commands.install_image_db(&intent.lowerdir, &upper.join(IMAGE_DB_DIR))?;

    fs::rename(&paths.root, &stale).with_context(|| {
        format!(
            "failed to retire the previous /nix overlay generation '{}'",
            paths.root.display()
        )
    })?;
    fs::rename(&tmp, &paths.root).with_context(|| {
        format!(
            "failed to install the seeded /nix overlay generation '{}'",
            paths.root.display()
        )
    })?;
    reclaim_stale_generations(paths, &stale, commands);
    // `work`, the guest's work dir and `merged` came along with the snapshot;
    // overlayfs wants an empty pair for the new mount.
    for dir in [&paths.workdir, &paths.guest_workdir, &paths.mergeddir] {
        commands.reset_work(dir)?;
    }
    tracing::info!(
        generation = %paths.root.display(),
        image_digest = %intent.image_digest,
        lowerdir = %intent.lowerdir.display(),
        "cang seeded a new /nix overlay generation from the previous one"
    );
    Ok(())
}

/// Drop the retired generations under `stale`, or move them out of the way when
/// their contents cannot be deleted (a generation a guest already used holds at
/// least one guest-owned directory, which is normal, not an error).
fn reclaim_stale_generations(
    paths: &NixOverlayPaths,
    stale: &Path,
    commands: &impl UpperSeedCommands,
) {
    let mut candidates = vec![stale.to_path_buf()];
    let parent = paths.root.parent().unwrap_or(Path::new(".")).to_path_buf();
    if let Ok(entries) = fs::read_dir(&parent) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(SEED_RETIRED_PREFIX) {
                candidates.push(entry.path());
            }
        }
    }
    for candidate in candidates {
        if candidate.symlink_metadata().is_err() {
            continue;
        }
        if commands.remove_tree(&candidate).is_ok() {
            continue;
        }
        if candidate == stale {
            let retired = retired_sibling(&candidate);
            let retired = if retired.parent() == Some(Path::new("")) {
                parent.join(retired.file_name().unwrap_or_default())
            } else {
                retired
            };
            match fs::rename(&candidate, &retired) {
                Ok(()) => tracing::warn!(
                    retired = %retired.display(),
                    "cang could not delete the previous /nix overlay generation; moved it aside                      (its store objects are hardlinked into the live generation, so it costs                      inodes rather than data)"
                ),
                Err(err) => tracing::warn!(
                    path = %candidate.display(),
                    error = %err,
                    "cang could not reclaim the previous /nix overlay generation"
                ),
            }
        }
    }
}

trait UpperSeedCommands {
    fn create_generation_root(&self, path: &Path) -> Result<()>;
    fn create_empty_upper(&self, path: &Path) -> Result<()>;
    fn clone_generation(&self, source: &Path, target: &Path) -> Result<()>;
    fn install_image_db(&self, lowerdir: &Path, dest_db: &Path) -> Result<()>;
    fn reset_work(&self, workdir: &Path) -> Result<()>;
    fn remove_tree(&self, path: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct HostUpperSeedCommands;

impl UpperSeedCommands for HostUpperSeedCommands {
    /// The generation directory: a btrfs subvolume when the filesystem offers
    /// them, else a plain directory. Everything one generation owns - `upper`,
    /// `work`, `merged` - lives inside it, so the overlay's requirement
    /// that upperdir and workdir share a filesystem holds by construction, and a later
    /// generation can be a snapshot of this one.
    fn create_generation_root(&self, path: &Path) -> Result<()> {
        if path.is_dir() {
            return Ok(());
        }
        if btrfs_ok(&["subvolume", "create"], path) {
            return Ok(());
        }
        fs::create_dir_all(path).with_context(|| {
            format!(
                "failed to create cang /nix overlay root '{}'",
                path.display()
            )
        })
    }

    fn create_empty_upper(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).with_context(|| {
            format!(
                "failed to create cang /nix overlay upper '{}'",
                path.display()
            )
        })
    }

    /// Copy one generation into `target` for a new image generation.
    ///
    /// On btrfs this is a `btrfs subvolume snapshot` - constant time, no data
    /// copied, ownership preserved by the filesystem - which is what makes an
    /// image move cost ~25 ms instead of a per-file clone. Elsewhere (or for a
    /// generation that is still a plain directory from an older cang) the tree
    /// is cloned: hardlinks where the kernel allows them, `FICLONE` where the
    /// filesystem supports it, a copy otherwise.
    fn clone_generation(&self, source: &Path, target: &Path) -> Result<()> {
        if is_subvolume(source) && btrfs_snapshot(source, target) {
            return Ok(());
        }
        self.create_generation_root(target)?;
        // Only the upper carries anything worth keeping. `work` and `merged` are
        // overlayfs scratch: they hold the previous mount's state, and after a
        // guest has run they belong to its uid, so copying them is both useless
        // and impossible for an unprivileged host process.
        let from = source.join(UPPER_DIR);
        if from.is_dir() {
            clone_tree(&from, &target.join(UPPER_DIR), &from).with_context(|| {
                format!(
                    "failed to clone the /nix overlay upper '{}' into '{}'",
                    from.display(),
                    target.display()
                )
            })?;
        }
        for dir in [WORK_DIR, MERGED_DIR] {
            fs::create_dir_all(target.join(dir)).with_context(|| {
                format!(
                    "failed to create '{}' in the seeded /nix overlay generation",
                    dir
                )
            })?;
        }
        Ok(())
    }

    fn install_image_db(&self, lowerdir: &Path, dest_db: &Path) -> Result<()> {
        let source_db = lowerdir.join("var/nix/db");
        if !source_db.is_dir() {
            return Ok(());
        }
        fs::create_dir_all(dest_db).with_context(|| {
            format!(
                "failed to create '/nix/var/nix/db' in the seeded upper '{}'",
                dest_db.display()
            )
        })?;
        for name in ["db.sqlite", "schema"] {
            let from = source_db.join(name);
            if from.is_file() {
                let to = dest_db.join(name);
                fs::copy(&from, &to).with_context(|| {
                    format!(
                        "failed to install the image /nix DB '{}' as '{}'",
                        from.display(),
                        to.display()
                    )
                })?;
            }
        }
        Ok(())
    }

    fn reset_work(&self, workdir: &Path) -> Result<()> {
        self.remove_tree(workdir)?;
        fs::create_dir_all(workdir).with_context(|| {
            format!(
                "failed to recreate cang /nix overlay workdir '{}'",
                workdir.display()
            )
        })
    }

    fn remove_tree(&self, path: &Path) -> Result<()> {
        if path.symlink_metadata().is_err() {
            return Ok(());
        }
        if btrfs_ok(&["subvolume", "delete"], path) {
            return Ok(());
        }
        make_dirs_writable(path);
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            // A generation a guest has used holds directories owned by the
            // guest's uid, which an unprivileged host process cannot delete.
            // Renaming needs write access to the *parent* only, so move it out
            // of the way rather than failing the launch; the seeder reclaims
            // retired generations on a later run.
            Err(err) => {
                let retired = retired_sibling(path);
                match fs::rename(path, &retired) {
                    Ok(()) => {
                        tracing::warn!(
                            path = %path.display(),
                            retired = %retired.display(),
                            error = %err,
                            "cang could not delete a /nix overlay generation and moved it aside"
                        );
                        Ok(())
                    }
                    Err(rename_err) => Err(rename_err).with_context(|| {
                        format!(
                            "failed to remove the retired /nix overlay upper '{}'",
                            path.display()
                        )
                    }),
                }
            }
        }
    }
}

/// `btrfs <args> <path>` succeeding means the capability is there (btrfs-progs
/// on PATH and the path on btrfs); every caller falls back when it does not.
/// A btrfs subvolume's root directory is inode 256 (unlike `btrfs subvolume
/// show`, this needs no privileges); a false positive costs one failed snapshot
/// attempt and then the clone path.
fn is_subvolume(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && metadata.ino() == 256)
        .unwrap_or(false)
}

fn btrfs_snapshot(source: &Path, target: &Path) -> bool {
    if target.exists() {
        return false;
    }
    let Some(btrfs) = btrfs_bin() else {
        return false;
    };
    Command::new(btrfs)
        .arg("subvolume")
        .arg("snapshot")
        .arg(source)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// The `btrfs` binary, resolved without trusting `PATH`.
///
/// The launch that seeds a generation runs in the VM worker, whose environment
/// is an allowlist - on a NixOS host `btrfs` is a profile path, not `/usr/bin` -
/// so a bare `Command::new("btrfs")` would silently decide "no btrfs here" and
/// fall back to a per-file clone. The usual install locations are searched too.
fn btrfs_bin() -> Option<&'static Path> {
    static BIN: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join("btrfs"))
                    .collect()
            })
            .unwrap_or_default();
        for dir in [
            "/run/current-system/sw/bin",
            "/usr/bin",
            "/usr/sbin",
            "/sbin",
            "/bin",
            "/usr/local/bin",
        ] {
            candidates.push(Path::new(dir).join("btrfs"));
        }
        candidates.into_iter().find(|candidate| candidate.is_file())
    })
    .as_deref()
}

fn btrfs_ok(args: &[&str], path: &Path) -> bool {
    let Some(btrfs) = btrfs_bin() else {
        return false;
    };
    Command::new(btrfs)
        .args(args)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Clone a tree so every entry is usable in the new generation, preferring the
/// cheapest mechanism per file: a hardlink (one inode, no data), then a reflink
/// (shared extents, works for files owned by another uid — `fs.protected_hardlinks`
/// forbids linking those, and after a guest has run every store object in the
/// upper is one), then a plain copy.
///
/// `upper_root` is the upper this tree belongs to, and it is what tells cache from
/// state when an entry cannot be carried over: `store/` is the cache (a skipped
/// object leaves the DB naming something that is not there, so a failure there is
/// fatal), while `var/nix` is the guest's own state - a lock file its root wrote is
/// mode 0600 under the guest's uid, which this process cannot read at all, and
/// failing a launch over a lock the next boot recreates is the wrong outcome.
fn clone_tree(source: &Path, target: &Path, upper_root: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat '{}'", source.display()))?;
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        fs::create_dir_all(target)
            .with_context(|| format!("failed to create '{}' while cloning", target.display()))?;
        let entries = match fs::read_dir(source) {
            Ok(entries) => entries,
            Err(err) if is_guest_state(upper_root, source) => {
                tracing::warn!(
                    path = %source.display(),
                    %err,
                    "skipping /nix upper state the host cannot read; the guest recreates it"
                );
                // Only the empty directory this call just made.
                let _ = fs::remove_dir(target);
                return Ok(());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read '{}'", source.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            clone_tree(&entry.path(), &target.join(entry.file_name()), upper_root)?;
        }
        // Store objects are 0555; restore the source mode only after the
        // directory is populated, and keep it exact so `nix-store --verify-path`
        // still matches the recorded narHash.
        fs::set_permissions(target, metadata.permissions())?;
        return Ok(());
    }
    if file_type.is_symlink() {
        let link = fs::read_link(source)?;
        std::os::unix::fs::symlink(link, target)
            .with_context(|| format!("failed to recreate symlink '{}'", target.display()))?;
        return Ok(());
    }
    if file_type.is_file() {
        let cloned = fs::hard_link(source, target).is_ok() || reflink(source, target).is_ok();
        if !cloned {
            match fs::copy(source, target) {
                Ok(_) => {}
                Err(err) if is_guest_state(upper_root, source) => {
                    let _ = fs::remove_file(target);
                    tracing::warn!(
                        path = %source.display(),
                        %err,
                        "skipping /nix upper state the host cannot read; the guest recreates it"
                    );
                    return Ok(());
                }
                Err(err) => {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to copy '{}' to '{}' while cloning the /nix overlay upper",
                            source.display(),
                            target.display()
                        )
                    });
                }
            }
        }
        fs::set_permissions(target, metadata.permissions())?;
    }
    // Sockets and fifos are never store content; a stale daemon socket is
    // dropped with the rest of `var/nix` state by the seed.
    Ok(())
}

/// Whether `path` is the guest's own `/nix` state rather than the store cache.
///
/// Everything under `var/nix` is state a running guest owns: lock files it creates
/// mode 0600 under its own uid, socket directories, the `temproots` trees. The
/// store's objects live under `store/` and nowhere else.
fn is_guest_state(upper_root: &Path, path: &Path) -> bool {
    path.strip_prefix(upper_root)
        .map(|relative| relative.starts_with("var/nix"))
        .unwrap_or(false)
}

/// `ioctl(dst, FICLONE, src)`: a copy-on-write clone sharing the source's
/// extents (btrfs, xfs, zfs, and ext4 with reflink). Costs metadata only, and
/// unlike a hardlink it is allowed for a file owned by another uid.
fn reflink(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    // FICLONE from <linux/fs.h>; libc does not name it.
    const FICLONE: libc::c_ulong = 0x4004_9409;

    let source_file = fs::File::open(source)?;
    let target_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)?;
    // SAFETY: both descriptors are open for the duration of the call, and
    // FICLONE takes the source fd as its argument.
    let rc = unsafe { libc::ioctl(target_file.as_raw_fd(), FICLONE, source_file.as_raw_fd()) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    let _ = fs::remove_file(target);
    Err(err)
}

/// A sibling name that reclaims a generation by moving it aside.
fn retired_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("generation"));
    name.push(format!(
        ".retired.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default()
    ));
    path.with_file_name(name)
}

fn make_dirs_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700));
    }
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            make_dirs_writable(&entry.path());
        }
    }
}

fn record_state(path: &Path, intent: &HostNixOverlay) -> Result<()> {
    if let Ok(text) = fs::read_to_string(path) {
        let state = parse_state(&text);
        let prior_digest = state.get("image_digest").map(String::as_str);
        let prior_lowerdir = state.get("lowerdir").map(String::as_str);
        let lowerdir = intent.lowerdir.display().to_string();
        if prior_digest != Some(intent.image_digest.as_str()) || prior_lowerdir != Some(&lowerdir) {
            tracing::debug!(
                state = %path.display(),
                prior_digest = ?prior_digest,
                prior_lowerdir = ?prior_lowerdir,
                image_digest = %intent.image_digest,
                lowerdir = %intent.lowerdir.display(),
                "cang host /nix overlay lowerdir changed; the upper is seeded from that generation"
            );
        }
    }
    write_state(path, intent)
}

fn write_state(path: &Path, intent: &HostNixOverlay) -> Result<()> {
    let state = format!(
        "selected_reference={}\nimage_digest={}\ndigest_key={}\nlowerdir={}\nupperdir={}\nworkdir={}\nmergeddir={}\n{}={}\n",
        intent.selected_reference,
        intent.image_digest,
        intent.digest_key,
        intent.lowerdir.display(),
        intent.upperdir.display(),
        intent.workdir.display(),
        intent.mergeddir.display(),
        GENERATION_KEY,
        generation_of(intent)
    );
    fs::write(path, state).with_context(|| {
        format!(
            "failed to write cang host /nix overlay state '{}'",
            path.display()
        )
    })
}

fn parse_state(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn lock_exclusive(file: &File) -> Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("flock failed")
    }
}

fn validate_absolute_intent(intent: &HostNixOverlay) -> Result<()> {
    for (name, path) in [
        ("lowerdir", &intent.lowerdir),
        ("upperdir", &intent.upperdir),
        ("workdir", &intent.workdir),
        ("mergeddir", &intent.mergeddir),
    ] {
        if !path.is_absolute() {
            bail!(
                "cang host /nix overlay {name} '{}' must be absolute",
                path.display()
            );
        }
    }
    Ok(())
}

fn overlay_options(intent: &HostNixOverlay) -> String {
    format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        intent.lowerdir.display(),
        intent.upperdir.display(),
        intent.workdir.display(),
    )
}

fn umount_path(target: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(target.as_os_str().as_bytes()).with_context(|| {
        format!(
            "host /nix overlay unmount target '{}' contains an interior NUL byte",
            target.display()
        )
    })?;
    // SAFETY: `raw` is a NUL-terminated copy of the target path and remains
    // valid for the duration of the syscall.
    let rc = unsafe { libc::umount(raw.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "cang host /nix overlay unmount '{}' failed",
                target.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::session::rootfs::image_source::{
        ImageSourceCacheProfile, ImageSourceCacheStatus, OciProcessConfig,
    };
    use crate::runtime::session::rootfs::task::{TaskRootfsHandle, TaskRootfsHandleTestSpec};
    use crate::task_rootfs::TaskRootfsBackend;
    use std::cell::RefCell;

    #[test]
    fn overlay_paths_are_workspace_scoped() {
        let a = NixOverlayPaths::new(Path::new("/state/cang/workspace-a"));
        let b = NixOverlayPaths::new(Path::new("/state/cang/workspace-b"));

        assert_eq!(
            a.upperdir,
            Path::new("/state/cang/workspace-a/nix-overlay/upper")
        );
        assert_eq!(
            a.workdir,
            Path::new("/state/cang/workspace-a/nix-overlay/work")
        );
        assert_eq!(
            a.mergeddir,
            Path::new("/state/cang/workspace-a/nix-overlay/merged")
        );
        // Outside the generation directory: a new generation replaces that
        // directory (by btrfs snapshot), and a lock file that moved with it
        // would let two launches hold two different locks.
        assert_eq!(
            a.lease_lock,
            Path::new("/state/cang/workspace-a/nix-overlay.lease.lock")
        );
        assert_eq!(
            a.lease_state,
            Path::new("/state/cang/workspace-a/nix-overlay.lease.state")
        );
        assert_ne!(a.upperdir, b.upperdir);
        assert_ne!(a.mergeddir, b.mergeddir);
    }

    #[test]
    fn intent_uses_digest_keyed_image_cache_nix_lowerdir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_entry = temp
            .path()
            .join("image-cache/btrfs-snapshots/sha256-deadbeef");
        fs::create_dir_all(cache_entry.join("rootfs/nix")).expect("cache lowerdir");
        let paths = NixOverlayPaths::new(temp.path().join("state/workspace").as_path());
        let handle = task_handle(
            cache_entry.clone(),
            Some("sha256:deadbeef"),
            Some("sha256-deadbeef"),
        );

        let intent = intent_from_task_rootfs(&paths, &handle).expect("intent");

        assert_eq!(intent.lowerdir, cache_entry.join("rootfs/nix"));
        assert_eq!(intent.image_digest, "sha256:deadbeef");
        assert_eq!(intent.digest_key, "sha256-deadbeef");
        assert_eq!(intent.upperdir, paths.upperdir);
        assert_eq!(intent.mergeddir, paths.mergeddir);
    }

    #[test]
    fn missing_cache_nix_lowerdir_fails_clearly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_entry = temp
            .path()
            .join("image-cache/btrfs-snapshots/sha256-deadbeef");
        fs::create_dir_all(cache_entry.join("rootfs")).expect("cache rootfs");
        let paths = NixOverlayPaths::new(temp.path());
        let handle = task_handle(
            cache_entry,
            Some("sha256:deadbeef"),
            Some("sha256-deadbeef"),
        );

        let err = intent_from_task_rootfs(&paths, &handle).expect_err("missing /nix fails");

        assert!(format!("{err:#}").contains("does not contain /nix"));
    }

    #[test]
    fn existing_state_for_different_digest_seeds_a_new_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        fs::create_dir_all(&paths.root).expect("overlay root");
        fs::write(
            &paths.lease_state,
            "selected_reference=localhost/cang:latest\nimage_digest=sha256:old\ndigest_key=sha256-old\nlowerdir=/old/nix\nupperdir=/state/nix-overlay/upper\nworkdir=/state/nix-overlay/work\nmergeddir=/state/nix-overlay/merged\n",
        )
        .expect("old state");
        let cache_entry = temp.path().join("image-cache/btrfs-snapshots/sha256-new");
        fs::create_dir_all(cache_entry.join("rootfs/nix")).expect("cache lowerdir");
        let handle = task_handle(cache_entry.clone(), Some("sha256:new"), Some("sha256-new"));

        let lease = NixOverlayLease::acquire(&workspace_state, &handle).expect("lease");

        assert_eq!(lease.intent().lowerdir, cache_entry.join("rootfs/nix"));
        assert_eq!(lease.intent().upperdir, paths.upperdir);
        assert_eq!(lease.intent().workdir, paths.workdir);
        assert_eq!(lease.intent().mergeddir, paths.mergeddir);
        let state = fs::read_to_string(&paths.lease_state).expect("rewritten state");
        assert!(state.contains("image_digest=sha256:new\n"));
        assert!(state.contains(&format!(
            "lowerdir={}\n",
            cache_entry.join("rootfs/nix").display()
        )));
        assert!(!state.contains("image_digest=sha256:old\n"));
        assert!(!state.contains("lowerdir=/old/nix\n"));
    }

    #[test]
    fn worker_mount_creates_dirs_then_mounts_overlay() {
        let runner = FakeOverlayCommands::default();
        let intent = absolute_intent();

        prepare_and_mount(&intent, &runner).expect("mount");

        assert_eq!(
            runner.calls.borrow().as_slice(),
            [
                "mkdir:/state/nix-overlay/upper",
                "mkdir:/state/nix-overlay/work",
                "mkdir:/state/nix-overlay/merged",
                "mount:lowerdir=/cache/rootfs/nix,upperdir=/state/nix-overlay/upper,workdir=/state/nix-overlay/work,userxattr:/state/nix-overlay/merged",
            ]
        );
    }

    #[test]
    fn worker_unmount_records_namespace_visible_merged_target() {
        let runner = FakeOverlayCommands::default();
        let intent = absolute_intent();
        prepare_and_mount(&intent, &runner).expect("mount");
        unmount_with(&intent, &runner).expect("unmount");

        assert!(
            runner
                .calls
                .borrow()
                .contains(&"umount:/state/nix-overlay/merged".to_owned())
        );
    }

    fn task_handle(
        cache_path: PathBuf,
        image_digest: Option<&str>,
        digest_key: Option<&str>,
    ) -> TaskRootfsHandle {
        TaskRootfsHandle::new_for_test(TaskRootfsHandleTestSpec {
            task_id: "task".to_owned(),
            task_dir: PathBuf::from("/state/tasks/task"),
            rootfs_path: PathBuf::from("/state/tasks/task/rootfs-btrfs-snapshot"),
            backend: TaskRootfsBackend::BtrfsSnapshot,
            selected_image_reference: "localhost/cang:latest".to_owned(),
            image_digest: image_digest.map(str::to_owned),
            process_config: OciProcessConfig::default(),
            cache_profile: ImageSourceCacheProfile {
                status: ImageSourceCacheStatus::Hit,
                digest_key: digest_key.map(str::to_owned),
                cache_path: Some(cache_path),
                uncached_reason: None,
            },
            preserve_debug: false,
        })
    }

    fn absolute_intent() -> HostNixOverlay {
        HostNixOverlay {
            selected_reference: "localhost/cang:latest".to_owned(),
            image_digest: "sha256:deadbeef".to_owned(),
            digest_key: "sha256-deadbeef".to_owned(),
            lowerdir: PathBuf::from("/cache/rootfs/nix"),
            upperdir: PathBuf::from("/state/nix-overlay/upper"),
            workdir: PathBuf::from("/state/nix-overlay/work"),
            mergeddir: PathBuf::from("/state/nix-overlay/merged"),
        }
    }

    #[derive(Default)]
    struct FakeOverlayCommands {
        calls: RefCell<Vec<String>>,
    }

    impl NixOverlayCommands for FakeOverlayCommands {
        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("mkdir:{}", path.display()));
            Ok(())
        }

        fn mount_overlay(&self, intent: &HostNixOverlay) -> Result<()> {
            self.calls.borrow_mut().push(format!(
                "mount:{}:{}",
                overlay_options(intent),
                intent.mergeddir.display()
            ));
            Ok(())
        }

        fn unmount_overlay(&self, intent: &HostNixOverlay) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("umount:{}", intent.mergeddir.display()));
            Ok(())
        }
    }
    fn write_upper_with_store_object(paths: &NixOverlayPaths, name: &str) -> PathBuf {
        let store_path = paths.upperdir.join("store").join(name);
        fs::create_dir_all(&store_path).expect("store path");
        fs::write(store_path.join("payload"), name).expect("payload");
        let var_nix = paths.upperdir.join("var/nix");
        fs::create_dir_all(var_nix.join("db")).expect("db dir");
        fs::create_dir_all(var_nix.join("profiles")).expect("profiles dir");
        fs::create_dir_all(var_nix.join("daemon-socket")).expect("socket dir");
        fs::create_dir_all(var_nix.join("temproots")).expect("temproots dir");
        fs::write(var_nix.join("db/db.sqlite"), "previous generation").expect("previous db");
        // Store objects are read-only; the clone has to keep that, because
        // `nix-store --verify-path` compares the recorded narHash.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&store_path, fs::Permissions::from_mode(0o555))
            .expect("read-only store path");
        store_path
    }

    /// A guest's own `/nix` state is often unreadable to the host: its root writes
    /// lock files 0600 under the guest's uid, and a directory it created 0700
    /// cannot be listed here at all. Seeding a new generation has to skip that
    /// state - the next boot recreates it - while a store object, which the DB
    /// keeps naming, stays a hard failure.
    #[test]
    fn cloning_skips_unreadable_guest_state_but_not_the_store() {
        use std::os::unix::fs::PermissionsExt;

        // Root reads everything, so the case needs an unprivileged uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let upper = temp.path().join("upper");
        let store_object = upper.join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-obj");
        let guest_state = upper.join("var/nix/temproots");
        fs::create_dir_all(&guest_state).expect("state dir");
        fs::create_dir_all(&store_object).expect("store object");
        fs::write(store_object.join("file"), "x").expect("store file");
        fs::set_permissions(&guest_state, fs::Permissions::from_mode(0o000)).expect("chmod state");

        let target = temp.path().join("clone");
        clone_tree(&upper, &target, &upper).expect("unreadable guest state is skipped");
        assert!(
            target
                .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-obj/file")
                .is_file()
        );
        assert!(!target.join("var/nix/temproots").exists());

        fs::set_permissions(&store_object, fs::Permissions::from_mode(0o000)).expect("chmod store");
        let err = clone_tree(&upper, &temp.path().join("clone2"), &upper)
            .expect_err("an unreadable store object must not be skipped");
        assert!(
            format!("{err:#}").contains("failed to read"),
            "unexpected error: {err:#}"
        );

        fs::set_permissions(&store_object, fs::Permissions::from_mode(0o755)).expect("restore");
        fs::set_permissions(&guest_state, fs::Permissions::from_mode(0o755)).expect("restore");
    }

    fn lease_state_text(image_digest: &str, lowerdir: &Path, paths: &NixOverlayPaths) -> String {
        format!(
            "selected_reference=localhost/cang:latest\nimage_digest={image_digest}\ndigest_key=k\nlowerdir={}\nupperdir={}\nworkdir={}\nmergeddir={}\n",
            lowerdir.display(),
            paths.upperdir.display(),
            paths.workdir.display(),
            paths.mergeddir.display()
        )
    }

    #[test]
    fn first_use_creates_an_empty_upper_and_work_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        let cache_entry = temp.path().join("image-cache/sha256-new");
        fs::create_dir_all(cache_entry.join("rootfs/nix/var/nix/db")).expect("cache");
        let handle = task_handle(cache_entry.clone(), Some("sha256:new"), Some("sha256-new"));

        let lease = NixOverlayLease::acquire(&workspace_state, &handle).expect("lease");

        assert_eq!(lease.seed(), UpperSeedOutcome::Fresh);
        assert!(paths.upperdir.is_dir());
        assert!(
            fs::read_to_string(&paths.lease_state)
                .expect("state")
                .contains("generation=")
        );
    }

    #[test]
    fn same_generation_reuses_the_upper_without_touching_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        let cache_entry = temp.path().join("image-cache/sha256-new");
        fs::create_dir_all(cache_entry.join("rootfs/nix/var/nix/db")).expect("cache");
        let handle = task_handle(cache_entry.clone(), Some("sha256:new"), Some("sha256-new"));
        NixOverlayLease::acquire(&workspace_state, &handle).expect("first lease");
        let store_path =
            write_upper_with_store_object(&paths, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept");

        let lease = NixOverlayLease::acquire(&workspace_state, &handle).expect("second lease");

        assert_eq!(lease.seed(), UpperSeedOutcome::Reused);
        assert!(store_path.is_dir());
        assert_eq!(
            fs::read_to_string(paths.upperdir.join("var/nix/db/db.sqlite")).expect("db"),
            "previous generation"
        );
        assert!(!paths.root.with_file_name(SEED_TMP_DIR).exists());
        assert!(!paths.root.with_file_name(SEED_STALE_DIR).exists());
    }

    #[test]
    fn new_generation_clones_the_previous_upper_and_swaps_in_the_image_db() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        fs::create_dir_all(&paths.root).expect("overlay root");
        let old_lowerdir = temp.path().join("image-cache/sha256-old/rootfs/nix");
        fs::create_dir_all(old_lowerdir.join("var/nix/db")).expect("old cache");
        fs::write(
            &paths.lease_state,
            lease_state_text("sha256:old", &old_lowerdir, &paths),
        )
        .expect("old state");
        let store_path =
            write_upper_with_store_object(&paths, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept");
        let store_mode = fs::metadata(&store_path).expect("metadata").permissions();
        fs::create_dir_all(&paths.workdir).expect("workdir");
        fs::write(paths.workdir.join("leftover"), "overlayfs state").expect("stale work");

        let cache_entry = temp.path().join("image-cache/sha256-new");
        fs::create_dir_all(cache_entry.join("rootfs/nix/var/nix/db")).expect("cache");
        fs::write(
            cache_entry.join("rootfs/nix/var/nix/db/db.sqlite"),
            "image generation",
        )
        .expect("image db");
        let handle = task_handle(cache_entry, Some("sha256:new"), Some("sha256-new"));

        let lease = NixOverlayLease::acquire(&workspace_state, &handle).expect("lease");

        assert_eq!(lease.seed(), UpperSeedOutcome::Seeded);
        // The cache survives: the store object is still there, bit for bit.
        let cloned = paths
            .upperdir
            .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept/payload");
        assert_eq!(
            fs::read_to_string(&cloned).expect("cloned payload"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(
                paths
                    .upperdir
                    .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept")
            )
            .expect("cloned metadata")
            .permissions()
            .mode()
                & 0o777,
            store_mode.mode() & 0o777,
            "the clone must keep store-object modes so narHash verification still matches"
        );
        // The live DB is the new image's; the previous one is parked for the guest.
        assert_eq!(
            fs::read_to_string(paths.upperdir.join(IMAGE_DB_DIR).join("db.sqlite"))
                .expect("the image DB is staged for the guest to install"),
            "image generation"
        );
        assert_eq!(
            fs::read_to_string(paths.upperdir.join(PARKED_DB_DIR).join("db.sqlite"))
                .expect("parked db"),
            "previous generation"
        );
        assert!(paths.upperdir.join("var/nix/profiles").is_dir());
        // The dead daemon's socket and temproots belong to the guest's uid now:
        // the guest wipes them when it installs the staged image DB.
        assert!(paths.upperdir.join("var/nix/daemon-socket").exists());
        assert!(!paths.root.with_file_name(SEED_TMP_DIR).exists());
        assert!(!paths.root.with_file_name(SEED_STALE_DIR).exists());
        assert!(paths.workdir.is_dir());
        assert!(!paths.workdir.join("leftover").exists());
        assert!(
            fs::read_to_string(&paths.lease_state)
                .expect("state")
                .contains("image_digest=sha256:new")
        );
    }

    #[test]
    fn interrupted_seed_leftovers_are_dropped_on_the_next_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        fs::create_dir_all(&paths.root).expect("overlay root");
        let old_lowerdir = temp.path().join("image-cache/sha256-old/rootfs/nix");
        fs::create_dir_all(old_lowerdir.join("var/nix/db")).expect("old cache");
        fs::write(
            &paths.lease_state,
            lease_state_text("sha256:old", &old_lowerdir, &paths),
        )
        .expect("old state");
        write_upper_with_store_object(&paths, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept");
        fs::create_dir_all(paths.root.join(SEED_TMP_DIR).join("store")).expect("stale tmp");
        fs::create_dir_all(paths.root.join(SEED_STALE_DIR)).expect("stale retired upper");

        let cache_entry = temp.path().join("image-cache/sha256-new");
        fs::create_dir_all(cache_entry.join("rootfs/nix/var/nix/db")).expect("cache");
        fs::write(
            cache_entry.join("rootfs/nix/var/nix/db/db.sqlite"),
            "image generation",
        )
        .expect("image db");
        let handle = task_handle(cache_entry, Some("sha256:new"), Some("sha256-new"));

        let lease = NixOverlayLease::acquire(&workspace_state, &handle).expect("lease");

        assert_eq!(lease.seed(), UpperSeedOutcome::Seeded);
        assert!(!paths.root.with_file_name(SEED_TMP_DIR).exists());
        assert!(!paths.root.with_file_name(SEED_STALE_DIR).exists());
        assert!(paths.upperdir.join("store").is_dir());
    }
    /// Opt-in: needs a btrfs directory, which a plain `cargo test` tempdir is
    /// not. Run with
    /// `CANG_BTRFS_TEST_ROOT=/path/on/btrfs cargo test -p cang reflinks_a_seeded_generation`.
    ///
    /// The seeded generation must share the previous generation's extents rather
    /// than copy its bytes, and it must stay a plain directory: overlayfs needs
    /// `upperdir` and `workdir` on the same filesystem, so a subvolume upper
    /// breaks every copy-up with `EXDEV`.
    #[test]
    fn reflinks_a_seeded_generation_on_btrfs() {
        use std::os::unix::fs::MetadataExt;

        let Ok(root) = std::env::var("CANG_BTRFS_TEST_ROOT") else {
            return;
        };
        let temp = tempfile::Builder::new()
            .prefix("cang-reflink-")
            .tempdir_in(&root)
            .expect("tempdir on the btrfs root");
        let old_cache = temp.path().join("cache/sha256-old");
        fs::create_dir_all(old_cache.join("rootfs/nix/var/nix/db")).expect("cache");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        let old_handle = task_handle(old_cache, Some("sha256:old"), Some("sha256-old"));
        {
            let lease = NixOverlayLease::acquire(&workspace_state, &old_handle).expect("first");
            assert_eq!(lease.seed(), UpperSeedOutcome::Fresh);
        }
        // A generation's worth of store object, big enough that a copy is visible.
        let payload = paths
            .upperdir
            .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept/payload");
        fs::create_dir_all(payload.parent().expect("object dir")).expect("object dir");
        fs::write(&payload, vec![7_u8; 4 * 1024 * 1024]).expect("payload");
        let blocks_before = fs::metadata(&payload).expect("metadata").blocks();

        let new_cache = temp.path().join("cache/sha256-new");
        fs::create_dir_all(new_cache.join("rootfs/nix/var/nix/db")).expect("cache");
        fs::write(
            new_cache.join("rootfs/nix/var/nix/db/db.sqlite"),
            "image generation",
        )
        .expect("image db");
        let new_handle = task_handle(new_cache, Some("sha256:new"), Some("sha256-new"));
        let lease = NixOverlayLease::acquire(&workspace_state, &new_handle).expect("second");

        assert_eq!(lease.seed(), UpperSeedOutcome::Seeded);
        assert_eq!(
            fs::metadata(&paths.upperdir).expect("upper").dev(),
            fs::metadata(&paths.root).expect("root").dev(),
            "upper owns no separate filesystem: overlayfs needs upperdir and workdir together"
        );
        let seeded = paths
            .upperdir
            .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept/payload");
        assert_eq!(
            fs::read(&seeded).expect("seeded payload").len(),
            4 * 1024 * 1024
        );
        assert_eq!(
            fs::metadata(&seeded).expect("seeded").blocks(),
            blocks_before,
            "the clone must not duplicate the payload's data"
        );
        assert_eq!(
            fs::read_to_string(paths.upperdir.join(IMAGE_DB_DIR).join("db.sqlite"))
                .expect("the image DB is staged for the guest to install"),
            "image generation"
        );
    }
    /// The clone strategy relies on `FICLONE` for files it may not hardlink
    /// (`fs.protected_hardlinks` refuses a file owned by another uid, and after a
    /// guest has run every store object in the upper is one). Opt-in on btrfs:
    /// `CANG_BTRFS_TEST_ROOT=/path/on/btrfs cargo test -p cang reflink_shares`.
    #[test]
    fn reflink_shares_extents_on_btrfs() {
        use std::os::unix::fs::MetadataExt;

        let Ok(root) = std::env::var("CANG_BTRFS_TEST_ROOT") else {
            return;
        };
        let temp = tempfile::Builder::new()
            .prefix("cang-ficlone-")
            .tempdir_in(&root)
            .expect("tempdir on the btrfs root");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        fs::write(&source, vec![3_u8; 4 * 1024 * 1024]).expect("source");

        reflink(&source, &target).expect("FICLONE on btrfs");

        assert_eq!(
            fs::read(&target).expect("target").len(),
            4 * 1024 * 1024,
            "the reflinked file must carry the source's data"
        );
        assert_ne!(
            fs::metadata(&source).expect("source").ino(),
            fs::metadata(&target).expect("target").ino(),
            "a reflink is a new inode, unlike a hardlink"
        );
        let shared = Command::new("btrfs")
            .args(["filesystem", "du", "-s"])
            .arg(temp.path())
            .output()
            .expect("btrfs filesystem du");
        let report = String::from_utf8_lossy(&shared.stdout).to_string();
        // "Exclusive 0.00B" is the proof: the clone owns no extent of its own,
        // so the payload's 4 MiB were never written a second time.
        let exclusive = report
            .lines()
            .filter(|line| !line.contains("Filename"))
            .filter_map(|line| line.split_whitespace().nth(1))
            .next()
            .unwrap_or_default();
        assert_eq!(
            exclusive, "0.00B",
            "the clone should share extents instead of allocating: {report}"
        );
    }
    /// Opt-in: needs btrfs. A new image generation must be a `btrfs subvolume
    /// snapshot` of the previous one - constant time, no data copied - with the
    /// previous DB parked for the guest to merge and the image's own installed.
    /// Run with
    /// `CANG_BTRFS_TEST_ROOT=/path/on/btrfs cargo test -p cang snapshots_a_new_generation`.
    #[test]
    fn snapshots_a_new_generation_on_btrfs() {
        use std::os::unix::fs::MetadataExt;

        let Ok(root) = std::env::var("CANG_BTRFS_TEST_ROOT") else {
            return;
        };
        let temp = tempfile::Builder::new()
            .prefix("cang-snapshot-")
            .tempdir_in(&root)
            .expect("tempdir on the btrfs root");
        let workspace_state = temp.path().join("state/workspace");
        let paths = NixOverlayPaths::new(&workspace_state);
        let old_cache = temp.path().join("cache/sha256-old");
        fs::create_dir_all(old_cache.join("rootfs/nix/var/nix/db")).expect("cache");
        let old_handle = task_handle(old_cache, Some("sha256:old"), Some("sha256-old"));
        {
            let lease = NixOverlayLease::acquire(&workspace_state, &old_handle).expect("first");
            assert_eq!(lease.seed(), UpperSeedOutcome::Fresh);
        }
        assert!(
            is_subvolume(&paths.root),
            "a generation should be one subvolume holding upper, work and merged"
        );
        assert_eq!(
            fs::metadata(&paths.upperdir).expect("upper").dev(),
            fs::metadata(&paths.root).expect("root").dev(),
            "upper owns no separate filesystem: overlayfs needs upperdir and workdir together"
        );
        write_upper_with_store_object(&paths, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept");
        fs::create_dir_all(paths.upperdir.join("var/nix/db")).expect("db dir");
        fs::write(
            paths.upperdir.join("var/nix/db/db.sqlite"),
            "previous generation",
        )
        .expect("previous db");

        let new_cache = temp.path().join("cache/sha256-new");
        fs::create_dir_all(new_cache.join("rootfs/nix/var/nix/db")).expect("cache");
        fs::write(
            new_cache.join("rootfs/nix/var/nix/db/db.sqlite"),
            "image generation",
        )
        .expect("image db");
        let new_handle = task_handle(new_cache, Some("sha256:new"), Some("sha256-new"));
        let lease = NixOverlayLease::acquire(&workspace_state, &new_handle).expect("second");

        assert_eq!(lease.seed(), UpperSeedOutcome::Seeded);
        assert!(
            is_subvolume(&paths.root),
            "the seeded generation stays a subvolume"
        );
        assert_eq!(
            fs::read_to_string(
                paths
                    .upperdir
                    .join("store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept/payload")
            )
            .expect("the snapshot keeps every store object"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-kept"
        );
        assert_eq!(
            fs::read_to_string(paths.upperdir.join(IMAGE_DB_DIR).join("db.sqlite"))
                .expect("the image DB is staged for the guest to install"),
            "image generation"
        );
        assert_eq!(
            fs::read_to_string(paths.upperdir.join(PARKED_DB_DIR).join("db.sqlite"))
                .expect("parked db"),
            "previous generation"
        );
        assert!(
            paths.lease_lock.is_file()
                && paths.lease_lock.parent() == Some(workspace_state.as_path()),
            "the lease must live outside the generation directory"
        );
        assert!(!paths.root.with_file_name(SEED_STALE_DIR).exists());
    }
}
