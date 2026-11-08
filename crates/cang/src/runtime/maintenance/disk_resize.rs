//! Grow-only resize for the workspace's raw persistent disks.
//!
//! `container-store resize` and `build-disk resize` do the same two things: grow
//! the host sparse image, then boot one narrow maintenance VM that mounts the
//! image and has the guest expand its btrfs filesystem. Only the disk, the
//! guest-side resize target and the report label differ, so they differ here as
//! one enum.

use anyhow::{Context, Result, anyhow, bail};
use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::DiskMaintenanceOptions;
use crate::runtime::fork_child;
use crate::runtime::launch::components::persistent_disks::raw_btrfs::RawBtrfsDisk;
use crate::runtime::launch::components::persistent_disks::{
    build_disk_attachment, build_disk_env_pairs, container_store_attachment,
    container_store_raw_disk_env_pairs, grow_build_disk, grow_container_store,
};
use crate::runtime::launch::config::{DiskAttachment, LaunchConfig, NetworkMode};
use crate::runtime::launch::plan::ImageSelection;
use crate::runtime::session::rootfs::guest_init::resolve_guest_init_with_entrypoint;
use crate::runtime::session::rootfs::image_source::{HostBuildahCommands, OciProcessConfig};
use crate::runtime::session::rootfs::task::{
    HostBtrfsRootfsCommands, TaskRootfsLease, TaskRootfsManager,
};
use crate::runtime::session::task_control::{
    ProcfsInspector, WorkspaceTaskGateReport, ensure_workspace_has_no_running_tasks,
};
use crate::runtime::vm::gpu::GpuMode;
use crate::runtime::vm::libkrun::{DirectLibkrunLauncher, LinkedLibkrunApi};
use crate::runtime::vm::prepared_root;
use crate::state::StateLayout;

const CONTAINER_STORE_TASK_PREFIX: &str = "container-store-resize";
const BUILD_DISK_TASK_PREFIX: &str = "build-disk-resize";

/// Which workspace raw disk a maintenance run grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiskKind {
    ContainerStore,
    BuildDisk,
}

impl DiskKind {
    fn grow(self, state_root: &Path, target_size_bytes: u64) -> Result<RawBtrfsDisk> {
        match self {
            Self::ContainerStore => grow_container_store(state_root, target_size_bytes),
            Self::BuildDisk => grow_build_disk(state_root, target_size_bytes),
        }
    }

    fn attachment(self, disk: &RawBtrfsDisk) -> DiskAttachment {
        match self {
            Self::ContainerStore => container_store_attachment(disk),
            Self::BuildDisk => build_disk_attachment(disk),
        }
    }

    fn env_pairs(self, disk: &RawBtrfsDisk) -> Vec<(String, String)> {
        match self {
            Self::ContainerStore => container_store_raw_disk_env_pairs(disk).to_vec(),
            Self::BuildDisk => build_disk_env_pairs(disk).to_vec(),
        }
    }

    /// The `cang-guest-init internal resize` target the maintenance VM runs.
    fn guest_target(self) -> &'static str {
        match self {
            Self::ContainerStore => "containers",
            Self::BuildDisk => "build",
        }
    }

    fn hostname(self) -> &'static str {
        match self {
            Self::ContainerStore => "cang-container-store-maintenance",
            Self::BuildDisk => "cang-build-disk-maintenance",
        }
    }

    fn task_prefix(self) -> &'static str {
        match self {
            Self::ContainerStore => CONTAINER_STORE_TASK_PREFIX,
            Self::BuildDisk => BUILD_DISK_TASK_PREFIX,
        }
    }

    fn report_action(self) -> &'static str {
        match self {
            Self::ContainerStore => "resized-container-store",
            Self::BuildDisk => "resized-build-disk",
        }
    }

    /// How the size parser names the disk in its errors.
    fn size_label(self) -> &'static str {
        match self {
            Self::ContainerStore => "container-store size",
            Self::BuildDisk => "build disk size",
        }
    }

    /// The command a user reruns after the host image grew but the guest resize
    /// failed - rerunnable, because both disks accept the already-reached size.
    fn rerun_command(self, size: &str) -> String {
        match self {
            Self::ContainerStore => format!("cang container-store resize --size {size}"),
            Self::BuildDisk => format!("cang build-disk resize --size {size}"),
        }
    }
}

