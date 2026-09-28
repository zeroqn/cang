use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::RuntimeOptions;
use crate::runtime::session::rootfs::image_source::ImageCacheCommand;
use crate::runtime::session::task_control::TaskControlCommand;
use crate::task_rootfs::TaskRootfsBackend;

pub(crate) mod attach;
mod attach_profile;
pub(crate) mod exec;
mod managed_attach_socket;
pub(crate) mod nix_overlay;
mod profile;
mod pty_raw_passthrough;
mod pulse_bridge;
pub(crate) mod rootfs;
pub(crate) mod supervisor;
pub(crate) mod task_control;
mod terminal_env;
mod waypipe_broker;

use crate::runtime::RuntimeProfileScope;
use crate::runtime::launch::config::{
    self, DEFAULT_PULSE_BRIDGE_PORT, DEFAULT_WAYPIPE_PORT, ExecConfig, LaunchConfig, LaunchSpec,
    ManagedSessionConfig, PulseBridgeConfig, WaypipeConfig,
};
use crate::runtime::launch::{HostPersistentDiskPreparer, LaunchPlan, PersistentDiskPreparer};
use attach::AttachInputPolicy;
use cang_attach_protocol::{
    DEFAULT_ATTACH_PORT, PROTOCOL_VERSION,
    terminal_trace::{
        prepare_terminal_trace_file_from_process_env, terminal_trace_env_pair_from_process_env,
    },
};
use cang_exec_protocol::{DEFAULT_EXEC_PORT, PROTOCOL_VERSION as EXEC_PROTOCOL_VERSION};
use profile::CangHostProfiler;
use rootfs::task::{HostBtrfsRootfsCommands, TaskRootfsLease, TaskRootfsManager};
use supervisor::{HostSupervisor, Supervisor};

pub(crate) fn run_image_command(command: ImageCacheCommand) -> Result<String> {
    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory for cang image cache command")?;
    let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home_dir = env::var_os("HOME").map(PathBuf::from);
    let config =
        crate::config::state::read_config(xdg_config_home.as_deref(), home_dir.as_deref())?;
    let state_layout = crate::state::resolve_state_layout_from_parts(
        &cwd,
        xdg_state_home.as_deref(),
        home_dir.as_deref(),
        config.state_location_override(),
    )?;
    let output = rootfs::image_source::run_image_cache_command(
        command,
        &state_layout.image_cache_dir(),
        &rootfs::image_source::HostBuildahCommands,
        &HostBtrfsRootfsCommands,
    )?;
    Ok(output.render_stdout())
}

pub(crate) fn run_task_control_command(command: TaskControlCommand) -> Result<String> {
    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory for cang task control command")?;
    let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home_dir = env::var_os("HOME").map(PathBuf::from);
    let config =
        crate::config::state::read_config(xdg_config_home.as_deref(), home_dir.as_deref())?;
    let state_layout = crate::state::resolve_state_layout_from_parts(
        &cwd,
        xdg_state_home.as_deref(),
        home_dir.as_deref(),
        config.state_location_override(),
    )?;
    task_control::run_task_control_command(command, state_layout.app_dir())
}

pub(crate) fn run_exec_command(
    task_id: String,
    command: Vec<String>,
    waypipe: Option<Option<PathBuf>>,
) -> Result<ExitCode> {
    let cwd = env::current_dir()?
        .canonicalize()
        .context("failed to canonicalize current directory for cang exec command")?;
    let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home_dir = env::var_os("HOME").map(PathBuf::from);
    let config =
        crate::config::state::read_config(xdg_config_home.as_deref(), home_dir.as_deref())?;
    let state_layout = crate::state::resolve_state_layout_from_parts(
        &cwd,
        xdg_state_home.as_deref(),
        home_dir.as_deref(),
        config.state_location_override(),
    )?;
    exec::exec_in_task_with_procfs(
        state_layout.app_dir(),
        &task_id,
        command,
        waypipe.as_ref().map(|target| target.as_deref()),
    )
}

