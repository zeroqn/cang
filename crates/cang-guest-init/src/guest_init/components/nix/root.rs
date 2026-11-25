use anyhow::{Context, Result, anyhow, bail};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::guest_init::command;
use crate::guest_init::components::env::{
    CangEnv, NIX_DAEMON_SOCKET_DIR, NIX_DAEMON_SOCKET_PATH, NIX_LOG_PATH, NIX_STATUS_PATH,
    NixLowerContract, RUN_DIR, nix_remote_uri,
};
use crate::guest_init::components::nix::block_upper;
use crate::guest_init::components::nix::status::{self, NixPrepState, NixPrepStatus};
use crate::guest_init::profile::ProfileRecorder;
use crate::guest_init::{fs, process};

const WAIT_FOR_STATUS_ENV: &str = "CANG_NIX_PREP_WAIT_FOR_STATUS";

const PROFILE_REQUIRE_TOOLS: &str = "nix-prep:require-tools";
const PROFILE_FIND_DISK: &str = "nix-prep:find-disk";
const PROFILE_PREPARE_RUN_DIRS: &str = "nix-prep:prepare-run-dirs";
const PROFILE_MOUNT_DISK: &str = "nix-prep:mount-disk";
const PROFILE_PRESEED_UPPER: &str = "nix-prep:preseed-upper";
const PROFILE_MOUNT_OVERLAY: &str = "nix-prep:mount-overlay";
const PROFILE_REPAIR_STORE: &str = "nix-prep:repair-store";
/// Where the guest composes its own `/nix` overlay over a read-only block lower.
const PROFILE_FIND_LOWER_DISK: &str = "nix-prep:find-lower-disk";
const PROFILE_MOUNT_LOWER_DISK: &str = "nix-prep:mount-lower-disk";
const PROFILE_PREPARE_BLOCK_DIRS: &str = "nix-prep:prepare-block-dirs";
const PROFILE_SEED_BLOCK_UPPER: &str = "nix-prep:seed-block-upper";
const PROFILE_MOUNT_BLOCK_OVERLAY: &str = "nix-prep:mount-block-overlay";
/// The cang host parks the previous /nix overlay generation's Nix DB here when
/// it seeds the upper for a new image generation (`PARKED_DB_DIR` on the host).
const PARKED_DB_DIR: &str = "/nix/.cang-previous-db";
const PARKED_PREVIOUS_DB: &str = "/nix/.cang-previous-db/db.sqlite";
/// The cang host stages the new image generation's Nix DB here when it seeds the
/// upper (a snapshot preserves the previous generation's ownership, so the host
/// cannot write inside `/nix/var/nix`; this guest, running as root, installs it).
const STAGED_IMAGE_DB_DIR: &str = "/nix/.cang-image-db";
const STAGED_IMAGE_DB: &str = "/nix/.cang-image-db/db.sqlite";
const LIVE_DB_DIR: &str = "/nix/var/nix/db";
const PROFILE_CREATE_SOCKET_DIR: &str = "nix-prep:create-socket-dir";
const PROFILE_START_DAEMON: &str = "nix-prep:start-daemon";
const PROFILE_CHECK_DAEMON: &str = "nix-prep:check-daemon";

/// The name `nix-daemon` itself reads for its socket path, so the daemon binds
/// the tmpfs path this contract names instead of `/nix/var/nix/daemon-socket`.
const DAEMON_SOCKET_ENV: &str = "NIX_DAEMON_SOCKET_PATH";
/// `nix-daemon`'s own default, and what `--store daemon` means. Prep links it to
/// the tmpfs socket so both spellings reach the same daemon.
const DEFAULT_DAEMON_SOCKET_PATH: &str = "/nix/var/nix/daemon-socket/socket";
/// Upper bound for one handshake attempt. A client that connects to a daemon
/// which then never answers does not return on its own, and prep must not hang.
const DAEMON_CHECK_BOUND: &str = "5";
/// How long prep waits for the daemon to answer over its own protocol: ten
/// attempts half a second apart, which is far longer than a healthy daemon needs
/// and still bounds a broken one.
const DAEMON_CHECK_ATTEMPTS: usize = 10;
const DAEMON_CHECK_INTERVAL: Duration = Duration::from_millis(500);