/// Grow `kind`'s host image to `size`, then have a maintenance VM expand its
/// btrfs filesystem to the new device size.
pub(crate) fn resize(
    kind: DiskKind,
    size: &str,
    options: &DiskMaintenanceOptions,
    runner: &impl MaintenanceVmRunner,
) -> Result<String> {
    let layout = current_state_layout()?;
    let task_report = ensure_workspace_has_no_running_tasks(layout.root_dir(), &ProcfsInspector)?;
    let target_size_bytes = parse_size_bytes(size, kind)?;
    let disk = kind.grow(layout.root_dir(), target_size_bytes)?;

    run_guest_resize(kind, &layout, options, &disk, runner).with_context(|| {
        format!(
            "cang grew the host {} disk '{}' to {} bytes but failed to resize the guest btrfs filesystem; fix the reported VM/guest issue and rerun `{}`",
            kind.size_label(),
            disk.path.display(),
            target_size_bytes,
            kind.rerun_command(size)
        )
    })?;

    Ok(render_report(
        kind.report_action(),
        &disk.path,
        disk.size_bytes,
        &task_report,
    ))
}

fn run_guest_resize(
    kind: DiskKind,
    layout: &StateLayout,
    options: &DiskMaintenanceOptions,
    disk: &RawBtrfsDisk,
    runner: &impl MaintenanceVmRunner,
) -> Result<()> {
    let manager = TaskRootfsManager::new(layout.root_dir().to_path_buf(), layout.image_cache_dir());
    let task_id = maintenance_task_id(kind.task_prefix())?;
    let handle = manager.materialize_btrfs_from_buildah(
        &image_selection(options),
        &task_id,
        false,
        &HostBuildahCommands,
        &HostBtrfsRootfsCommands,
    )?;
    let lease = TaskRootfsLease::new(handle, HostBtrfsRootfsCommands);
    let run_result = {
        let handle = lease.handle();
        let guest_init = resolve_guest_init_with_entrypoint(
            handle.rootfs_path(),
            options.guest_init.as_deref(),
            &handle.process_config().entrypoint,
        )?;
        let config = maintenance_launch_config(
            kind,
            handle.rootfs_path(),
            &guest_init.guest_exec_path,
            guest_init.override_mount,
            disk,
            options,
            handle.process_config(),
        )?;
        runner.run(&config, handle.task_dir())
    };
    let cleanup_result = lease.cleanup();
    match (run_result, cleanup_result) {
        (Ok(()), Ok(_)) => Ok(()),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(run_error), Ok(_)) => Err(run_error),
        (Err(run_error), Err(cleanup_error)) => Err(cleanup_error.context(format!(
            "failed to clean cang resize maintenance rootfs after guest resize error: {run_error:#}"
        ))),
    }
}

fn maintenance_launch_config(
    kind: DiskKind,
    task_rootfs: &Path,
    guest_init_exec: &str,
    guest_init_override: Option<crate::runtime::launch::config::GuestInitOverrideMount>,
    disk: &RawBtrfsDisk,
    options: &DiskMaintenanceOptions,
    process_config: &OciProcessConfig,
) -> Result<LaunchConfig> {
    let mut env = kind.env_pairs(disk);
    if let Some(path) = process_config
        .env
        .iter()
        .find_map(|entry| entry.strip_prefix("PATH=").map(str::to_owned))
    {
        env.push(("PATH".to_owned(), path));
    }
    if options.log_settings.level.enables_debug() {
        env.push(("CANG_GUEST_DEBUG".to_owned(), "1".to_owned()));
    }

    Ok(LaunchConfig {
        task_rootfs: task_rootfs.to_path_buf(),
        hostname: kind.hostname().to_owned(),
        mounts: Vec::new(),
        host_nix_overlay: None,
        guest_init_override,
        disks: vec![kind.attachment(disk)],
        ram_mib: maintenance_ram_mib(options.mem_gib)?,
        vcpus: 1,
        log_level: options.log_settings.level,
        network_mode: NetworkMode::Tsi,
        gpu_mode: GpuMode::Off,
        gpu_shm_bytes: None,
        zero_copy_shm: false,
        zero_message_open: None,
        no_flush: None,
        cache_symlinks: None,
        negative_entry_millis: None,
        new_perms: crate::runtime::launch::config::GuestPermissions::default(),
        publish: Vec::new(),
        workdir: "/".to_owned(),
        exec_path: guest_init_exec.to_owned(),
        argv: vec![
            "internal".to_owned(),
            "resize".to_owned(),
            kind.guest_target().to_owned(),
        ],
        env,
        guest_config_env: Vec::new(),
        passt_fd: None,
        pulse_bridge: None,
        waypipe: None,
        exec: None,
        managed_session: None,
        seccomp: Default::default(),
        landlock: crate::runtime::landlock::LandlockMode::Off,
    })
}