pub(crate) fn run(options: RuntimeOptions, profile_scope: RuntimeProfileScope) -> Result<ExitCode> {
    let mut profiler = CangHostProfiler::new_started_at(
        host_profile_enabled(&options),
        profile_scope.started_at(),
    );
    let pty = options.pty;
    let attach_input_policy =
        AttachInputPolicy::new(pty.suppress_focus_input, pty.focus_report_guard);
    let cwd = profiler.measure_result("workspace_canonicalization", || {
        resolve_workspace(options.workspace.as_deref())
    })?;
    prepare_terminal_trace_file_from_process_env(pty.trace, &cwd)
        .context("failed to prepare cang terminal trace file")?;
    let plan =
        profiler.measure_result("launch_plan_build", || LaunchPlan::from_env(options, cwd))?;
    profiler.record_metadata(
        "task_rootfs_backend",
        plan.task_rootfs_backend.as_config_value(),
    );
    profiler.record_metadata("image", plan.image_selection.selected_reference());

    tracing::debug!(
        image = plan.image_selection.selected_reference(),
        network_mode = plan.network_mode.as_config_value(),
        rootfs_backend = %plan.task_rootfs_backend,
        state_root = %plan.state_layout.root_dir().display(),
        image_cache = %plan.image_cache_dir.display(),
        sccache = %plan.sccache_dir.display(),
        mounts = plan.bind_mounts.len(),
        workspace_slug = %plan.workspace_slug,
        config = %plan.config_diagnostics.config_path.display(),
        loaded = plan.config_diagnostics.config_loaded,
        "cang launch plan"
    );

    match plan.task_rootfs_backend {
        TaskRootfsBackend::BtrfsSnapshot => {
            let task_id = rootfs::task::new_task_id(&plan.workspace_slug);
            let manager = TaskRootfsManager::new(
                plan.state_layout.root_dir().to_path_buf(),
                plan.image_cache_dir.clone(),
            );
            let handle =
                profiler.measure_result_with("task_rootfs_materialization", |profiler| {
                    let mut rootfs_profile = profiler.rootfs_materialization_recorder();
                    manager.materialize_btrfs_from_buildah_profiled(
                        &plan.image_selection,
                        &task_id,
                        plan.preserve_debug,
                        &rootfs::image_source::HostBuildahCommands,
                        &HostBtrfsRootfsCommands,
                        &mut rootfs_profile,
                    )
                })?;
            let lease = TaskRootfsLease::new(handle, HostBtrfsRootfsCommands);
            let nix_overlay_lease = profiler.measure_result("nix_overlay_lease", || {
                nix_overlay::NixOverlayLease::acquire(plan.state_layout.root_dir(), lease.handle())
            })?;
            profiler.record_metadata(
                "nix_overlay_lowerdir",
                nix_overlay_lease.intent().lowerdir.display().to_string(),
            );
            profiler.record_metadata(
                "nix_overlay_merged",
                nix_overlay_lease.intent().mergeddir.display().to_string(),
            );
            if let Some(digest) = lease.handle().image_digest() {
                profiler.record_metadata("image_digest", digest);
            }
            let cache_profile = lease.handle().cache_profile();
            profiler.record_metadata(
                "task_rootfs_cache_status",
                cache_profile.status.as_profile_value(),
            );
            if let Some(digest_key) = &cache_profile.digest_key {
                profiler.record_metadata("task_rootfs_cache_digest_key", digest_key);
            }
            if let Some(cache_path) = &cache_profile.cache_path {
                profiler
                    .record_metadata("task_rootfs_cache_path", cache_path.display().to_string());
            }
            if let Some(reason) = &cache_profile.uncached_reason {
                profiler.record_metadata("task_rootfs_cache_uncached_reason", reason);
            }

            {
                let handle = lease.handle();
                let digest = handle.image_digest().unwrap_or("<unknown>");
                tracing::debug!(
                    task_id = handle.task_id(),
                    backend = %handle.backend(),
                    image = handle.selected_image_reference(),
                    digest,
                    rootfs = %handle.rootfs_path().display(),
                    task_dir = %handle.task_dir().display(),
                    "cang task rootfs"
                );
            }

            let host_run_result = match profiler
                .measure_result("persistent_disk_preparation", || {
                    HostPersistentDiskPreparer.prepare(plan.state_layout.root_dir())
                })
                .context("failed to prepare cang persistent dev cache disks")
            {
                Ok(disks) => match profiler.measure_result("guest_init_resolution", || {
                    rootfs::guest_init::resolve_guest_init_with_entrypoint(
                        lease.handle().rootfs_path(),
                        plan.guest_init.as_deref(),
                        lease.handle().process_config().entrypoint.as_slice(),
                    )
                }) {
                    Ok(guest_init) => {
                        let attach_socket = managed_attach_socket::allocate(
                            lease.handle().task_id(),
                            lease.handle().task_dir(),
                        )
                        .context("failed to allocate cang managed attach socket")?;
                        let exec_socket = managed_attach_socket::allocate_exec(
                            lease.handle().task_id(),
                            lease.handle().task_dir(),
                        )
                        .context("failed to allocate cang exec socket")?;
                        let pulse_bridge = if let Some(host_port) =
                            plan.pulse.and_then(|pulse| pulse.host_loopback_port())
                        {
                            Some(PulseBridgeConfig {
                                socket: managed_attach_socket::allocate_pulse_bridge(
                                    lease.handle().task_id(),
                                    lease.handle().task_dir(),
                                )
                                .context("failed to allocate cang Pulse bridge socket")?,
                                guest_port: DEFAULT_PULSE_BRIDGE_PORT,
                                host_port,
                            })
                        } else {
                            None
                        };
                        let waypipe = if plan.waypipe {
                            Some((
                                managed_attach_socket::allocate_waypipe_data(
                                    lease.handle().task_id(),
                                    lease.handle().task_dir(),
                                )
                                .context("failed to allocate cang Waypipe data socket")?,
                                managed_attach_socket::allocate_waypipe_control(
                                    lease.handle().task_id(),
                                    lease.handle().task_dir(),
                                )
                                .context("failed to allocate cang Waypipe control socket")?,
                            ))
                        } else {
                            None
                        };
                        let exec = ExecConfig {
                            socket: exec_socket,
                            guest_port: DEFAULT_EXEC_PORT,
                            protocol_version: EXEC_PROTOCOL_VERSION,
                            socket_uid: current_uid(),
                            socket_gid: current_gid(),
                        };
                        let managed_session = ManagedSessionConfig {
                            attach_socket,
                            guest_port: DEFAULT_ATTACH_PORT,
                            protocol_version: PROTOCOL_VERSION,
                            attach_socket_uid: current_uid(),
                            attach_socket_gid: current_gid(),
                            cleanup_task_rootfs_on_exit: !plan.preserve_debug,
                            guest_kernel_console_log: lease.handle().task_dir().join(
                                crate::runtime::launch::config::MANAGED_GUEST_KERNEL_CONSOLE_LOG,
                            ),
                        };
                        match profiler.measure_result("launch_config_build", || {
                            LaunchConfig::build_for_task(LaunchSpec {
                                task_rootfs: lease.handle().rootfs_path(),
                                hostname: &plan.hostname,
                                mounts: &plan.bind_mounts,
                                guest_init_override: guest_init.override_mount.clone(),
                                guest_init_exec: &guest_init.guest_exec_path,
                                guest_command: &plan.guest_command,
                                image_process_config: lease.handle().process_config(),
                                mem_gib: plan.mem_gib,
                                log_level: plan.log_level,
                                network_mode: plan.network_mode,
                                pulse: plan.pulse,
                                gpu_mode: plan.gpu_mode,
                                zero_copy_shm: plan.zero_copy_shm,
                                wayland: plan.wayland,
                                new_perms: plan.new_perms,
                                publish: &plan.publish,
                                profile: plan.profile,
                                root: plan.root,
                                allocator: plan.allocator,
                                host_uid: current_uid(),
                                host_gid: current_gid(),
                                vcpus: config::resolve_cpu_count()?,
                                disks: disks.attachments(),
                                extra_env: {
                                    let mut env = disks.env_pairs();
                                    env.extend(terminal_env::host_terminal_env_pairs());
                                    if let Some(pair) =
                                        attach_profile::guest_env_pair_from_process_env()
                                    {
                                        env.push(pair);
                                    }
                                    if let Some(pair) =
                                        pty_raw_passthrough::guest_env_pair(pty.mode)
                                    {
                                        env.push(pair);
                                    }
                                    if let Some(pair) =
                                        terminal_trace_env_pair_from_process_env(pty.trace)
                                    {
                                        env.push(pair);
                                    }
                                    env
                                },
                                host_nix_overlay: Some(nix_overlay_lease.intent().clone()),
                                pulse_bridge,
                                waypipe: waypipe.as_ref().map(|(socket, _)| WaypipeConfig {
                                    socket: socket.clone(),
                                    guest_port: DEFAULT_WAYPIPE_PORT,
                                }),
                                exec: Some(exec.clone()),
                                managed_session: Some(managed_session.clone()),
                            })
                        }) {
                            Ok(mut config) => {
                                config.seccomp = plan.seccomp.clone();
                                config.landlock = plan.landlock;
                                tracing::debug!(
                                    guest_init = %guest_init.guest_exec_path,
                                    disks = config.disks.len(),
                                    ram_mib = config.ram_mib,
                                    vcpus = config.vcpus,
                                    network_mode = config.network_mode.as_config_value(),
                                    gpu_mode = config.gpu_mode.as_config_value(),
                                    workspace = %plan.workspace_dir.display(),
                                    mounts = config.mounts.len(),
                                    "cang libkrun launch"
                                );

                                let active_task = task_control::ActiveTaskSpec {
                                    task_id: lease.handle().task_id().to_owned(),
                                    workspace_slug: plan.workspace_slug.clone(),
                                    workspace_dir: plan.workspace_dir.clone(),
                                    task_dir: lease.handle().task_dir().to_path_buf(),
                                    image_reference: lease
                                        .handle()
                                        .selected_image_reference()
                                        .to_owned(),
                                    image_digest: lease.handle().image_digest().map(str::to_owned),
                                    waypipe: waypipe.map(|(_, control_socket)| {
                                        task_control::WaypipeTaskSpec {
                                            control_socket,
                                            initial_target: plan.waypipe_socket.clone(),
                                        }
                                    }),
                                    exec: Some(task_control::ExecTaskSpec {
                                        socket: exec.socket,
                                        guest_port: exec.guest_port,
                                        protocol_version: exec.protocol_version,
                                    }),
                                    managed: Some(task_control::ManagedTaskSpec {
                                        attach_socket: managed_session.attach_socket.clone(),
                                        guest_port: managed_session.guest_port,
                                        protocol_version: managed_session.protocol_version,
                                    }),
                                };
                                BtrfsHostRunResult::Helper {
                                    managed: config.is_managed_session(),
                                    result: profiler.measure_result_with(
                                        "helper_session",
                                        |profiler| {
                                            HostSupervisor.run(
                                                &config,
                                                lease.handle().task_dir(),
                                                profiler,
                                                &active_task,
                                                plan.daemon,
                                                attach_input_policy,
                                            )
                                        },
                                    ),
                                }
                            }
                            Err(err) => BtrfsHostRunResult::SetupFailed(err),
                        }
                    }
                    Err(err) => BtrfsHostRunResult::SetupFailed(err),
                },
                Err(err) => BtrfsHostRunResult::SetupFailed(err),
            };
            let managed_helper_succeeded = matches!(
                &host_run_result,
                BtrfsHostRunResult::Helper {
                    managed: true,
                    result: Ok(_),
                }
            );
            let cleanup_result = if managed_helper_succeeded {
                profiler.measure_result("task_state_cleanup", || Ok(lease.preserve()))
            } else if plan.preserve_debug {
                let hint = lease.handle().preserve_debug_hint();
                profiler.measure_result("task_state_cleanup", || {
                    task_control::remove_active_task(lease.handle().task_dir())?;
                    let result = lease.preserve();
                    if let rootfs::task::CleanupResult::Preserved(path) = &result {
                        eprintln!(
                            "cang: preserving task rootfs '{}' because --preserve-debug was set; {hint}",
                            path.display()
                        );
                    }
                    Ok(result)
                })
            } else {
                profiler.measure_result("task_state_cleanup", || {
                    task_control::remove_active_task(lease.handle().task_dir())?;
                    lease.cleanup()
                })
            };
            finalize_profiled_btrfs_run(host_run_result, cleanup_result, || {
                profiler.emit_to_stderr();
            })
        }
        TaskRootfsBackend::FuseOverlay => bail!(
            "cang fuse-overlay task rootfs materialization is not implemented in this phase; use btrfs-snapshot or wait for the fuse-overlay slice"
        ),
    }
}

