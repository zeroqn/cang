use anyhow::{Context, Result, anyhow, bail};
use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::{ContainerStoreCommand, ContainerStoreOptions};
use crate::runtime::launch::components::persistent_disks::{
    container_store_attachment, container_store_default_size_bytes,
    container_store_raw_disk_env_pairs, grow_container_store, recreate_container_store,
};
use crate::runtime::launch::config::{LaunchConfig, NetworkMode};
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

const MAINTENANCE_TASK_PREFIX: &str = "container-store-resize";

pub(crate) fn run(
    command: ContainerStoreCommand,
    options: ContainerStoreOptions,
) -> Result<String> {
    match command {
        ContainerStoreCommand::Resize { size } => resize(&size, options, &HostMaintenanceVmRunner),
        ContainerStoreCommand::Reset { force } => reset(force),
    }
}

fn resize(
    size: &str,
    options: ContainerStoreOptions,
    runner: &impl MaintenanceVmRunner,
) -> Result<String> {
    let layout = current_state_layout()?;
    let task_report = ensure_workspace_has_no_running_tasks(layout.root_dir(), &ProcfsInspector)?;
    let target_size_bytes = parse_size_bytes(size)?;
    let disk = grow_container_store(layout.root_dir(), target_size_bytes)?;

    run_guest_resize(&layout, &options, &disk, runner).with_context(|| {
        format!(
            "cang grew the host container-store disk '{}' to {} bytes but failed to resize the guest btrfs filesystem; fix the reported VM/guest issue and rerun `cang container-store resize --size {}`",
            disk.path.display(),
            target_size_bytes,
            size
        )
    })?;

    Ok(render_report(
        "resized-container-store",
        &disk.path,
        disk.size_bytes,
        &task_report,
    ))
}

fn reset(force: bool) -> Result<String> {
    if !force {
        bail!(
            "cang container-store reset is destructive; rerun with --force to delete and recreate the raw container-store disk"
        );
    }
    let layout = current_state_layout()?;
    let task_report = ensure_workspace_has_no_running_tasks(layout.root_dir(), &ProcfsInspector)?;
    let disk = recreate_container_store(layout.root_dir())?;

    Ok(render_report(
        "reset-container-store",
        &disk.path,
        container_store_default_size_bytes(),
        &task_report,
    ))
}