const PRESEED_COMPLETION_SENTINEL: &str = ".cang-nix-preseeded";
const PRESEED_ATTEMPT_MARKER: &str = ".cang-nix-preseed-attempted";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum NixOperation {
    WriteRunningStatus,
    FindDisk,
    MountDisk,
    PreseedUpper,
    MountOverlay,
    RepairStore,
    FindLowerDisk,
    MountLowerDisk,
    PrepareBlockDirs,
    SeedBlockUpper,
    MountBlockOverlay,
    CreateSocketDir,
    StartDaemon,
    CheckDaemon,
    WriteReadyStatus,
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_operations() -> Vec<NixOperation> {
    vec![
        NixOperation::WriteRunningStatus,
        NixOperation::FindDisk,
        NixOperation::MountDisk,
        NixOperation::PreseedUpper,
        NixOperation::MountOverlay,
        NixOperation::RepairStore,
        NixOperation::CreateSocketDir,
        NixOperation::StartDaemon,
        NixOperation::CheckDaemon,
        NixOperation::WriteReadyStatus,
    ]
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_host_overlay_operations() -> Vec<NixOperation> {
    vec![
        NixOperation::WriteRunningStatus,
        NixOperation::RepairStore,
        NixOperation::CreateSocketDir,
        NixOperation::StartDaemon,
        NixOperation::CheckDaemon,
        NixOperation::WriteReadyStatus,
    ]
}

/// The guest-composed `/nix`: the guest mounts the read-only lower disk, seeds
/// an upper it owns when the upper is the build disk's, composes the overlay, and
/// then does everything a host-composed `/nix` does.
#[cfg(test)]
pub(in crate::guest_init) fn planned_block_lower_operations() -> Vec<NixOperation> {
    vec![
        NixOperation::WriteRunningStatus,
        NixOperation::FindLowerDisk,
        NixOperation::MountLowerDisk,
        NixOperation::PrepareBlockDirs,
        NixOperation::SeedBlockUpper,
        NixOperation::MountBlockOverlay,
        NixOperation::RepairStore,
        NixOperation::CreateSocketDir,
        NixOperation::StartDaemon,
        NixOperation::CheckDaemon,
        NixOperation::WriteReadyStatus,
    ]
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_block_lower_profile_labels() -> Vec<&'static str> {
    vec![
        PROFILE_REQUIRE_TOOLS,
        PROFILE_PREPARE_RUN_DIRS,
        PROFILE_FIND_LOWER_DISK,
        PROFILE_MOUNT_LOWER_DISK,
        PROFILE_PREPARE_BLOCK_DIRS,
        PROFILE_SEED_BLOCK_UPPER,
        PROFILE_MOUNT_BLOCK_OVERLAY,
        PROFILE_REPAIR_STORE,
        PROFILE_CREATE_SOCKET_DIR,
        PROFILE_START_DAEMON,
        PROFILE_CHECK_DAEMON,
    ]
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_profile_labels() -> Vec<&'static str> {
    vec![
        PROFILE_REQUIRE_TOOLS,
        PROFILE_FIND_DISK,
        PROFILE_PREPARE_RUN_DIRS,
        PROFILE_MOUNT_DISK,
        PROFILE_PRESEED_UPPER,
        PROFILE_MOUNT_OVERLAY,
        PROFILE_REPAIR_STORE,
        PROFILE_CREATE_SOCKET_DIR,
        PROFILE_START_DAEMON,
    ]
}

pub(in crate::guest_init) fn start_background_prep(env_contract: &CangEnv) -> Result<()> {
    if !env_contract.nix_overlay {
        return Ok(());
    }
    if !process::is_root() {
        bail!("internal /nix overlay prep must start as root");
    }

    let status_path = PathBuf::from(NIX_STATUS_PATH);
    let current = status::read_status(&status_path)?;
    if matches!(current.state, NixPrepState::Ready | NixPrepState::Failed) {
        return Ok(());
    }

    fs::create_dir_all(Path::new(RUN_DIR))?;
    let log_path = PathBuf::from(NIX_LOG_PATH);
    let current_exe = std::env::current_exe().context("failed to resolve guest-init executable")?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let log_err = log.try_clone()?;
    let child = unsafe {
        Command::new(current_exe)
            .args(["internal", "nix", "prep"])
            .env(WAIT_FOR_STATUS_ENV, "1")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()
    }
    .context("failed to spawn internal /nix overlay prep worker")?;
    let running = NixPrepStatus::running(child.id(), log_path);
    status::write_running_unless_terminal(&status_path, &running).map(|_| ())
}

pub(in crate::guest_init) fn run_prep_to_status() -> Result<()> {
    let env_contract = CangEnv::from_process_env()?;
    if !env_contract.nix_overlay {
        return Ok(());
    }
    let status_path = PathBuf::from(NIX_STATUS_PATH);
    let log_path = PathBuf::from(NIX_LOG_PATH);
    let pid = std::process::id();
    if std::env::var(WAIT_FOR_STATUS_ENV).as_deref() == Ok("1") {
        wait_for_parent_running_status(&status_path, pid)?;
    } else {
        let running = NixPrepStatus::running(pid, log_path);
        if !status::write_running_unless_terminal(&status_path, &running)? {
            return Ok(());
        }
    }

    let mut profiler =
        crate::guest_init::profile::GuestProfiler::from_process_env("internal nix prep");
    match run_prep(&env_contract, &mut profiler) {
        Ok(()) => status::mark_ready_for_pid(&status_path, pid),
        Err(err) => {
            let message = format!("{err:#}");
            let _ = append_log(&message);
            status::mark_failed_for_pid(&status_path, pid, message)
        }
    }
}

pub(in crate::guest_init) fn run_prep(
    env_contract: &CangEnv,
    profiler: &mut impl ProfileRecorder,
) -> Result<()> {
    if !env_contract.nix_overlay {
        return Ok(());
    }
    if !process::is_root() {
        bail!("internal /nix overlay prep must run as root");
    }
    // A block contract is the launch's own composition, and it does not depend on
    // the host grafting an overlay at `/nix`: when both layers are on the block
    // disk the host grafts nothing (the whole point - one `/nix`, every access on
    // the block path), so the guest's overlay is the only `/nix` this boot has.
    if let Some(contract) = &env_contract.nix_lower {
        return run_block_prep(profiler, contract, env_contract.nix_host_overlay);
    }
    if env_contract.nix_host_overlay {
        return run_host_overlay_prep(profiler);
    }

    profiler.measure_result(PROFILE_REQUIRE_TOOLS, || {
        for tool in ["blkid", "mount", "findmnt", "nix-daemon"] {
            command::require_on_path(tool)?;
        }
        Ok(())
    })?;

    let disk = profiler.measure_result(PROFILE_FIND_DISK, || {
        crate::guest_init::components::disk::nix::find_disk(
            &env_contract.nix_disk_label,
            &env_contract.nix_disk_id,
        )
    })?;
    let lower_dir = Path::new("/nix");
    let run_dir = Path::new("/run/cang");
    let disk_mount = run_dir.join("nix-disk");
    let upper_dir = disk_mount.join("upper");
    let work_dir = disk_mount.join("work");

    profiler.measure_result(PROFILE_PREPARE_RUN_DIRS, || {
        fs::create_dir_all(run_dir)?;
        fs::create_dir_all(&disk_mount)?;
        Ok(())
    })?;

    profiler.measure_result(PROFILE_MOUNT_DISK, || {
        if !findmnt(&disk_mount)? {
            command::run(
                "mount",
                &["-t", "btrfs", path_str(&disk)?, path_str(&disk_mount)?],
            )
            .context("failed to mount internal /nix btrfs disk")?;
        }
        Ok(())
    })?;

    profiler.measure_result(PROFILE_PRESEED_UPPER, || {
        preseed_upper(lower_dir, &upper_dir, &work_dir)
    })?;
    let options = overlay_options(lower_dir, &upper_dir, &work_dir);
    profiler.measure_result(PROFILE_MOUNT_OVERLAY, || {
        command::run(
            "mount",
            &["-t", "overlay", "overlay", "-o", &options, "/nix"],
        )
        .context("failed to mount internal overlay at /nix")
    })?;

    finish_prep(profiler)
}

// The merged /nix is an overlay whose upper outlives image generations while its
// lowerdir is one image. The host closes that gap when it seeds a new
// generation: it clones the previous upper (so every substituted object stays
// cached), installs the new image's DB and parks the previous generation's DB
// here. This step then merges those registrations into the live DB and
// deregisters the rows whose objects went with the old image — offline, before
// the daemon starts and before any nix wrapper is handed a ready marker.
//
// Without a parked DB this is the drift repair: restore a registered-but-absent
// object from the substituters, and only deregister a row no cache can satisfy.
//
// Best effort by design: a guest image older than this binary does not ship the
// tool, a broken store must not stop the VM from booting, and the reconcile
// only ever touches paths the DB already claims.
/// Install the image generation's DB that the host staged for this generation.
///
/// A seeded generation is a snapshot of the previous one, so `/nix/var/nix`
/// still belongs to the guest's uid and the *previous* image's DB. The host
/// therefore leaves two directories at the upper's root - `.cang-image-db` (the
/// new image's) and `.cang-previous-db` (the previous generation's, merged
/// below) - and this function does the swap as root, dropping the dead daemon's
/// socket and temproots with it.
fn install_staged_image_db() -> Result<()> {
    if !Path::new(STAGED_IMAGE_DB).is_file() {
        return Ok(());
    }
    let live = Path::new(LIVE_DB_DIR);
    std::fs::create_dir_all(live).with_context(|| format!("failed to create '{LIVE_DB_DIR}'"))?;
    for name in [
        "db.sqlite",
        "db.sqlite-wal",
        "db.sqlite-shm",
        "schema",
        "big-lock",
    ] {
        match std::fs::remove_file(live.join(name)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to replace '{LIVE_DB_DIR}/{name}'"));
            }
        }
    }
    // The staged DB is a copy of a real `var/nix/db`, which may carry SQLite's
    // write-ahead log beside the database: installing the DB without it would
    // drop every transaction the log holds.
    for staged in ["db.sqlite", "db.sqlite-wal", "db.sqlite-shm", "schema"] {
        let from = Path::new(STAGED_IMAGE_DB_DIR).join(staged);
        if from.is_file() {
            std::fs::copy(&from, live.join(staged)).with_context(|| {
                format!("failed to install the image Nix DB '{}'", from.display())
            })?;
        }
    }
    for stale in ["daemon-socket", "temproots"] {
        let path = Path::new("/nix/var/nix").join(stale);
        if let Err(err) = std::fs::remove_dir_all(&path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "cang-guest-init: warning: failed to remove '{}': {err}",
                path.display()
            );
        }
    }
    if let Err(err) = std::fs::remove_dir_all(Path::new(STAGED_IMAGE_DB_DIR)) {
        eprintln!("cang-guest-init: warning: failed to remove '{STAGED_IMAGE_DB_DIR}': {err}");
    }
    Ok(())
}