enum BtrfsHostRunResult {
    Helper {
        managed: bool,
        result: Result<supervisor::ChildStatus>,
    },
    SetupFailed(anyhow::Error),
}

fn finalize_profiled_btrfs_run(
    host_run_result: BtrfsHostRunResult,
    cleanup_result: Result<rootfs::task::CleanupResult>,
    emit_profile: impl FnOnce(),
) -> Result<ExitCode> {
    emit_profile();
    match host_run_result {
        BtrfsHostRunResult::Helper { result, .. } => {
            finalize_btrfs_run_result(result, cleanup_result)
        }
        BtrfsHostRunResult::SetupFailed(setup_error) => {
            finalize_post_lease_setup_failure(setup_error, cleanup_result)
        }
    }
}

fn resolve_workspace(workspace: Option<&std::path::Path>) -> Result<PathBuf> {
    let workspace = workspace.map_or_else(env::current_dir, |path| Ok(path.to_path_buf()))?;
    let workspace = workspace
        .canonicalize()
        .context("failed to canonicalize cang workspace mount")?;
    if !workspace.is_dir() {
        bail!("cang workspace is not a directory: {}", workspace.display());
    }
    Ok(workspace)
}

fn host_profile_enabled(options: &RuntimeOptions) -> bool {
    options.profile
}