fn run_guest_resize(
    layout: &StateLayout,
    options: &ContainerStoreOptions,
    disk: &crate::runtime::launch::components::persistent_disks::raw_btrfs::RawBtrfsDisk,
    runner: &impl MaintenanceVmRunner,
) -> Result<()> {
    let manager = TaskRootfsManager::new(layout.root_dir().to_path_buf(), layout.image_cache_dir());
    let task_id = maintenance_task_id()?;
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
    task_rootfs: &Path,
    guest_init_exec: &str,
    guest_init_override: Option<crate::runtime::launch::config::GuestInitOverrideMount>,
    disk: &crate::runtime::launch::components::persistent_disks::raw_btrfs::RawBtrfsDisk,
    options: &ContainerStoreOptions,
    process_config: &OciProcessConfig,
) -> Result<LaunchConfig> {
    let mut env = container_store_raw_disk_env_pairs(disk).to_vec();
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
        hostname: "cang-container-store-maintenance".to_owned(),
        mounts: Vec::new(),
        host_nix_overlay: None,
        guest_init_override,
        disks: vec![container_store_attachment(disk)],
        ram_mib: maintenance_ram_mib(options.mem_gib)?,
        vcpus: 1,
        log_level: options.log_settings.level,
        network_mode: NetworkMode::Tsi,
        gpu_mode: GpuMode::Off,
        zero_copy_shm: false,
        new_perms: crate::runtime::launch::config::GuestPermissions::default(),
        publish: Vec::new(),
        workdir: "/".to_owned(),
        exec_path: guest_init_exec.to_owned(),
        argv: vec![
            "internal".to_owned(),
            "resize".to_owned(),
            "containers".to_owned(),
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

trait MaintenanceVmRunner {
    fn run(&self, config: &LaunchConfig, task_state_dir: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct HostMaintenanceVmRunner;

impl MaintenanceVmRunner for HostMaintenanceVmRunner {
    fn run(&self, config: &LaunchConfig, task_state_dir: &Path) -> Result<()> {
        let prepared_root = prepared_root::prepare(config, task_state_dir)?;
        let launch_config = config.with_root_export(prepared_root.root().to_path_buf());
        launch_config.write_guest_config_to_rootfs()?;
        let api = LinkedLibkrunApi::new();
        DirectLibkrunLauncher::new(api).start_enter_profiled_with_pre_enter_hook(
            &launch_config,
            None,
            || Ok(()),
        )
    }
}

fn current_state_layout() -> Result<StateLayout> {
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

fn image_selection(options: &ContainerStoreOptions) -> ImageSelection {
    match &options.image {
        Some(reference) => ImageSelection::Explicit {
            reference: reference.clone(),
        },
        None if options.pull_latest => ImageSelection::CanonicalWithRefresh,
        None => ImageSelection::PreferLocalhostThenCanonical,
    }
}

fn maintenance_task_id() -> Result<String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?
        .as_secs();
    Ok(format!("{MAINTENANCE_TASK_PREFIX}-{ts}"))
}

fn render_report(
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

fn parse_size_bytes(value: &str) -> Result<u64> {
    let value = value.trim();
    if value.is_empty() {
        bail!("container-store size must not be empty");
    }
    if value.starts_with('-') || value.starts_with('+') {
        bail!("container-store size must be an unsigned whole number");
    }
    let digit_len = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if digit_len == 0 {
        bail!("container-store size must start with digits");
    }
    let (number, suffix) = value.split_at(digit_len);
    if number.len() != value.len() && value[digit_len..].starts_with('.') {
        bail!("container-store size must be a whole number, not a fraction");
    }
    let amount = number
        .parse::<u64>()
        .context("container-store size number is too large")?;
    if amount == 0 {
        bail!("container-store size must be greater than zero");
    }
    let multiplier = size_multiplier(suffix)?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("container-store size overflows u64 bytes"))
}

fn size_multiplier(suffix: &str) -> Result<u64> {
    match suffix.to_ascii_lowercase().as_str() {
        "" => Ok(1),
        "k" | "kib" => Ok(1024),
        "m" | "mib" => Ok(1024_u64.pow(2)),
        "g" | "gib" => Ok(1024_u64.pow(3)),
        "t" | "tib" => Ok(1024_u64.pow(4)),
        _ => bail!(
            "unsupported container-store size suffix '{suffix}'; use bytes, K, M, G, T, KiB, MiB, GiB, or TiB"
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
        assert_eq!(parse_size_bytes("1").unwrap(), 1);
        assert_eq!(parse_size_bytes("1K").unwrap(), 1024);
        assert_eq!(parse_size_bytes("2m").unwrap(), 2 * 1024_u64.pow(2));
        assert_eq!(parse_size_bytes("3GiB").unwrap(), 3 * 1024_u64.pow(3));
        assert_eq!(parse_size_bytes("4tib").unwrap(), 4 * 1024_u64.pow(4));
        assert_eq!(parse_size_bytes(" 128G ").unwrap(), 128 * 1024_u64.pow(3));
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
            parse_size_bytes(value).expect_err("invalid size should fail");
        }
    }

    #[test]
    fn maintenance_launch_config_is_slim_and_targets_internal_container_resize() {
        let disk = test_disk(Path::new("/state/cang-containers.raw"));
        let config = maintenance_launch_config(
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
        let err = parse_size_bytes("1.5G").expect_err("fraction should fail");
        assert!(!format!("{err:#}").contains("guest btrfs"));
    }

    fn test_options() -> ContainerStoreOptions {
        ContainerStoreOptions {
            image: None,
            pull_latest: false,
            guest_init: None,
            mem_gib: None,
            log_settings: crate::logging::LogSettings::resolve(Some(LogLevel::Info), false, None),
        }
    }

    fn test_disk(path: &Path) -> RawBtrfsDisk {
        RawBtrfsDisk {
            path: path.to_path_buf(),
            id: "cang-containers".to_owned(),
            label: "CANG_CONTAINERS".to_owned(),
            size_bytes: 128 * 1024 * 1024 * 1024,
            status: RawBtrfsDiskStatus::Reused,
        }
    }
}