fn repair_store(profiler: &mut impl ProfileRecorder) -> Result<()> {
    profiler.measure_result(PROFILE_REPAIR_STORE, || {
        install_staged_image_db()?;
        if command::find_on_path("cang-nix-store-repair").is_none() {
            eprintln!(
                "cang-guest-init: warning: cang-nix-store-repair is not on PATH; skipping /nix store reconciliation"
            );
            return Ok(());
        }
        let parked_db = Path::new(PARKED_PREVIOUS_DB).is_file();
        let args = store_reconcile_args(parked_db);
        match command::run("cang-nix-store-repair", &args) {
            Ok(()) => {
                if parked_db {
                    // The merge is idempotent, but once it succeeded the parked
                    // copy is dead weight inside every future generation.
                    if let Err(err) = std::fs::remove_dir_all(Path::new(PARKED_DB_DIR)) {
                        eprintln!(
                            "cang-guest-init: warning: failed to remove '{}': {err}",
                            PARKED_DB_DIR
                        );
                    }
                }
            }
            Err(err) => eprintln!(
                "cang-guest-init: warning: /nix store reconciliation failed: {err:#}"
            ),
        }
        Ok(())
    })
}

/// A seeded generation ships the previous generation's DB next to the live one.
/// Merging it is a union, and every path it registers is already on disk, so the
/// right policy is purge-only: absence now means "went with the old image", and
/// fetching it would be a download the user did not ask for. The plain drift
/// repair keeps the restore-first policy.
fn store_reconcile_args(parked_db: bool) -> Vec<&'static str> {
    if parked_db {
        vec!["--import-db", PARKED_PREVIOUS_DB, "--purge-only"]
    } else {
        Vec::new()
    }
}