fn maintenance_ram_mib(mem_gib: Option<u32>) -> Result<u32> {
    let gib = mem_gib.unwrap_or(1);
    if gib == 0 {
        bail!("maintenance VM memory must be greater than 0 GiB");
    }
    gib.checked_mul(1024)
        .ok_or_else(|| anyhow!("maintenance VM memory is too large"))
}

pub(crate) trait MaintenanceVmRunner {
    fn run(&self, config: &LaunchConfig, task_state_dir: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostMaintenanceVmRunner;

impl MaintenanceVmRunner for HostMaintenanceVmRunner {
    fn run(&self, config: &LaunchConfig, task_state_dir: &Path) -> Result<()> {
        let prepared_root = prepared_root::prepare(config, task_state_dir)?;
        let launch_config = config.with_root_export(prepared_root.root().to_path_buf());
        launch_config.write_guest_config_to_rootfs()?;
        // libkrun's VMM ends the process it runs in, so the guest runs in a
        // forked child: the parent keeps the prepared-root mounts, can unmount
        // them and let the caller delete the task state dir, and learns whether
        // the guest resize actually succeeded.
        let guest_status = run_vmm_in_child(&launch_config)?;
        let unmount_result = prepared_root.unmount();
        match (guest_status, unmount_result) {
            (0, Ok(())) => Ok(()),
            (0, Err(err)) => Err(err),
            (code, Ok(())) => bail!("cang maintenance VM exited with status {code}"),
            (code, Err(err)) => {
                Err(err.context(format!("cang maintenance VM exited with status {code}")))
            }
        }
    }
}

fn run_vmm_in_child(config: &LaunchConfig) -> Result<i32> {
    // SAFETY: fork only duplicates this process. The maintenance CLI has not
    // started the VMM yet and runs no other thread, so the child has no
    // inherited lock to block on.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!(
            "failed to fork the cang maintenance VM: {}",
            std::io::Error::last_os_error()
        );
    }
    if pid == 0 {
        let api = LinkedLibkrunApi::new();
        let result = DirectLibkrunLauncher::new(api).start_enter_profiled_with_pre_enter_hook(
            config,
            None,
            || Ok(()),
        );
        if let Err(err) = result {
            fork_child::report_child_error(format!("cang maintenance VM: {err:#}\n").as_bytes());
            fork_child::exit_child(1);
        }
        fork_child::exit_child(0);
    }
    wait_for_guest_exit(pid)
}

fn wait_for_guest_exit(pid: libc::pid_t) -> Result<i32> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waiting on the child forked immediately above.
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc >= 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err).context("failed to wait for the cang maintenance VM");
    }
    if libc::WIFEXITED(status) {
        return Ok(libc::WEXITSTATUS(status));
    }
    if libc::WIFSIGNALED(status) {
        bail!(
            "cang maintenance VM was killed by signal {}",
            libc::WTERMSIG(status)
        );
    }
    bail!("cang maintenance VM ended with an unrecognised wait status {status}")
}

pub(super) fn current_state_layout() -> Result<StateLayout> {
    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory for cang container-store maintenance")?;
    let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home_dir = env::var_os("HOME").map(PathBuf::from);
    let config =
        crate::config::state::read_config(xdg_config_home.as_deref(), home_dir.as_deref())?;
    crate::state::resolve_state_layout_from_parts(
        &cwd,
        xdg_state_home.as_deref(),
        home_dir.as_deref(),
        config.state_location_override(),
    )
}