fn finalize_post_lease_setup_failure(
    setup_error: anyhow::Error,
    cleanup_result: Result<rootfs::task::CleanupResult>,
) -> Result<ExitCode> {
    if let Err(cleanup_error) = cleanup_result {
        eprintln!(
            "cang: best-effort task rootfs cleanup failed after post-materialization error: {cleanup_error:#}"
        );
    }
    Err(setup_error)
}

fn finalize_btrfs_run_result(
    run_result: Result<supervisor::ChildStatus>,
    cleanup_result: Result<rootfs::task::CleanupResult>,
) -> Result<ExitCode> {
    match (run_result, cleanup_result) {
        (Ok(status), Ok(_)) => Ok(status.exit_code()),
        (Ok(status), Err(cleanup_error)) => {
            Err(context_cleanup_after_child_status(cleanup_error, status))
        }
        (Err(run_error), Ok(_)) => Err(run_error),
        (Err(run_error), Err(cleanup_error)) => Err(cleanup_error.context(format!(
            "failed to clean cang task rootfs after libkrun helper error: {run_error:#}"
        ))),
    }
}

fn context_cleanup_after_child_status(
    cleanup_error: anyhow::Error,
    status: supervisor::ChildStatus,
) -> anyhow::Error {
    match status {
        supervisor::ChildStatus::Exited(code) => cleanup_error.context(format!(
            "failed to clean cang task rootfs after guest exited with status {code}"
        )),
        supervisor::ChildStatus::Signaled => {
            cleanup_error.context("failed to clean cang task rootfs after helper was signaled")
        }
        supervisor::ChildStatus::Detached => cleanup_error,
    }
}