/// What a failure to compose the guest `/nix` overlay means for the boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum BlockLowerFailure {
    /// Nothing else stands in for `/nix`: either the host grafted no overlay for
    /// this launch (both layers on the block disk) or the upper is the build
    /// disk's and the host never seeded it. The failure must fail nix prep rather
    /// than report success over a `/nix` that was never composed.
    Fatal,
    /// The host-composed overlay is mounted at `/nix` underneath, so a failed
    /// guest mount keeps it, which is what a block lower beside a host-share upper
    /// always did.
    KeepHostOverlay,
}

/// Whether a block-mode failure may fall back, from the two things that decide it:
/// whether the host grafted an overlay at `/nix` at all, and whether the upper is
/// the guest's to seed.
///
/// The later placement is read off the upperdir, because that is what tells the
/// guest whether it is the one that has to seed it.
pub(in crate::guest_init) fn block_lower_failure_policy(
    upperdir: &Path,
    host_overlay: bool,
) -> BlockLowerFailure {
    if !host_overlay || block_upper::guest_disk_mount_point(upperdir).is_some() {
        BlockLowerFailure::Fatal
    } else {
        BlockLowerFailure::KeepHostOverlay
    }
}

/// The reason a composed-nothing `/nix` carries into the status file, so `cang`'s
/// diagnostics name the layer that broke instead of an unmounted `/nix`.
const NO_HOST_OVERLAY_REASON: &str = "this launch grafts no host-composed /nix overlay, so a guest overlay that could not be composed has nothing to fall back to";