fn image_selection(options: &DiskMaintenanceOptions) -> ImageSelection {
    match &options.image {
        Some(reference) => ImageSelection::Explicit {
            reference: reference.clone(),
        },
        None if options.pull_latest => ImageSelection::CanonicalWithRefresh,
        None => ImageSelection::PreferLocalhostThenCanonical,
    }
}

fn maintenance_task_id(prefix: &str) -> Result<String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs();
    Ok(format!("{prefix}-{ts}"))
}

pub(super) fn render_report(
    action: &str,
    path: &Path,
    size_bytes: u64,
    task_report: &WorkspaceTaskGateReport,
) -> String {
    let mut output = String::new();
    if !task_report.stale_task_ids.is_empty() {
        output.push_str(&format!(
            "stale-tasks-ignored\t{}\n",
            task_report.stale_task_ids.join(",")
        ));
    }
    output.push_str(&format!("{action}\t{}\t{}\n", path.display(), size_bytes));
    output
}

fn parse_size_bytes(value: &str, kind: DiskKind) -> Result<u64> {
    let label = kind.size_label();
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    if value.starts_with('-') || value.starts_with('+') {
        bail!("{label} must be an unsigned whole number");
    }
    let digit_len = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if digit_len == 0 {
        bail!("{label} must start with digits");
    }
    let (number, suffix) = value.split_at(digit_len);
    if number.len() != value.len() && value[digit_len..].starts_with('.') {
        bail!("{label} must be a whole number, not a fraction");
    }
    let amount = number
        .parse::<u64>()
        .with_context(|| format!("{label} number is too large"))?;
    if amount == 0 {
        bail!("{label} must be greater than zero");
    }
    let multiplier = size_multiplier(suffix, label)?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("{label} overflows u64 bytes"))
}

