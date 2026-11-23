use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::{DiskMaintenanceOptions, RuntimeOptions};
use crate::runtime::maintenance::{disk_resize::HostMaintenanceVmRunner, workspace_grow};
use crate::runtime::session::rootfs::image_source::ImageCacheCommand;
use crate::runtime::session::task_control::TaskControlCommand;
use crate::task_rootfs::TaskRootfsBackend;

pub(crate) mod attach;
mod attach_profile;
pub(crate) mod exec;
mod launcher_command;
mod managed_attach_socket;
mod memory_reclaim;
pub(crate) mod nix_overlay;
mod profile;
mod pty_raw_passthrough;
mod pulse_bridge;
pub(crate) mod rootfs;
pub(crate) mod supervisor;
pub(crate) mod task_control;
mod terminal_env;
mod waypipe_broker;
mod workspace_relay;
pub(crate) mod workspace_server;

use crate::runtime::RuntimeProfileScope;
use crate::runtime::launch::components::persistent_disks::{
    self, raw_btrfs::HostRawImageCommandRunner, workspace,
};
use crate::runtime::launch::config::{
    self, DEFAULT_PULSE_BRIDGE_PORT, DEFAULT_WAYPIPE_PORT, ExecConfig, LaunchConfig, LaunchSpec,
    ManagedSessionConfig, PulseBridgeConfig, WaypipeConfig, WorkspaceRelayConfig,
    workspace_relay_port,
};
use crate::runtime::launch::{
    HostPersistentDiskPreparer, LaunchPlan, NixLowerDiskStatus, PersistentDiskPreparer,
    prepare_nix_lower_block,
};
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
    // Kept beside the plan because the plan consumes the options: an auto-grow
    // boots the maintenance VM the same way `cang disk ...` does, and it must boot
    // the image and guest-init this launch would boot.
    let maintenance_options = options.disk_maintenance_options();
    let plan =
        profiler.measure_result("launch_plan_build", || LaunchPlan::from_env(options, cwd))?;
    // A `--gpu=drm` launch needs its render server, its sandbox policy and a loader
    // path that can find its libraries. The render server is spawned by the helper
    // long after the CLI has done its work, so a source-built cang that lacks the
    // package wrapper's environment (or a render server whose RPATH does not cover
    // its closure) used to boot a VM that then had no usable GPU path. Refused
    // here - before the lease, the auto-grow VM, the task directory or a single
    // byte of rootfs work.
    if plan.gpu_mode == crate::runtime::vm::gpu::GpuMode::Drm {
        profiler.measure_result("render_server_preflight", || {
            crate::runtime::session::supervisor::render_server::preflight()
        })?;
    }
    // The single-writer rule on a workspace image (ticket 29): a workspace
    // server holds the image exclusively for its whole lifetime, and every
    // `--workspace-disk` launch holds it shared. Taken before the workspace task
    // gate below, so a launch that may not have the image is refused for *that*
    // reason - by name, pointing at `cang workspace stop`.
    let _workspace_lease = match plan.workspace_disk.as_deref() {
        Some(disk) if plan.workspace_server => {
            Some(profiler.measure_result("workspace_server_lease", || {
                workspace_server::WorkspaceLease::acquire_server(disk)
            })?)
        }
        Some(disk) => profiler.measure_result("workspace_image_lease", || {
            workspace_server::WorkspaceLease::acquire_task(disk)
        })?,
        None => None,
    };
    // `/workspace` grows itself - but only for a server, where it pays for itself.
    // The marker that decides is inside the image and only a guest can read it,
    // so this costs one short read-only maintenance VM. A `cang workspace serve`
    // mounts that image once and then stays up for as long as its tree is used,
    // which makes it a once-per-server cost paid before the tree has anything in
    // it; a one-shot `--workspace-disk` launch does not auto-grow, because a whole
    // extra VM boot on every launch is not what "fewer manual grows" was worth.
    // That case is left to `cang disk resize --workspace-disk`, which the refusal
    // line names, and `docs/usage.md` says so plainly.
    if plan.workspace_server
        && let Some(disk) = plan.workspace_disk.as_deref()
    {
        auto_grow_workspace_image(&mut profiler, disk, &maintenance_options);
    }
    // The server's shutdown handlers exist before any launch work happens: a
    // stop request that arrives while the rootfs is still being materialized
    // must be an ordered teardown, not the default action killing cang and
    // leaving the VM behind.
    let workspace_server_signals = plan
        .workspace_server
        .then(workspace_server::install_signals)
        .transpose()?;
    if plan.workspace_server {
        profiler.measure_result("workspace_server_mountpoint", || {
            workspace_server::refuse_occupied_mountpoint(&plan.workspace_dir)
        })?;
    }
    // What happens to the terminal when the guest is ready. A workspace server
    // attaches nothing: the attach client puts the host terminal into raw mode,
    // so `^C` would reach the guest payload as a keystroke and stop the VM
    // before its mount was unmounted - the opposite of the teardown order the
    // server guarantees.
    let initial_attach = if plan.workspace_server {
        supervisor::InitialAttach::Skip
    } else if plan.daemon {
        supervisor::InitialAttach::AttachThenDetach
    } else {
        supervisor::InitialAttach::Attach
    };
    // `cang -- <command>` is a process launcher, not an interactive session: its
    // caller's stdin is a closed pipe or /dev/null in every scripted use, and
    // reading that as a detach request dropped both the command's output and its
    // exit status while the command itself kept running to completion.
    let attach_input_policy = if plan.guest_command.is_empty() {
        attach_input_policy
    } else {
        attach_input_policy.end_session_input_on_stdin_eof()
    };
    profiler.measure_result("workspace_task_gate", || {
        task_control::ensure_workspace_has_no_running_task(
            plan.state_layout.app_dir(),
            &plan.workspace_dir,
            &task_control::ProcfsInspector,
        )
    })?;
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
                "nix_overlay_seed",
                match nix_overlay_lease.seed() {
                    nix_overlay::UpperSeedOutcome::Reused => "reused",
                    nix_overlay::UpperSeedOutcome::Fresh => "fresh",
                    nix_overlay::UpperSeedOutcome::Seeded => "seeded",
                },
            );
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

            // Block mode's guest half lives in the image's `cang-guest-init`, so
            // which binary will run is decided before the lower disk is built:
            // an image built before that half existed would get a disk it
            // ignores, and the guest's /nix would silently stay virtio-fs.
            let guest_init = profiler.measure_result("guest_init_resolution", || {
                rootfs::guest_init::resolve_guest_init_with_entrypoint(
                    lease.handle().rootfs_path(),
                    plan.guest_init.as_deref(),
                    lease.handle().process_config().entrypoint.as_slice(),
                )
            })?;
            let guest_init_binary = guest_init
                .override_mount
                .as_ref()
                .map_or(guest_init.host_path.as_path(), |mount| {
                    mount.source.as_path()
                });

            // `/workspace` as a btrfs tree of its own: created at the requested
            // size on first use, validated on reuse. Everything the guest does
            // then stays on this disk instead of crossing the share.
            let workspace_disk = profiler
                .measure_result("workspace_disk_preparation", || {
                    plan.workspace_disk.as_ref().map_or(Ok(None), |path| {
                        workspace::prepare_with_runner(
                            path,
                            workspace::WorkspaceRequest {
                                size_bytes: plan.workspace_disk_size_bytes,
                                format: plan.workspace_disk_format,
                            },
                            &HostRawImageCommandRunner,
                        )
                        .map(Some)
                    })
                })
                .context("failed to prepare the cang workspace disk")?;
            // A block device is the host's, not cang's: say so once, because every
            // other disk cang attaches is one it created, formatted and grows.
            if let Some(disk) = workspace_disk.as_ref().filter(|disk| disk.is_device()) {
                eprintln!(
                    "cang: serving the host block device '{}' ({}); cang will not format it \
                     or grow it, and it must not be mounted on the host while the guest has it",
                    disk.path.display(),
                    disk.size_bytes
                );
            }
            match &workspace_disk {
                Some(disk) => profiler.record_metadata(
                    "workspace_disk_status",
                    match disk.status {
                        workspace::WorkspaceDiskStatus::Created => "created",
                        workspace::WorkspaceDiskStatus::Reused => "reused",
                    },
                ),
                None => profiler.record_metadata("workspace_disk_status", "disabled"),
            }
            if let Some(disk) = &workspace_disk {
                profiler.record_metadata("workspace_disk_path", disk.path.display().to_string());
            }

            // The guest's /nix lower on a read-only btrfs block file, built from
            // this image's store content. `None` means the launch asked for the
            // host-composed overlay; an unavailable lower is an error unless the
            // user allowed the fallback, so a slow `/nix` is never silent.
            // `/nix` follows the workspace disk: no flag decides this, the plan does.
            let nix_lower_block = profiler.measure_result("nix_lower_block_preparation", || {
                prepare_nix_lower_block(
                    nix_overlay_lease.intent(),
                    guest_init_binary,
                    plan.nix_block_lower,
                )
            })?;
            match &nix_lower_block {
                Some(block) => {
                    profiler.record_metadata(
                        "nix_lower_block_status",
                        match block.disk().status {
                            NixLowerDiskStatus::Built => "built",
                            NixLowerDiskStatus::Reused => "reused",
                        },
                    );
                    profiler.record_metadata(
                        "nix_lower_block_image",
                        block.disk().path.display().to_string(),
                    );
                    profiler
                        .record_metadata("nix_lower_block_identity", block.disk().identity.clone());
                }
                None => profiler.record_metadata("nix_lower_block_status", "disabled"),
            }

            // A workspace server's own bookkeeping: the state file `cang
            // workspace stop`/`status` read (removed when this scope ends) and
            // the monitor's report, which decides the exit code below.
            let mut workspace_server_session: Option<workspace_server::WorkspaceServerSession> =
                None;
            let mut workspace_server_report: Option<workspace_server::ShutdownReport> = None;
            // Ticket 27: `cang-build.raw` is retired. This launch reclaims the
            // one a previous cang left in this workspace, and says what it held.
            // Deliberately not part of the launch's own success: a busy image is
            // `Ok(None)` (a guest is still using it, so it stays), and a real
            // removal error is reported on one line and the launch continues -
            // a file nothing needs any more must not fail a launch that has
            // nothing to do with it.
            match profiler.measure_result("retired_build_disk_reclaim", || {
                persistent_disks::reclaim_retired_build_disk(plan.state_layout.root_dir())
            }) {
                Ok(Some(reclaimed)) => {
                    eprintln!(
                        "{}",
                        persistent_disks::retired_build_disk::report(&reclaimed)
                    );
                }
                Ok(None) => {}
                Err(err) => eprintln!(
                    "cang: could not reclaim a retired build disk; leaving it alone: {err:#}"
                ),
            }
            let host_run_result = match profiler
                .measure_result("persistent_disk_preparation", || {
                    HostPersistentDiskPreparer.prepare(plan.state_layout.root_dir())
                })
                .context("failed to prepare cang persistent dev cache disks")
            {
                Ok(disks) => {
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
                    // `--workspace-relay`: the socket libkrun binds for the
                    // guest's relay listener, and the guest port it carries -
                    // the first free one in the launch's own port registry.
                    let workspace_relay = plan
                        .workspace_relay
                        .then(|| {
                            managed_attach_socket::allocate_workspace_relay(
                                lease.handle().task_id(),
                                lease.handle().task_dir(),
                            )
                        })
                        .transpose()
                        .context("failed to allocate cang workspace relay socket")?
                        .map(|socket| WorkspaceRelayConfig {
                            socket,
                            guest_port: workspace_relay_port(&[
                                DEFAULT_ATTACH_PORT,
                                DEFAULT_WAYPIPE_PORT,
                                DEFAULT_EXEC_PORT,
                                DEFAULT_PULSE_BRIDGE_PORT,
                            ]),
                        });
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
                        guest_kernel_console_log: lease
                            .handle()
                            .task_dir()
                            .join(crate::runtime::launch::config::MANAGED_GUEST_KERNEL_CONSOLE_LOG),
                    };
                    // A block-mode launch adds the read-only lower disk; the writable
                    // layer is a directory inside the workspace tree, which the guest
                    // creates after it mounts that disk, so the host contributes no
                    // share for it.
                    let launch_mounts = plan.bind_mounts.clone();
                    let mut launch_disks = disks.attachments();
                    let mut extra_env = disks.env_pairs();
                    if let Some(disk) = &workspace_disk {
                        launch_disks.push(workspace::attachment(disk));
                        extra_env.extend(workspace::env_pairs(disk));
                    }
                    if let Some(block) = &nix_lower_block {
                        launch_disks.push(block.attachment());
                        extra_env.extend(block.env_pairs());
                    }
                    // `/nix` gets exactly one mount. When the guest composes the
                    // overlay from the block pair, the host grafts nothing there:
                    // its own overlay would sit underneath as a covered,
                    // virtio-fs-backed `/nix` that no access can enter, and a
                    // guest whose overlay failed would silently read it instead.
                    // Without the graft the guest's overlay *is* `/nix`, so the
                    // failure has to be loud - which the guest's own policy does
                    // off the same fact (`CANG_NIX_HOST_OVERLAY`).
                    let host_nix_overlay = (!nix_lower_block
                        .as_ref()
                        .is_some_and(|block| block.serves_guest_overlay()))
                    .then(|| nix_overlay_lease.intent().clone());
                    if host_nix_overlay.is_some() {
                        extra_env.push(("CANG_NIX_HOST_OVERLAY".to_owned(), "1".to_owned()));
                    }
                    profiler.record_metadata(
                        "nix_host_overlay",
                        if host_nix_overlay.is_some() {
                            "grafted-at-/nix"
                        } else {
                            "guest-composed"
                        },
                    );
                    match profiler.measure_result("launch_config_build", || {
                        LaunchConfig::build_for_task(LaunchSpec {
                            task_rootfs: lease.handle().rootfs_path(),
                            hostname: &plan.hostname,
                            mounts: &launch_mounts,
                            guest_init_override: guest_init.override_mount.clone(),
                            guest_init_exec: &guest_init.guest_exec_path,
                            guest_command: &plan.guest_command,
                            image_process_config: lease.handle().process_config(),
                            mem_gib: plan.mem_gib,
                            log_level: plan.log_level,
                            network_mode: plan.network_mode,
                            pulse: plan.pulse,
                            gpu_mode: plan.gpu_mode,
                            gpu_shm_bytes: plan.gpu_shm_bytes,
                            zero_copy_shm: plan.zero_copy_shm,
                            zero_message_open: plan.zero_message_open,
                            no_flush: plan.no_flush,
                            cache_symlinks: None,
                            negative_entry_millis: plan.negative_entry_millis,
                            wayland: plan.wayland,
                            new_perms: plan.new_perms,
                            publish: &plan.publish,
                            profile: plan.profile,
                            root: plan.root,
                            allocator: plan.allocator,
                            host_uid: current_uid(),
                            host_gid: current_gid(),
                            vcpus: config::resolve_cpu_count()?,
                            disks: launch_disks,
                            extra_env: {
                                let mut env = extra_env;
                                env.extend(terminal_env::host_terminal_env_pairs());
                                if let Some(pair) =
                                    attach_profile::guest_env_pair_from_process_env()
                                {
                                    env.push(pair);
                                }
                                // The guest relay is what watches the served
                                // tree, so the host's decision has to reach it
                                // through the guest environment.
                                if let Some(pair) =
                                    workspace_relay::watch_guest_env_pair_from_process_env()
                                {
                                    env.push(pair);
                                }
                                if let Some(pair) = pty_raw_passthrough::guest_env_pair(pty.mode) {
                                    env.push(pair);
                                }
                                if let Some(pair) = launcher_command::guest_env_pair(
                                    &plan.guest_command,
                                    std::io::stdin().is_terminal(),
                                ) {
                                    env.push(pair);
                                }
                                if let Some(pair) =
                                    terminal_trace_env_pair_from_process_env(pty.trace)
                                {
                                    env.push(pair);
                                }
                                env
                            },
                            host_nix_overlay,
                            pulse_bridge,
                            waypipe: waypipe.as_ref().map(|(socket, _)| WaypipeConfig {
                                socket: socket.clone(),
                                guest_port: DEFAULT_WAYPIPE_PORT,
                            }),
                            exec: Some(exec.clone()),
                            managed_session: Some(managed_session.clone()),
                            workspace_relay: workspace_relay.clone(),
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
                            // Announced before the VM starts, so a `cang
                            // workspace stop` that arrives during the launch can
                            // find (and stop) the server instead of racing it.
                            if let (Some(disk), Some(signals)) = (
                                plan.workspace_disk.as_ref(),
                                workspace_server_signals.as_ref(),
                            ) {
                                workspace_server_session =
                                    Some(workspace_server::WorkspaceServerSession::start(
                                        disk,
                                        &plan.workspace_dir,
                                        lease.handle().task_id(),
                                        lease.handle().task_dir(),
                                        std::sync::Arc::clone(signals),
                                    )?);
                            }
                            // The host side of the reverse share: the pump owns
                            // the mount for exactly the life of this launch, so
                            // its teardown is ordered before the task dir (which
                            // holds the pump's own state file) is cleaned up.
                            let workspace_relay_guard = match &workspace_relay {
                                Some(relay) => Some(workspace_relay::WorkspaceRelayGuard::start(
                                    workspace_relay::pump_program(),
                                    lease.handle().task_dir(),
                                    &plan.workspace_dir,
                                    &relay.socket,
                                    plan.profile,
                                )?),
                                None => None,
                            };
                            // A workspace server hands the mount to a monitor
                            // thread, which is what enforces the order the ticket
                            // requires: the mount is gone before the VM stops, and
                            // a mount that never appears stops the server with a
                            // named reason instead of leaving an unusable VM.
                            let mut server_monitor = None;
                            let mut launch_relay_guard = None;
                            if let Some(guard) = workspace_relay_guard {
                                match workspace_server_session.as_ref() {
                                    Some(session) => {
                                        server_monitor = Some(session.monitor(
                                            guard,
                                            lease.handle().task_dir(),
                                            lease.handle().task_id(),
                                        )?);
                                    }
                                    None => launch_relay_guard = Some(guard),
                                }
                            }
                            let helper_result =
                                profiler.measure_result_with("helper_session", |profiler| {
                                    HostSupervisor.run(
                                        &config,
                                        lease.handle().task_dir(),
                                        profiler,
                                        &active_task,
                                        initial_attach,
                                        attach_input_policy,
                                    )
                                });
                            let relay_outcome = launch_relay_guard
                                .as_ref()
                                .map(workspace_relay::WorkspaceRelayGuard::outcome)
                                .or_else(|| {
                                    server_monitor
                                        .as_ref()
                                        .map(workspace_server::WorkspaceServerMonitor::outcome)
                                });
                            drop(launch_relay_guard);
                            if let Some(monitor) = server_monitor {
                                let report = monitor.finish();
                                profiler.record_metadata(
                                    "workspace_server",
                                    match &report {
                                        Some(report) if report.failure().is_some() => {
                                            "failed-teardown"
                                        }
                                        Some(_) => "stopped",
                                        None => "vm-ended",
                                    },
                                );
                                workspace_server_report = report;
                            }
                            profiler.record_metadata(
                                "workspace_relay",
                                relay_outcome
                                    .as_ref()
                                    .map_or("disabled", |outcome| outcome.as_profile_value()),
                            );
                            if let Some(relay) = &workspace_relay {
                                tracing::debug!(
                                    guest_port = relay.guest_port,
                                    socket = %relay.socket.display(),
                                    "cang workspace relay"
                                );
                            }

                            BtrfsHostRunResult::Helper {
                                managed: config.is_managed_session(),
                                result: helper_result,
                            }
                        }
                        Err(err) => BtrfsHostRunResult::SetupFailed(err),
                    }
                }
                Err(err) => BtrfsHostRunResult::SetupFailed(err),
            };
            // A workspace server's helper is stopped with a signal rather than
            // exiting with its guest, so the helper's own cleanup does not run:
            // the launcher removes the active-task record and the task rootfs
            // itself. Without this every server stop left a stale `cang ps` row
            // and a rootfs snapshot behind.
            let managed_helper_succeeded = !plan.workspace_server
                && matches!(
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
            let exit_code = finalize_profiled_btrfs_run(host_run_result, cleanup_result, || {
                profiler.emit_to_stderr();
            })?;
            // A workspace server that was *asked* to stop succeeded even though
            // its VM was signalled; anything the teardown could not finish (a
            // mount that stayed busy, a relay that never mounted) decides the
            // exit code, by name.
            match workspace_server_report {
                Some(report) => match report.failure() {
                    Some(failure) => Err(anyhow::anyhow!("{failure}")),
                    None => Ok(ExitCode::SUCCESS),
                },
                None => Ok(exit_code),
            }
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

/// Auto-grow the workspace image a `cang workspace serve` is about to mount.
///
/// Called with the server's exclusive image lease already held and before its VM
/// exists, so the grow cannot race a second launch and the guest that mounts the
/// image next already sees the larger device. One line is printed when the image
/// grew or when the headroom rule refused it, and nothing at all when there is
/// nothing to do.
///
/// Best-effort by design: this is a convenience in front of the mount, not a
/// condition of it. An image that does not exist yet (a first serve creates it at
/// `--workspace-size`), a guest-init that predates the marker, or a maintenance VM
/// that failed to boot all leave the image as it is, and the serve proceeds; only
/// the last of those says so, because it is the one a user may want to know about.
fn auto_grow_workspace_image(
    profiler: &mut CangHostProfiler,
    disk: &std::path::Path,
    options: &DiskMaintenanceOptions,
) {
    // The grow policy needs the host file's length, and a first serve has no file
    // yet: creation is the launch's job, so there is nothing to decide here.
    let size_bytes = match std::fs::symlink_metadata(disk) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.len(),
        Ok(_) => return,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            eprintln!(
                "cang: could not inspect the workspace image '{}', so it was not grown automatically: {err}",
                disk.display()
            );
            return;
        }
    };
    let outcome = match profiler.measure_result("workspace_auto_grow", || {
        workspace_grow::auto_grow(disk, size_bytes, options, &HostMaintenanceVmRunner)
    }) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!(
                "cang: could not read the workspace image's free-space marker, so it was not grown automatically: {err:#}"
            );
            return;
        }
    };
    if let Some(line) = workspace_grow::grew_line(&outcome) {
        eprintln!("{line}");
    } else if let Some(line) = workspace_grow::refused_line(disk, &outcome) {
        eprintln!("{line}");
    }
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
            workspace_disk: None,
            workspace_relay: None,
            workspace_size: None,
            workspace_format: false,
            workspace_serve: false,
            mem_gib: None,
            network_mode: config::NetworkMode::Tsi,
            pulse: None,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            gpu_shm_bytes: None,
            zero_copy_shm: false,
            zero_message_open: None,
            no_flush: None,
            cache_symlinks: None,
            negative_entry_millis: None,
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