/// Mount the read-only lower disk, seed the upper when it is the guest's to seed,
/// and compose the `/nix` overlay over both.
///
/// Every step is a profile point so a guest profile shows what the block path
/// cost; whether a failure is fatal is [`block_lower_failure_policy`]'s call.
fn mount_block_lower(
    profiler: &mut impl ProfileRecorder,
    contract: &NixLowerContract,
) -> Result<()> {
    let disk = profiler.measure_result(PROFILE_FIND_LOWER_DISK, || {
        crate::guest_init::components::disk::nix_lower::find_disk(
            &contract.disk_label,
            &contract.disk_id,
        )
    })?;
    let mount = Path::new(crate::guest_init::components::disk::nix_lower::MOUNT_POINT);
    profiler.measure_result(PROFILE_MOUNT_LOWER_DISK, || {
        fs::create_dir_all(mount)?;
        if findmnt(mount)? {
            return Ok(());
        }
        let args = crate::guest_init::components::disk::nix_lower::mount_args(&disk, mount)?;
        command::run("mount", &args).with_context(|| {
            format!(
                "failed to mount the read-only /nix lower disk '{}' at '{}'",
                disk.display(),
                mount.display()
            )
        })
    })?;
    // Overlayfs wants upper and work on one filesystem, empty, and not shared
    // with the host-composed overlay that already holds the same upper: the host
    // therefore writes a second work dir beside it for this mount.
    //
    // Both directories must exist before the mount, and for a build-disk upper
    // neither does: that generation, `upper` and `work` are all the seed's to
    // create, as one unit (a subvolume cannot be made over an existing
    // directory), so this step only prepares the host-share pair.
    let on_guest_disk = block_upper::guest_disk_mount_point(&contract.upperdir).is_some();
    profiler.measure_result(PROFILE_PREPARE_BLOCK_DIRS, || {
        if !on_guest_disk {
            fs::create_dir_all(&contract.workdir)?;
            fs::create_dir_all(&contract.upperdir)?;
        }
        Ok(())
    })?;
    if on_guest_disk {
        let outcome = profiler.measure_result(PROFILE_SEED_BLOCK_UPPER, || {
            block_upper::seed_block_upper(
                &contract.upperdir,
                &contract.workdir,
                mount,
                contract.import_upper.as_deref(),
            )
        })?;
        eprintln!(
            "cang-guest-init: /nix upper '{}': {outcome:?}",
            contract.upperdir.display()
        );
    }
    let options = overlay_options(mount, &contract.upperdir, &contract.workdir);
    profiler.measure_result(PROFILE_MOUNT_BLOCK_OVERLAY, || {
        command::run(
            "mount",
            &["-t", "overlay", "overlay", "-o", &options, "/nix"],
        )
        .with_context(|| {
            format!(
                "failed to mount the guest /nix overlay lowerdir='{}' upperdir='{}' workdir='{}'",
                mount.display(),
                contract.upperdir.display(),
                contract.workdir.display()
            )
        })
    })
}

fn overlay_options(lower_dir: &Path, upper_dir: &Path, work_dir: &Path) -> String {
    format!(
        "lowerdir={},upperdir={},workdir={},userxattr",
        lower_dir.display(),
        upper_dir.display(),
        work_dir.display()
    )
}

fn run_host_overlay_prep(profiler: &mut impl ProfileRecorder) -> Result<()> {
    profiler.measure_result(PROFILE_REQUIRE_TOOLS, || {
        command::require_on_path("nix-daemon")
    })?;
    profiler.measure_result(PROFILE_PREPARE_RUN_DIRS, || {
        fs::create_dir_all(Path::new(RUN_DIR))?;
        ensure_host_overlay_nix_is_directory(Path::new("/nix"))
    })?;
    finish_prep(profiler)
}