fn size_multiplier(suffix: &str, label: &str) -> Result<u64> {
    match suffix.to_ascii_lowercase().as_str() {
        "" => Ok(1),
        "k" | "kib" => Ok(1024),
        "m" | "mib" => Ok(1024_u64.pow(2)),
        "g" | "gib" => Ok(1024_u64.pow(3)),
        "t" | "tib" => Ok(1024_u64.pow(4)),
        _ => bail!(
            "unsupported {label} suffix '{suffix}'; use bytes, K, M, G, T, KiB, MiB, GiB, or TiB"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogLevel;
    use crate::runtime::launch::components::persistent_disks::raw_btrfs::{
        RawBtrfsDisk, RawBtrfsDiskStatus,
    };

    #[test]
    fn size_parser_accepts_frozen_whole_number_binary_suffix_grammar() {
        for kind in [DiskKind::ContainerStore, DiskKind::BuildDisk] {
            assert_eq!(parse_size_bytes("1", kind).unwrap(), 1);
            assert_eq!(parse_size_bytes("1K", kind).unwrap(), 1024);
            assert_eq!(parse_size_bytes("2m", kind).unwrap(), 2 * 1024_u64.pow(2));
            assert_eq!(parse_size_bytes("3GiB", kind).unwrap(), 3 * 1024_u64.pow(3));
            assert_eq!(parse_size_bytes("4tib", kind).unwrap(), 4 * 1024_u64.pow(4));
            assert_eq!(
                parse_size_bytes(" 128G ", kind).unwrap(),
                128 * 1024_u64.pow(3)
            );
        }
    }

    #[test]
    fn size_parser_rejects_invalid_values() {
        for value in [
            "",
            "   ",
            "0",
            "-1",
            "+1",
            "1.5G",
            "1GB",
            "G",
            "18446744073709551616",
            "18446744073709551615T",
        ] {
            parse_size_bytes(value, DiskKind::ContainerStore)
                .expect_err("invalid size should fail");
        }
    }

    /// The same grammar under each disk's own label, so a build-disk error names
    /// the build disk rather than the container store.
    #[test]
    fn size_parser_names_the_disk_it_parsed() {
        let err = parse_size_bytes("1.5G", DiskKind::BuildDisk).expect_err("fraction should fail");
        assert!(format!("{err:#}").contains("build disk size"), "{err:#}");
        assert!(!format!("{err:#}").contains("container-store"), "{err:#}");
    }

    #[test]
    fn maintenance_launch_config_is_slim_and_targets_internal_container_resize() {
        let disk = test_disk(
            Path::new("/state/cang-containers.raw"),
            "cang-containers",
            "CANG_CONTAINERS",
        );
        let config = maintenance_launch_config(
            DiskKind::ContainerStore,
            Path::new("/rootfs"),
            "/nix/store/hash/bin/cang-guest-init",
            None,
            &disk,
            &test_options(),
            &OciProcessConfig::default(),
        )
        .expect("config should build");

        assert!(config.mounts.is_empty());
        assert!(config.host_nix_overlay.is_none());
        assert_eq!(config.disks.len(), 1);
        assert_eq!(config.disks[0].id, "cang-containers");
        assert_eq!(config.hostname, "cang-container-store-maintenance");
        assert_eq!(config.exec_path, "/nix/store/hash/bin/cang-guest-init");
        assert_eq!(config.argv, ["internal", "resize", "containers"]);
        assert!(
            config
                .env
                .iter()
                .any(|(key, value)| key == "CANG_CONTAINERS_STORE" && value == "raw-disk")
        );
        assert!(config.guest_config_env.is_empty());
    }

    /// The build disk's maintenance VM attaches only `cang-build.raw` and asks the
    /// guest to resize that one; the guest finds it by the id and label it ships.
    #[test]
    fn build_disk_maintenance_config_targets_internal_build_resize() {
        let disk = test_disk(
            Path::new("/state/cang-build.raw"),
            "cang-build",
            "CANG_BUILD",
        );
        let config = maintenance_launch_config(
            DiskKind::BuildDisk,
            Path::new("/rootfs"),
            "/nix/store/hash/bin/cang-guest-init",
            None,
            &disk,
            &test_options(),
            &OciProcessConfig::default(),
        )
        .expect("config should build");

        assert_eq!(config.disks.len(), 1);
        assert_eq!(config.disks[0].id, "cang-build");
        assert_eq!(config.disks[0].path, Path::new("/state/cang-build.raw"));
        assert_eq!(config.hostname, "cang-build-disk-maintenance");
        assert_eq!(config.argv, ["internal", "resize", "build"]);
        assert!(
            config
                .env
                .iter()
                .any(|(key, value)| key == "CANG_BUILD_DISK_ID" && value == "cang-build")
        );
        assert!(
            config
                .env
                .iter()
                .any(|(key, value)| key == "CANG_BUILD_DISK_LABEL" && value == "CANG_BUILD")
        );
    }

    #[test]
    fn render_report_includes_stale_task_info_without_blocking() {
        let output = render_report(
            "reset-container-store",
            Path::new("/state/cang-containers.raw"),
            64,
            &WorkspaceTaskGateReport {
                stale_task_ids: vec!["task-a".to_owned(), "task-b".to_owned()],
            },
        );

        assert!(output.contains("stale-tasks-ignored\ttask-a,task-b"));
        assert!(output.contains("reset-container-store\t/state/cang-containers.raw\t64"));
    }

    #[test]
    fn host_grow_failure_message_is_not_labeled_as_guest_failure() {
        let err =
            parse_size_bytes("1.5G", DiskKind::ContainerStore).expect_err("fraction should fail");
        assert!(!format!("{err:#}").contains("guest btrfs"));
    }

    #[test]
    fn rerun_command_matches_each_disk_subcommand() {
        assert_eq!(
            DiskKind::ContainerStore.rerun_command("128G"),
            "cang container-store resize --size 128G"
        );
        assert_eq!(
            DiskKind::BuildDisk.rerun_command("100G"),
            "cang build-disk resize --size 100G"
        );
    }

    fn test_options() -> DiskMaintenanceOptions {
        DiskMaintenanceOptions {
            image: None,
            pull_latest: false,
            guest_init: None,
            mem_gib: None,
            log_settings: crate::logging::LogSettings::resolve(Some(LogLevel::Info), false, None),
        }
    }

    fn test_disk(path: &Path, id: &str, label: &str) -> RawBtrfsDisk {
        RawBtrfsDisk {
            path: path.to_path_buf(),
            id: id.to_owned(),
            label: label.to_owned(),
            size_bytes: 128 * 1024 * 1024 * 1024,
            status: RawBtrfsDiskStatus::Reused,
        }
    }
}