pub(crate) fn run_internal(args: Vec<OsString>) -> Result<()> {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(supervisor::is_supervisor_internal_arg)
    {
        return supervisor::run_internal(args);
    }
    rootfs::image_source::run_internal(args)
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::cell::Cell;
    use std::path::PathBuf;

    fn runtime_options(debug: bool, profile: bool) -> RuntimeOptions {
        RuntimeOptions {
            image: None,
            pull_latest: false,
            debug,
            log_settings: crate::logging::LogSettings::resolve(None, debug, None),
            profile,
            root: false,
            daemon: false,
            pty: crate::cli::PtyOptions::DEFAULT,
            seccomp: Some(crate::runtime::seccomp::SeccompMode::Off),
            landlock: None,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            allocator: crate::runtime::launch::config::AllocatorMode::Mimalloc,
            rootfs_backend: None,
            container_store_backend: None,
            guest_init: None,
            preserve_debug: false,
            mem_gib: None,
            network_mode: config::NetworkMode::Tsi,
            pulse: None,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            zero_copy_shm: false,
            wayland: false,
            workspace: None,
            waypipe: None,
            publish: Vec::new(),
            volumes: Vec::new(),
            guest_command: Vec::new(),
        }
    }

    #[test]
    fn resolve_workspace_accepts_directory_and_rejects_file() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace should exist");

        assert_eq!(
            resolve_workspace(Some(&workspace)).expect("directory should resolve"),
            workspace
        );

        let file = dir.path().join("file");
        std::fs::write(&file, "not a directory").expect("file should exist");
        let err = resolve_workspace(Some(&file)).expect_err("file should fail");

        assert!(format!("{err:#}").contains("cang workspace is not a directory"));
    }

    #[test]
    fn host_profile_is_enabled_when_profile_is_requested() {
        assert!(!host_profile_enabled(&runtime_options(false, false)));
        assert!(!host_profile_enabled(&runtime_options(true, false)));
        assert!(host_profile_enabled(&runtime_options(false, true)));
        assert!(host_profile_enabled(&runtime_options(true, true)));
    }

    #[test]
    fn attach_profile_guest_env_is_host_opt_in_only() {
        assert_eq!(attach_profile::guest_env_pair_from_value(None), None);
        assert_eq!(attach_profile::guest_env_pair_from_value(Some("0")), None);
        assert_eq!(
            attach_profile::guest_env_pair_from_value(Some("1")),
            Some((
                attach_profile::ATTACH_PROFILE_ENV.to_owned(),
                "1".to_owned()
            ))
        );
    }

    #[test]
    fn profiled_btrfs_finalization_emits_report_before_returning_helper_result() {
        let emitted = Cell::new(false);
        let code = finalize_profiled_btrfs_run(
            BtrfsHostRunResult::Helper {
                managed: false,
                result: Ok(supervisor::ChildStatus::exited(3)),
            },
            Ok(rootfs::task::CleanupResult::Removed),
            || emitted.set(true),
        )
        .expect("successful helper and cleanup should return helper exit code");

        assert!(
            emitted.get(),
            "host profile should be emitted on completed btrfs path"
        );
        assert_eq!(code, ExitCode::from(3));
    }

    #[test]
    fn profiled_btrfs_finalization_emits_report_before_returning_setup_error() {
        let emitted = Cell::new(false);
        let err = finalize_profiled_btrfs_run(
            BtrfsHostRunResult::SetupFailed(anyhow!("guest-init failure")),
            Ok(rootfs::task::CleanupResult::Removed),
            || emitted.set(true),
        )
        .expect_err("setup failure should still be returned");

        assert!(
            emitted.get(),
            "host profile should be emitted before setup errors return"
        );
        assert_eq!(err.to_string(), "guest-init failure");
    }

    #[test]
    fn btrfs_finalization_returns_helper_exit_code_after_cleanup_success() {
        let code = finalize_btrfs_run_result(
            Ok(supervisor::ChildStatus::exited(42)),
            Ok(rootfs::task::CleanupResult::Removed),
        )
        .expect("successful helper and cleanup should return helper exit code");

        assert_eq!(code, ExitCode::from(42));
    }

    #[test]
    fn btrfs_finalization_returns_helper_exit_code_after_preserve_success() {
        let code = finalize_btrfs_run_result(
            Ok(supervisor::ChildStatus::exited(7)),
            Ok(rootfs::task::CleanupResult::Preserved(PathBuf::from(
                "/tmp/cang-task/rootfs",
            ))),
        )
        .expect("successful helper and preserve should return helper exit code");

        assert_eq!(code, ExitCode::from(7));
    }

    #[test]
    fn post_lease_setup_failure_is_preserved_after_cleanup_success() {
        let err = finalize_post_lease_setup_failure(
            anyhow!("persistent disk failure"),
            Ok(rootfs::task::CleanupResult::Removed),
        )
        .expect_err("setup failure should be preserved when cleanup succeeds");

        assert_eq!(err.to_string(), "persistent disk failure");
    }

    #[test]
    fn post_lease_setup_failure_is_preserved_after_cleanup_failure() {
        let err = finalize_post_lease_setup_failure(
            anyhow!("guest-init failure"),
            Err(anyhow!("cleanup failure")),
        )
        .expect_err("setup failure should still be preserved when best-effort cleanup fails");

        assert_eq!(err.to_string(), "guest-init failure");
    }

    #[test]
    fn btrfs_finalization_cleanup_error_dominates_successful_helper() {
        let err = finalize_btrfs_run_result(
            Ok(supervisor::ChildStatus::exited(127)),
            Err(anyhow!("cleanup failure")),
        )
        .expect_err("cleanup failure should dominate successful helper");
        let text = format!("{err:#}");

        assert!(text.contains("cleanup failure"));
        assert!(text.contains("guest exited with status 127"));
    }

    #[test]
    fn btrfs_finalization_preserves_helper_error_after_cleanup_success() {
        let err = finalize_btrfs_run_result(
            Err(anyhow!("helper failure")),
            Ok(rootfs::task::CleanupResult::Removed),
        )
        .expect_err("helper failure should be preserved when cleanup succeeds");

        assert_eq!(err.to_string(), "helper failure");
    }

    #[test]
    fn btrfs_finalization_cleanup_error_after_helper_error_is_contextualized() {
        let err = finalize_btrfs_run_result(
            Err(anyhow!("helper failure")),
            Err(anyhow!("cleanup failure")),
        )
        .expect_err("cleanup failure should dominate helper failure with context");
        let text = format!("{err:#}");

        assert!(text.contains("cleanup failure"));
        assert!(text.contains("failed to clean cang task rootfs after libkrun helper error"));
        assert!(text.contains("helper failure"));
    }
}