/// Compose `/nix` in the guest - read-only lower on virtio-blk, writable upper
/// where the contract put it - and start the daemon on it.
///
/// This is the launch's only `/nix` when both layers are on the block disk: the
/// host grafts no overlay for that pairing, so `host_overlay` is what decides
/// whether a failure here may keep one (see [`block_lower_failure_policy`]).
fn run_block_prep(
    profiler: &mut impl ProfileRecorder,
    contract: &NixLowerContract,
    host_overlay: bool,
) -> Result<()> {
    profiler.measure_result(PROFILE_REQUIRE_TOOLS, || {
        command::require_on_path("nix-daemon")?;
        for tool in ["blkid", "mount", "findmnt"] {
            command::require_on_path(tool)?;
        }
        Ok(())
    })?;
    profiler.measure_result(PROFILE_PREPARE_RUN_DIRS, || {
        fs::create_dir_all(Path::new(RUN_DIR))?;
        // The overlay is *mounted at* `/nix`, and with a block pair nothing else
        // provides the directory: the mount point is the whole `/nix` until this
        // step composes it, so it is created rather than required to be there.
        fs::create_dir_all(Path::new("/nix"))?;
        Ok(())
    })?;
    match mount_block_lower(profiler, contract) {
        Ok(()) => {}
        Err(err) => match block_lower_failure_policy(&contract.upperdir, host_overlay) {
            BlockLowerFailure::Fatal => {
                return Err(err).with_context(|| {
                    format!(
                        "{NO_HOST_OVERLAY_REASON} (upperdir '{}', lowerdir '{}')",
                        contract.upperdir.display(),
                        Path::new(crate::guest_init::components::disk::nix_lower::MOUNT_POINT)
                            .display()
                    )
                });
            }
            BlockLowerFailure::KeepHostOverlay => eprintln!(
                "cang-guest-init: read-only /nix lower unusable, keeping the host-composed overlay: {err:#}"
            ),
        },
    }
    finish_prep(profiler)
}

/// What every `/nix` path does once `/nix` is composed: reconcile the store
/// against its DB, prepare the daemon's socket directory on the guest's tmpfs,
/// start the daemon on it, and then make it answer over its own protocol.
fn finish_prep(profiler: &mut impl ProfileRecorder) -> Result<()> {
    repair_store(profiler)?;
    profiler.measure_result(PROFILE_CREATE_SOCKET_DIR, ensure_daemon_socket_dir)?;
    let daemon = profiler.measure_result(PROFILE_START_DAEMON, start_daemon)?;
    profiler.measure_result(PROFILE_CHECK_DAEMON, || verify_daemon_answers(daemon))?;
    Ok(())
}

fn ensure_daemon_socket_dir() -> Result<()> {
    fs::create_dir_all(Path::new(NIX_DAEMON_SOCKET_DIR)).with_context(|| {
        format!("failed to create the nix-daemon socket directory '{NIX_DAEMON_SOCKET_DIR}'")
    })?;
    // Clients that spell the store `--store daemon` (or anything built on
    // `nix-daemon`'s own default) mean the `/nix` path, so that path has to
    // resolve to the tmpfs socket. A symlink is not a socket and cannot lose a
    // binding, so it is safe there. Best effort: a `/nix` that refuses the link
    // only costs clients that spell the store that way.
    if let Err(err) = link_default_daemon_socket(
        Path::new(DEFAULT_DAEMON_SOCKET_PATH),
        Path::new(NIX_DAEMON_SOCKET_PATH),
    ) {
        eprintln!(
            "cang-guest-init: warning: {DEFAULT_DAEMON_SOCKET_PATH} does not resolve to \
             {NIX_DAEMON_SOCKET_PATH} ({err:#}); clients using `--store daemon` will not \
             reach the daemon"
        );
    }
    Ok(())
}

/// Point the default socket path at the tmpfs socket, replacing whatever a
/// previous boot left there (a socket node this guest no longer uses).
pub(in crate::guest_init) fn link_default_daemon_socket(link: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(link) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to replace the stale '{}'", link.display()));
        }
    }
    std::os::unix::fs::symlink(target, link).with_context(|| {
        format!(
            "failed to link '{}' to '{}'",
            link.display(),
            target.display()
        )
    })
}

/// Start the store daemon, on the cores the workload is confined to.
///
/// The daemon is why `nix build` was outside the CPU bound: a build delegates to it
/// over `NIX_REMOTE`, so the builders are its children and inherit *its* affinity,
/// and it is spawned from prep - not from the session - so it would otherwise keep
/// every vCPU (note 04 §5.5). Confining it at spawn puts the daemon and every
/// builder it forks on the reserved-cores side of the mask, and nothing else
/// changes: a `nix build` run without the daemon (local store) is confined already
/// because the builders are then the client's children.
///
/// The range is resolved here, in the parent, and applied in the `pre_exec` hook,
/// where only async-signal-safe work is legal. Its OOM score is deliberately left
/// alone: the daemon inherited the prep worker's protection, and a daemon the
/// guest could lose mid-build is a worse outcome than a slower build.
fn start_daemon() -> Result<Child> {
    let cpu_range = crate::guest_init::runtime::cpu_bound::planned_workload_range();
    let mut command = Command::new("nix-daemon");
    command
        .arg("--daemon")
        .env(DAEMON_SOCKET_ENV, NIX_DAEMON_SOCKET_PATH);
    // SAFETY: `pre_exec` runs between `fork` and `exec`, where one
    // `sched_setaffinity` on a pre-resolved range is what the child may do.
    unsafe {
        command.pre_exec(move || {
            crate::guest_init::runtime::cpu_bound::apply_workload_range(cpu_range)
        });
    }
    command.spawn().context("failed to start nix-daemon")
}

/// A socket node is not a working daemon.
///
/// The session is gated on this prep reaching `ready`, so what "ready" must mean
/// is that a client asking the daemon over its protocol gets an answer - not that
/// a socket-shaped inode exists. Failing that, drop the stale node, restart the
/// daemon once, and check again. A daemon that still does not answer is logged
/// and the session continues: a broken daemon must not take a session down that
/// never uses nix, and the next `nix` command reports it either way.
fn verify_daemon_answers(mut daemon: Child) -> Result<()> {
    match handshake(DAEMON_CHECK_ATTEMPTS, DAEMON_CHECK_INTERVAL, daemon_answers) {
        Ok(()) => return Ok(()),
        Err(err) => eprintln!(
            "cang-guest-init: nix-daemon did not answer on {NIX_DAEMON_SOCKET_PATH} ({err:#}); \
             restarting it once"
        ),
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_file(Path::new(NIX_DAEMON_SOCKET_PATH));
    let _restarted = start_daemon()?;
    if let Err(err) = handshake(DAEMON_CHECK_ATTEMPTS, DAEMON_CHECK_INTERVAL, daemon_answers) {
        eprintln!(
            "cang-guest-init: warning: nix-daemon does not answer on {NIX_DAEMON_SOCKET_PATH} \
             after a restart ({err:#}); nix commands in this session will fail"
        );
    }
    Ok(())
}

/// Retry a check that may legitimately fail while the daemon binds its socket,
/// bounded in attempts so a dead daemon cannot hold prep.
fn handshake(
    attempts: usize,
    interval: Duration,
    mut answer: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut last = anyhow!("no nix-daemon check was attempted");
    for attempt in 1..=attempts {
        match answer() {
            Ok(()) => return Ok(()),
            Err(err) => last = err,
        }
        if attempt < attempts {
            std::thread::sleep(interval);
        }
    }
    Err(last)
}

fn daemon_answers() -> Result<()> {
    let store = nix_remote_uri();
    match command::output_trimmed(
        "timeout",
        &[
            DAEMON_CHECK_BOUND,
            "nix",
            "store",
            "info",
            "--store",
            store.as_str(),
            "--json",
        ],
    )? {
        Some(_) => Ok(()),
        None => {
            bail!("'nix store info --store {store}' did not answer within {DAEMON_CHECK_BOUND}s")
        }
    }
}

fn ensure_host_overlay_nix_is_directory(nix_dir: &Path) -> Result<()> {
    let metadata = std::fs::metadata(nix_dir).with_context(|| {
        format!(
            "cang host-overlay /nix path '{}' is missing",
            nix_dir.display()
        )
    })?;
    if !metadata.is_dir() {
        bail!(
            "cang host-overlay /nix path '{}' is not a directory",
            nix_dir.display()
        );
    }
    Ok(())
}

fn wait_for_parent_running_status(status_path: &Path, pid: u32) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let current = status::read_status(status_path)?;
        if current.state == NixPrepState::Running && current.pid == Some(pid) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("timed out waiting for parent to publish nix prep running status for pid {pid}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn append_log(message: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(NIX_LOG_PATH)?;
    writeln!(file, "{message}")?;
    Ok(())
}

fn preseed_upper(lower_dir: &Path, upper_dir: &Path, work_dir: &Path) -> Result<()> {
    preseed_upper_with(lower_dir, upper_dir, work_dir, copy_lower_var, repair_upper)
}

fn preseed_upper_with(
    lower_dir: &Path,
    upper_dir: &Path,
    work_dir: &Path,
    mut copy_var: impl FnMut(&Path, &Path) -> Result<()>,
    mut repair: impl FnMut(&Path) -> Result<()>,
) -> Result<()> {
    let state = classify_preseed_state(upper_dir);
    prepare_upper_dirs(upper_dir, work_dir)?;

    match state {
        PreseedState::Completed => {
            repair(upper_dir)?;
            remove_file_if_exists(&attempt_marker(upper_dir));
        }
        PreseedState::LegacySeeded => {
            repair(upper_dir)?;
            write_completion_sentinel(upper_dir)?;
        }
        PreseedState::FreshOrRetry => {
            let copied = if lower_dir.join("var").is_dir() {
                write_attempt_marker(upper_dir)?;
                copy_var(&lower_dir.join("var"), &upper_dir.join("var"))?;
                true
            } else {
                false
            };
            repair(upper_dir)?;
            if copied {
                write_completion_sentinel(upper_dir)?;
                remove_file_if_exists(&attempt_marker(upper_dir));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreseedState {
    Completed,
    LegacySeeded,
    FreshOrRetry,
}

fn classify_preseed_state(upper_dir: &Path) -> PreseedState {
    if completion_sentinel(upper_dir).exists() {
        return PreseedState::Completed;
    }
    if !attempt_marker(upper_dir).exists() && legacy_nix_state_exists(upper_dir) {
        return PreseedState::LegacySeeded;
    }
    PreseedState::FreshOrRetry
}

fn legacy_nix_state_exists(upper_dir: &Path) -> bool {
    let nix_var = upper_dir.join("var/nix");
    nix_var.join("db").exists() || nix_var.join("profiles").exists()
}

fn prepare_upper_dirs(upper_dir: &Path, work_dir: &Path) -> Result<()> {
    fs::create_dir_all(upper_dir)?;
    fs::create_dir_all(work_dir)?;
    fs::create_dir_all(&upper_dir.join("store"))?;
    fs::create_dir_all(&upper_dir.join("var"))?;
    Ok(())
}

fn copy_lower_var(lower_var: &Path, upper_var: &Path) -> Result<()> {
    let source = format!("{}/.", lower_var.display());
    command::run("cp", &["-a", "--no-clobber", &source, path_str(upper_var)?])
        .context("failed to preseed internal upperdir /nix/var from image lowerdir")
}

fn repair_upper(upper_dir: &Path) -> Result<()> {
    fs::create_dir_all(&upper_dir.join("var/nix"))?;
    command::run("chown", &[":nixbld", path_str(&upper_dir.join("store"))?])?;
    fs::chmod(&upper_dir.join("store"), 0o1775)?;
    fs::chmod(&upper_dir.join("var"), 0o755)?;
    fs::chmod(&upper_dir.join("var/nix"), 0o755)?;
    Ok(())
}

fn write_attempt_marker(upper_dir: &Path) -> Result<()> {
    fs::write_file(&attempt_marker(upper_dir), "attempted\n", 0o644)
}

fn write_completion_sentinel(upper_dir: &Path) -> Result<()> {
    fs::write_file(&completion_sentinel(upper_dir), "preseeded\n", 0o644)
}

fn remove_file_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            eprintln!(
                "cang-guest-init: warning: failed to remove {}: {err}",
                path.display()
            );
        }
    }
}

fn completion_sentinel(upper_dir: &Path) -> std::path::PathBuf {
    upper_dir.join(PRESEED_COMPLETION_SENTINEL)
}

fn attempt_marker(upper_dir: &Path) -> std::path::PathBuf {
    upper_dir.join(PRESEED_ATTEMPT_MARKER)
}

fn findmnt(path: &Path) -> Result<bool> {
    command::status_ok("findmnt", &["-rn", path_str(path)?])
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

#[cfg(test)]
#[path = "root_tests.rs"]
mod tests;
