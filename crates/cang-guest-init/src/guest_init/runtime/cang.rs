use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::path::{Path, PathBuf};

use crate::guest_init::components::env::{
    CONTAINERS_STORE_ENV, CangEnv, ContainerStoreBackend, DEFAULT_SHELL, ENTER_AS_ROOT_ENV,
    nix_remote_uri,
};

/// The host names the build disk's label and id together, so one of the two is
/// enough to know a disk was attached. The build disk is retired (nothing
/// attaches one), which is why "absent" is a statement and not a default.
const BUILD_DISK_LABEL_ENV: &str = "CANG_BUILD_DISK_LABEL";
use crate::guest_init::components::home::identity::{DevIdentity, validate_host_identity};
use crate::guest_init::runtime::exec::ExecConfig;
use crate::guest_init::runtime::session::{self, ManagedSessionConfig};
use crate::guest_init::{command, process, profile};

const HOST_UID_ENV: &str = "CANG_HOST_UID";
const HOST_GID_ENV: &str = "CANG_HOST_GID";
const SESSION_MANAGED_ENV: &str = "CANG_SESSION_MANAGED";
const ATTACH_PORT_ENV: &str = "CANG_ATTACH_PORT";
const ATTACH_PROTOCOL_VERSION_ENV: &str = "CANG_ATTACH_PROTOCOL_VERSION";
const ATTACH_PROFILE_ENV: &str = "CANG_ATTACH_PROFILE";
const WAYPIPE_PORT_ENV: &str = "CANG_WAYPIPE_PORT";
const PULSE_SERVER_ENV: &str = "CANG_PULSE_SERVER";
const PULSE_BRIDGE_PORT_ENV: &str = "CANG_PULSE_BRIDGE_PORT";
const PREPARED_ROOT_TARGETS: &[&str] = &[
    "/workspace",
    "/home/dev/.codex",
    "/home/dev/.omp",
    "/home/dev/.pi",
    "/home/dev/.zvec-grep",
    "/home/dev/.local/share/cortexkit",
    "/home/dev/.cargo",
    "/home/dev/.cache/sccache",
];

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum CangEnterOperation {
    ReadEnv,
    ValidatePreparedRootPaths,
    EnsureTmpTmpfs,
    StartGuestSwap,
    EnsureTunDevice,
    EnsureLoopback,
    ResolveIdentity,
    DeriveShellEnvironment,
    ExportShellEnvironment,
    BuildOpenglDriverFarm,
    MaterializeHome,
    MaterializeAllocatorPreload,
    RestrictDmesg,
    ConfigureIoUring,
    StartSchedExtScheduler,
    StartNixPrep,
    StartPodmanPrep,
    ExportNixRemote,
    StartWaylandProxy,
    EnsureNofileFloor,
    ClearProfileEnvBeforeExec,
    ReportProfileBeforeExec,
    DropAndExec,
    RunManagedSession,
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_enter_operations() -> Vec<CangEnterOperation> {
    vec![
        CangEnterOperation::ReadEnv,
        CangEnterOperation::ValidatePreparedRootPaths,
        CangEnterOperation::EnsureTmpTmpfs,
        CangEnterOperation::EnsureTunDevice,
        CangEnterOperation::EnsureLoopback,
        CangEnterOperation::ResolveIdentity,
        CangEnterOperation::DeriveShellEnvironment,
        CangEnterOperation::ExportShellEnvironment,
        CangEnterOperation::BuildOpenglDriverFarm,
        CangEnterOperation::MaterializeHome,
        CangEnterOperation::MaterializeAllocatorPreload,
        CangEnterOperation::RestrictDmesg,
        CangEnterOperation::ConfigureIoUring,
        CangEnterOperation::StartSchedExtScheduler,
        CangEnterOperation::StartNixPrep,
        CangEnterOperation::StartPodmanPrep,
        CangEnterOperation::ExportNixRemote,
        CangEnterOperation::StartWaylandProxy,
        CangEnterOperation::EnsureNofileFloor,
        CangEnterOperation::StartGuestSwap,
        CangEnterOperation::ClearProfileEnvBeforeExec,
        CangEnterOperation::ReportProfileBeforeExec,
        CangEnterOperation::DropAndExec,
        CangEnterOperation::RunManagedSession,
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PulseEndpoint {
    Direct(String),
    Bridge { port: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnterEnv {
    enter_as_root: bool,
    /// `true` only when the host named a build disk. No launch attaches one
    /// any more (cargo's `target/` and home live in the workspace tree), so a
    /// launch that names none must not probe `/dev` for it: the probe is a btrfs
    /// label scan that always fails and always costs ~100 ms on the boot path.
    build_disk_requested: bool,
    host_uid: Option<u32>,
    host_gid: Option<u32>,
    cang: CangEnv,
    pulse: Option<PulseEndpoint>,
    waypipe_port: Option<u32>,
    exec: Option<ExecConfig>,
    managed_session: Option<ManagedSessionConfig>,
}

trait EnvSource {
    fn var(&self, name: &str) -> Option<String>;
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
}

pub(in crate::guest_init) fn enter(command: Vec<String>) -> Result<()> {
    debug_breadcrumb("enter dispatch reached");
    let mut profiler = profile::GuestProfiler::from_process_env("cang enter");
    debug_breadcrumb("read-env starting");
    let env_contract = profiler.measure_result("read-env", || EnterEnv::from_env(&ProcessEnv))?;
    debug_breadcrumb("read-env complete");
    debug_breadcrumb("validate-prepared-root starting");
    profiler.measure_result("validate-prepared-root", || {
        validate_prepared_root_paths(&prepared_root_targets(&env_contract))
    })?;
    debug_breadcrumb("validate-prepared-root complete");
    profiler.measure_result("ensure-tmp-tmpfs", ensure_tmp_tmpfs_mounted)?;
    // Before anything long-lived is started, and not merely before the shell:
    // libkrun's init leaves this process with the descriptor limits it inherited
    // (soft 1024, hard 4096 in the guest), and every child it spawns keeps them.
    // The workspace relay opens one `O_PATH` descriptor per inode the host's
    // kernel looks up, so at 4096 it failed `EMFILE` after ~1 000 paths of a
    // `git status` - and the nix and Podman prep workers open far more than the
    // old value allows for their own work. Raising the ceiling here is what puts
    // every later child on it, the exec'd shell included.
    profiler.measure_result("ensure-nofile-floor", process::ensure_nofile_floor)?;
    profiler.measure_result("ensure-user-namespaces", || {
        // Synchronously, before the command is exec'd: the compatibility
        // wrappers around `rustc`/`rust-analyzer` run their tool under `bwrap`,
        // which needs an unprivileged user namespace, and the guest kernel
        // (`linux-hardened`) boots with `kernel.unprivileged_userns_clone=0`.
        // Raising it only from the background Podman prep worker meant a task
        // whose first `bwrap` ran before that worker reached its second
        // operation failed with "No permissions to create a new namespace" -
        // intermittently, once per launch at most. The worker's own raise stays
        // for the paths that do not come through here, and is a no-op.
        if process::is_root() {
            crate::guest_init::components::rootless::kernel::enable_user_namespaces()?;
        }
        Ok(())
    })?;
    profiler.measure_result("ensure-tun-device", || {
        crate::guest_init::components::rootless::kernel::prepare_tun_device()
    })?;
    profiler.measure_result("ensure-loopback", || {
        if process::is_root() {
            crate::guest_init::components::net::loopback::ensure_loopback_ipv4()?;
        }
        Ok(())
    })?;
    debug_breadcrumb("resolve-identity starting");
    let identity = profiler.measure_result("resolve-identity", || {
        resolve_identity(
            &command,
            &env_contract,
            process::is_root(),
            process::uid(),
            process::gid(),
        )
    })?;
    debug_breadcrumb("resolve-identity complete");
    profiler.measure_result("prepare-wrappers", || {
        if process::is_root() {
            crate::guest_init::components::rootless::idmap::prepare(
                &identity,
                env_contract.cang.permissions,
            )?;
        }
        Ok(())
    })?;
    let pulse_server = env_contract.pulse.as_ref().map(|pulse| match pulse {
        PulseEndpoint::Direct(value) => value.clone(),
        PulseEndpoint::Bridge { .. } => {
            crate::guest_init::components::pulse::server_value(&identity)
        }
    });
    let shell_env = profiler.measure("derive-shell-env", || {
        crate::guest_init::components::shell::env::derive(
            &identity,
            env_contract.cang.containers_storage,
            pulse_server.as_deref(),
        )
    });
    profiler.measure("export-shell-env", || {
        crate::guest_init::components::shell::env::export(&shell_env)
    });
    profiler.measure_result("mount-build-target", || {
        // `CARGO_TARGET_DIR` arrives from the host pointing at the disk it
        // attached; mounting it here is what makes that path the disk. A failure
        // removes the variable, so a build falls back to the workspace instead of
        // filling a rootfs directory that dies with the task.
        if !env_contract.build_disk_requested {
            return Ok(());
        }
        let mount = crate::guest_init::components::disk::build::ensure_ready(
            &env_contract.cang.build_disk_label,
            &env_contract.cang.build_disk_id,
            &identity,
        );
        match mount {
            Ok(_mount) => Ok(()),
            Err(err) => {
                eprintln!(
                    "cang-guest-init: build target disk unavailable, cargo builds onto the workspace: {err:#}"
                );
                Ok(())
            }
        }
    })?;
    profiler.measure("export-sccache-env", || {
        // Every cargo build in the guest goes through the cache cang already
        // mounts, or the cache why it is mounted there at all.
        crate::guest_init::components::sccache::export_if_available();
    });
    profiler.measure("export-gpu-env", || {
        crate::guest_init::components::wayland::export_mesa_if_enabled(env_contract.cang.gpu_drm)
    });
    profiler.measure("export-zg-env", || {
        crate::guest_init::components::zg::export_selection(env_contract.cang.gpu_drm)
    });
    profiler.measure_result("build-opengl-driver-farm", || {
        // The NixOS-conventional `/run/opengl-driver` farm is a boot-time root
        // operation; the explicit MESA_ENV above stays the authoritative
        // override, and the farm is what an env-less nixpkgs client resolves
        // through. Only a DRM guest has a hardware stack to expose.
        if process::is_root() {
            crate::guest_init::components::opengl::ensure_driver_layout_if_enabled(
                env_contract.cang.gpu_drm,
            )?;
        }
        Ok(())
    })?;
    profiler.measure_result("materialize-home", || {
        crate::guest_init::components::home::root::materialize(&identity)
    })?;
    profiler.measure_result("mount-workspace-disk", || {
        // `--workspace-disk` launches carry no `/workspace` bind mount, so the
        // tree the command runs in does not exist until this mounts it. Fatal
        // rather than degraded: an empty `/workspace` is not a smaller workspace,
        // it is a different measurement.
        if let Some(contract) = env_contract.cang.workspace_disk.as_ref() {
            crate::guest_init::components::disk::workspace::ensure_ready(contract, &identity)?;
        }
        Ok(())
    })?;

    profiler.measure_result("start-workspace-relay", || {
        // The host mounts this tree over FUSE, so the server has to be listening
        // before the task command can do anything useful - and it serves what
        // `mount-workspace-disk` just mounted, which is why this step is after it.
        if let Some(contract) = env_contract.cang.workspace_relay.as_ref() {
            crate::guest_init::components::relay::start(contract.port, &contract.root)?;
        }
        Ok(())
    })?;
    profiler.measure_result("seed-zg-models", || {
        crate::guest_init::components::zg::seed_models(process::is_root(), &identity)
    })?;
    profiler.measure_result("materialize-allocator-preload", || {
        crate::guest_init::components::hardening::allocator::ensure_from_env_if_root(
            process::is_root(),
        )
    })?;
    profiler.measure_result("restrict-dmesg", || {
        if process::is_root() {
            crate::guest_init::components::hardening::dmesg::restrict()?;
        }
        Ok(())
    })?;
    profiler.measure_result("configure-io-uring", || {
        if process::is_root() {
            crate::guest_init::components::hardening::io_uring::configure(
                env_contract
                    .cang
                    .permissions
                    .contains(crate::guest_init::components::env::GuestPermission::IoUring),
                identity.gid,
            )?;
        }
        Ok(())
    })?;
    profiler.measure_result("record-responsiveness-policy", || {
        // One small file under /run/cang/, written before the workload exists so a
        // later arm can read what the policy decided instead of inferring it from
        // the mask. Best effort: the levers apply whether or not it can be written.
        if process::is_root() {
            crate::guest_init::components::responsive::write_status();
        }
        Ok(())
    })?;
    profiler.measure_result("start-early-oom-responder", || {
        // Detached and idle until the guest is short of memory, so it costs one
        // fork/exec on the enter path and nothing after that. Started as root
        // because it raises its own OOM score and signals other users' processes.
        if process::is_root() {
            crate::guest_init::components::responsive::start_early_responder();
        }
        Ok(())
    })?;
    profiler.measure_result("start-sched-ext-scheduler", || {
        // Best effort and detached: the scheduler changes how the workload is
        // scheduled from here on, and a guest whose firmware or image cannot serve
        // one keeps the fair scheduler instead of failing the boot. Started as
        // root because loading it is a BPF program load, and before the workload
        // so its policy is in force for the workload's first wakeup.
        if process::is_root() {
            crate::guest_init::components::scx::start();
        }
        Ok(())
    })?;
    profiler.measure_result("configure-perf", || {
        if process::is_root() {
            crate::guest_init::components::hardening::perf::configure(
                env_contract
                    .cang
                    .permissions
                    .contains(crate::guest_init::components::env::GuestPermission::Perf),
            )?;
        }
        Ok(())
    })?;
    profiler.measure_result("configure-passt-dns", || {
        if env_contract.cang.use_passt && process::is_root() {
            crate::guest_init::components::net::dns::ensure_passt_resolv_conf(Path::new(
                "/etc/resolv.conf",
            ))?;
        }
        Ok(())
    })?;
    profiler.measure_result("start-nix-prep", || {
        crate::guest_init::components::nix::root::start_background_prep(&env_contract.cang)
    })?;
    profiler.measure_result("start-podman-prep", || {
        crate::guest_init::components::podman::root::start_background_prep(
            &identity,
            &env_contract.cang,
        )
    })?;
    profiler.measure("export-nix-remote", || {
        if env_contract.cang.nix_overlay {
            // SAFETY: cang guest-init mutates the process environment during
            // single-threaded bootstrap before exec so the shell sees NIX_REMOTE.
            unsafe { std::env::set_var("NIX_REMOTE", nix_remote_uri()) };
        }
    });
    profiler.measure_result("start-wayland-proxy", || {
        crate::guest_init::components::wayland::start_if_enabled(
            env_contract.cang.wayland,
            env_contract.cang.gpu_drm,
            &identity,
        )
    })?;
    let swap_gate = profiler.measure_result("start-guest-swap", || {
        // Guest swap is optional by design: it never fails the session, it only
        // decides whether memory pressure degrades or kills the workload.
        // Activating the zram device costs 0.6-1.4 s of this boot (the sizing is
        // CPU-bound: zram allocates metadata for the whole capacity), so it
        // happens in a detached worker instead of here - and the worker waits
        // until this process `exec`s, because measured beside the enter path its
        // sizing slowed that path by its own cost one-for-one on four vCPUs.
        // Started last on purpose: the worker's own startup is another ~0.2 s of
        // CPU and page-ins, and everything before this point would show it.
        // A worker that cannot be spawned is reported and the session continues,
        // because a guest without swap must still boot.
        if !process::is_root() {
            return Ok(None);
        }
        match crate::guest_init::components::swap::zram::start_background_ensure() {
            Ok(hold) => Ok(Some(hold)),
            Err(err) => {
                eprintln!("cang-guest-init: guest swap worker not started: {err:#}");
                Ok(None)
            }
        }
    })?;

    profile::clear_guest_profile_env();
    profiler.report_before_exec()?;
    let _pulse = match env_contract.pulse.as_ref() {
        Some(PulseEndpoint::Bridge { port }) => Some(
            crate::guest_init::components::pulse::start(*port, &identity)
                .context("failed to start guest Pulse bridge")?,
        ),
        _ => None,
    };
    let waypipe = env_contract
        .waypipe_port
        .map(|port| {
            crate::guest_init::components::waypipe::start(
                port,
                &identity,
                env_contract.cang.gpu_drm,
            )
        })
        .transpose()?;
    let _exec = env_contract
        .exec
        .map(|config| {
            super::exec::start(
                config,
                identity.clone(),
                env_contract.cang.permissions,
                waypipe.clone(),
            )
        })
        .transpose()?;
    let drop_to_identity = should_drop_to_identity(process::is_root(), env_contract.enter_as_root);
    if let Some(managed_session) = env_contract.managed_session {
        debug_breadcrumb("managed session starting");
        // This path never `exec`s, so the worker's `CLOEXEC` gate does not fire
        // on its own: release it here, at the hand-off, exactly as the exec path
        // releases it by `exec`ing.
        if let Some(gate) = swap_gate {
            gate.release();
        }
        return session::run(
            &command,
            &identity,
            env_contract.cang.permissions,
            drop_to_identity,
            managed_session,
        );
    }
    debug_breadcrumb("final exec handoff starting");
    // The exec path releases the worker by `exec`ing: the hold is `CLOEXEC`, so
    // the descriptor closes and the worker's read sees EOF. Leaked rather than
    // dropped, or the worker would start inside the hand-off.
    if let Some(gate) = swap_gate {
        std::mem::forget(gate);
    }
    if drop_to_identity {
        process::drop_to_identity_and_exec(&identity, env_contract.cang.permissions, &command)
    } else {
        process::exec_command(&command)
    }
}

impl EnterEnv {
    fn from_env(env: &impl EnvSource) -> Result<Self> {
        Ok(Self {
            enter_as_root: env_flag(env, ENTER_AS_ROOT_ENV),
            build_disk_requested: env.var(BUILD_DISK_LABEL_ENV).is_some(),
            host_uid: parse_optional_u32(env, HOST_UID_ENV)?,
            host_gid: parse_optional_u32(env, HOST_GID_ENV)?,
            cang: cang_env_from(env)?,
            pulse: parse_pulse_endpoint(env)?,
            waypipe_port: parse_optional_u32(env, WAYPIPE_PORT_ENV)?,
            exec: exec_from_env(env)?,
            managed_session: managed_session_from_env(env)?,
        })
    }
}

fn parse_pulse_endpoint(env: &impl EnvSource) -> Result<Option<PulseEndpoint>> {
    let direct = parse_pulse_server(env)?;
    let bridge = parse_optional_u32(env, PULSE_BRIDGE_PORT_ENV)?;
    match (direct, bridge) {
        (Some(value), None) => Ok(Some(PulseEndpoint::Direct(value))),
        (None, Some(0)) => bail!("{PULSE_BRIDGE_PORT_ENV} must be nonzero"),
        (None, Some(port)) => Ok(Some(PulseEndpoint::Bridge { port })),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => {
            bail!("{PULSE_SERVER_ENV} and {PULSE_BRIDGE_PORT_ENV} cannot both be configured")
        }
    }
}

fn parse_pulse_server(env: &impl EnvSource) -> Result<Option<String>> {
    let Some(value) = env.var(PULSE_SERVER_ENV) else {
        return Ok(None);
    };
    let address = value
        .strip_prefix("tcp:")
        .ok_or_else(|| anyhow!("{PULSE_SERVER_ENV} must use tcp:IP:PORT"))?
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("{PULSE_SERVER_ENV} must contain a valid IP address and port"))?;
    if address.port() == 0 {
        bail!("{PULSE_SERVER_ENV} port must be nonzero");
    }

    Ok(Some(format!("tcp:{address}")))
}

fn exec_from_env(env: &impl EnvSource) -> Result<Option<ExecConfig>> {
    let port = parse_optional_u32(env, "CANG_EXEC_PORT")?;
    let version = env.var("CANG_EXEC_PROTOCOL_VERSION");
    match (port, version) {
        (None, None) => Ok(None),
        (Some(port), Some(version)) => Ok(Some(ExecConfig {
            port,
            protocol_version: version
                .parse::<u16>()
                .context("invalid numeric value in CANG_EXEC_PROTOCOL_VERSION")?,
        })),
        _ => Err(anyhow!(
            "CANG_EXEC_PORT and CANG_EXEC_PROTOCOL_VERSION must be provided together"
        )),
    }
}

fn managed_session_from_env(env: &impl EnvSource) -> Result<Option<ManagedSessionConfig>> {
    if !env_flag(env, SESSION_MANAGED_ENV) {
        return Ok(None);
    }
    Ok(Some(ManagedSessionConfig {
        port: parse_required_u32(env, ATTACH_PORT_ENV)?,
        protocol_version: parse_required_u16(env, ATTACH_PROTOCOL_VERSION_ENV)?,
        attach_profile: env
            .var(ATTACH_PROFILE_ENV)
            .as_deref()
            .is_some_and(super::attach_profile::env_value_enabled),
    }))
}

fn parse_required_u32(env: &impl EnvSource, name: &str) -> Result<u32> {
    env.var(name)
        .ok_or_else(|| anyhow!("{name} is required when {SESSION_MANAGED_ENV}=1"))?
        .parse::<u32>()
        .with_context(|| format!("invalid numeric value in {name}"))
}

fn parse_required_u16(env: &impl EnvSource, name: &str) -> Result<u16> {
    env.var(name)
        .ok_or_else(|| anyhow!("{name} is required when {SESSION_MANAGED_ENV}=1"))?
        .parse::<u16>()
        .with_context(|| format!("invalid numeric value in {name}"))
}

fn cang_env_from(env: &impl EnvSource) -> Result<CangEnv> {
    Ok(CangEnv {
        nix_overlay: env_flag(env, "CANG_NIX_OVERLAY"),
        nix_host_overlay: env_flag(env, "CANG_NIX_HOST_OVERLAY"),
        containers_storage: env_flag(env, "CANG_CONTAINERS_STORAGE"),
        container_store_backend: ContainerStoreBackend::from_optional_env_value(
            env.var(CONTAINERS_STORE_ENV),
        )?,
        use_passt: env_flag(env, "CANG_USE_PASST"),
        gpu_drm: env_flag(env, "CANG_GPU_DRM"),
        wayland: env_flag(env, "CANG_WAYLAND"),
        permissions: env
            .var("CANG_PERMISSIONS")
            .map_or(Ok(Default::default()), |value| value.parse())?,
        enter_as_root: env_flag(env, ENTER_AS_ROOT_ENV),
        host_uid: parse_optional_u32(env, HOST_UID_ENV)?,
        host_gid: parse_optional_u32(env, HOST_GID_ENV)?,
        nix_disk_id: env
            .var("CANG_NIX_DISK_ID")
            .unwrap_or_else(|| crate::guest_init::components::env::RAW_NIX_DISK_ID.to_owned()),
        nix_disk_label: env
            .var("CANG_NIX_DISK_LABEL")
            .unwrap_or_else(|| crate::guest_init::components::env::RAW_NIX_DISK_LABEL.to_owned()),
        workspace_disk: crate::guest_init::components::env::WorkspaceDiskContract::from_lookup(
            |name| env.var(name).filter(|value| !value.is_empty()),
        )?,
        workspace_relay: crate::guest_init::components::env::WorkspaceRelayContract::from_lookup(
            |name| env.var(name).filter(|value| !value.is_empty()),
        )?,
        containers_disk_id: env.var("CANG_CONTAINERS_DISK_ID").unwrap_or_else(|| {
            crate::guest_init::components::env::RAW_CONTAINER_DISK_ID.to_owned()
        }),
        containers_disk_label: env.var("CANG_CONTAINERS_DISK_LABEL").unwrap_or_else(|| {
            crate::guest_init::components::env::RAW_CONTAINER_DISK_LABEL.to_owned()
        }),
        build_disk_id: env
            .var("CANG_BUILD_DISK_ID")
            .unwrap_or_else(|| crate::guest_init::components::env::RAW_BUILD_DISK_ID.to_owned()),
        build_disk_label: env
            .var("CANG_BUILD_DISK_LABEL")
            .unwrap_or_else(|| crate::guest_init::components::env::RAW_BUILD_DISK_LABEL.to_owned()),
        nix_lower: crate::guest_init::components::env::NixLowerContract::from_lookup(|name| {
            env.var(name)
        })?,
    })
}

/// The host-grafted paths this launch must already see. A `--workspace-disk`
/// launch is grafted `~/.cargo` by neither side: the guest binds the tree's own
/// `cargo/` there after it mounts the disk, so the path is absent at validation
/// time by design.
fn prepared_root_targets(env_contract: &EnterEnv) -> Vec<&'static str> {
    PREPARED_ROOT_TARGETS
        .iter()
        .copied()
        .filter(|path| {
            !(env_contract.cang.workspace_disk.is_some()
                && *path == crate::guest_init::components::disk::workspace::CARGO_HOME_TARGET)
        })
        .collect()
}

fn env_flag(env: &impl EnvSource, name: &str) -> bool {
    env.var(name).as_deref() == Some("1")
}

fn parse_optional_u32(env: &impl EnvSource, name: &str) -> Result<Option<u32>> {
    env.var(name)
        .map(|value| {
            value
                .parse::<u32>()
                .with_context(|| format!("invalid numeric value in {name}"))
        })
        .transpose()
}

fn resolve_identity(
    command: &[String],
    env: &EnterEnv,
    is_root: bool,
    uid: u32,
    gid: u32,
) -> Result<DevIdentity> {
    let shell = resolve_shell(command);
    if is_root {
        let uid = env
            .host_uid
            .ok_or_else(|| anyhow!("{HOST_UID_ENV} is required for cang enter"))?;
        let gid = env
            .host_gid
            .ok_or_else(|| anyhow!("{HOST_GID_ENV} is required for cang enter"))?;
        validate_host_identity(uid, gid)?;
        Ok(DevIdentity::new(uid, gid, shell))
    } else {
        Ok(DevIdentity::new(uid, gid, shell))
    }
}

fn should_drop_to_identity(is_root: bool, enter_as_root: bool) -> bool {
    is_root && !enter_as_root
}

fn resolve_shell(command: &[String]) -> PathBuf {
    let shell = command.first().map(String::as_str).unwrap_or(DEFAULT_SHELL);
    if shell.contains('/') {
        PathBuf::from(shell)
    } else {
        command::find_on_path(shell).unwrap_or_else(|| PathBuf::from(shell))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TmpTmpfsPlan {
    Mount,
    Remount,
}

fn validate_prepared_root_paths(paths: &[&str]) -> Result<()> {
    for path in paths {
        validate_prepared_root_path(Path::new(path))?;
    }
    Ok(())
}

fn validate_prepared_root_path(path: &Path) -> Result<()> {
    if path.exists() && path.is_dir() {
        return Ok(());
    }
    if path.exists() {
        bail!(
            "cang prepared-root path '{}' exists but is not a directory",
            path.display()
        );
    }
    bail!(
        "cang prepared-root path '{}' is missing; host prepared-root bind grafting failed",
        path.display()
    );
}

fn ensure_tmp_tmpfs_mounted() -> Result<()> {
    let tmp = Path::new("/tmp");
    fs::create_dir_all(tmp).context("failed to create cang /tmp mount target")?;
    let mounts = fs::read_to_string("/proc/mounts").context("failed to read /proc/mounts")?;
    match tmp_tmpfs_plan(&mounts, tmp)? {
        TmpTmpfsPlan::Mount => mount_tmp_tmpfs(tmp),
        TmpTmpfsPlan::Remount => remount_tmp_tmpfs(tmp),
    }
}

fn tmp_tmpfs_plan(mounts: &str, target: &Path) -> Result<TmpTmpfsPlan> {
    let target = target.display().to_string();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let Some(source) = fields.next() else {
            continue;
        };
        let Some(mountpoint) = fields.next() else {
            continue;
        };
        let Some(fs_type) = fields.next() else {
            continue;
        };
        if mountpoint == target {
            if source == "tmpfs" && fs_type == "tmpfs" {
                return Ok(TmpTmpfsPlan::Remount);
            }
            bail!(
                "cang /tmp target {target} is already mounted from {source} as {fs_type}, not tmpfs"
            );
        }
    }
    Ok(TmpTmpfsPlan::Mount)
}

fn mount_tmp_tmpfs(target: &Path) -> Result<()> {
    command::run(
        "mount",
        &[
            "-t",
            "tmpfs",
            "tmpfs",
            "-o",
            "rw,exec,mode=1777",
            &target.display().to_string(),
        ],
    )
    .context("failed to mount cang /tmp tmpfs")
}

fn remount_tmp_tmpfs(target: &Path) -> Result<()> {
    command::run(
        "mount",
        &[
            "-o",
            "remount,rw,exec,mode=1777",
            &target.display().to_string(),
        ],
    )
    .context("failed to remount cang /tmp tmpfs")
}

fn debug_breadcrumb(message: &str) {
    if std::env::var("CANG_GUEST_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("cang-guest-init: debug: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct TestEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for TestEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|value| (*value).to_owned())
        }
    }

    fn env(vars: &[(&'static str, &'static str)]) -> TestEnv {
        TestEnv(vars.iter().copied().collect())
    }

    #[test]
    fn planned_cang_enter_validates_prepared_root_before_identity_drop_and_exec() {
        let operations = planned_enter_operations();
        let pos = |op| {
            operations
                .iter()
                .position(|candidate| candidate == &op)
                .expect("operation should exist")
        };

        assert!(
            pos(CangEnterOperation::ValidatePreparedRootPaths)
                < pos(CangEnterOperation::ResolveIdentity)
        );
        assert!(
            pos(CangEnterOperation::ValidatePreparedRootPaths)
                < pos(CangEnterOperation::EnsureTmpTmpfs)
        );
        assert!(
            pos(CangEnterOperation::ValidatePreparedRootPaths)
                < pos(CangEnterOperation::EnsureTunDevice)
        );
        assert!(pos(CangEnterOperation::EnsureTmpTmpfs) < pos(CangEnterOperation::ResolveIdentity));
        assert!(
            pos(CangEnterOperation::EnsureTunDevice) < pos(CangEnterOperation::ResolveIdentity)
        );
        assert!(
            pos(CangEnterOperation::ValidatePreparedRootPaths)
                < pos(CangEnterOperation::StartNixPrep)
        );
        assert!(
            pos(CangEnterOperation::ResolveIdentity) < pos(CangEnterOperation::StartPodmanPrep)
        );
        assert!(pos(CangEnterOperation::ConfigureIoUring) < pos(CangEnterOperation::StartNixPrep));
        assert!(
            pos(CangEnterOperation::ConfigureIoUring) < pos(CangEnterOperation::StartPodmanPrep)
        );
        assert!(
            pos(CangEnterOperation::ConfigureIoUring) < pos(CangEnterOperation::StartWaylandProxy)
        );
        assert!(
            pos(CangEnterOperation::ConfigureIoUring) < pos(CangEnterOperation::RunManagedSession)
        );
        assert!(pos(CangEnterOperation::ConfigureIoUring) < pos(CangEnterOperation::DropAndExec));
        assert!(pos(CangEnterOperation::StartNixPrep) < pos(CangEnterOperation::StartPodmanPrep));
        assert!(
            pos(CangEnterOperation::ExportNixRemote) < pos(CangEnterOperation::EnsureNofileFloor)
        );
        assert!(pos(CangEnterOperation::EnsureNofileFloor) < pos(CangEnterOperation::DropAndExec));
        assert!(pos(CangEnterOperation::StartPodmanPrep) < pos(CangEnterOperation::DropAndExec));
        assert!(pos(CangEnterOperation::ExportNixRemote) < pos(CangEnterOperation::DropAndExec));
    }

    #[test]
    fn planned_cang_enter_ensures_loopback_before_user_command() {
        let operations = planned_enter_operations();
        let pos = |op| {
            operations
                .iter()
                .position(|candidate| candidate == &op)
                .expect("operation should exist")
        };

        assert!(pos(CangEnterOperation::EnsureLoopback) < pos(CangEnterOperation::DropAndExec));
        assert!(
            pos(CangEnterOperation::EnsureLoopback) < pos(CangEnterOperation::RunManagedSession)
        );
        assert!(pos(CangEnterOperation::EnsureLoopback) < pos(CangEnterOperation::StartNixPrep));
    }

    /// The scheduler has to be driving the guest before the workload's first
    /// wakeup, and after the step that can still fail the session.
    #[test]
    fn planned_cang_enter_starts_the_scheduler_before_the_workload() {
        let operations = planned_enter_operations();
        let pos = |op| {
            operations
                .iter()
                .position(|candidate| candidate == &op)
                .expect("operation should exist")
        };

        assert!(
            pos(CangEnterOperation::StartSchedExtScheduler) < pos(CangEnterOperation::DropAndExec)
        );
        assert!(
            pos(CangEnterOperation::StartSchedExtScheduler)
                < pos(CangEnterOperation::RunManagedSession)
        );
        assert!(
            pos(CangEnterOperation::ConfigureIoUring)
                < pos(CangEnterOperation::StartSchedExtScheduler)
        );
    }

    /// The swap *worker* is started before the workload, not its activation:
    /// `ensure-guest-swap` used to block exec for 0.6-1.2 s, and the device is
    /// only needed once something asks the kernel for memory.
    #[test]
    fn planned_cang_enter_starts_the_swap_worker_before_the_workload_starts() {
        let operations = planned_enter_operations();
        let pos = |op| {
            operations
                .iter()
                .position(|candidate| candidate == &op)
                .expect("operation should exist")
        };

        assert!(pos(CangEnterOperation::StartGuestSwap) < pos(CangEnterOperation::DropAndExec));
        assert!(
            pos(CangEnterOperation::StartGuestSwap) < pos(CangEnterOperation::RunManagedSession)
        );
        assert!(
            pos(CangEnterOperation::StartNixPrep) < pos(CangEnterOperation::StartGuestSwap),
            "the worker is started last: its own startup would otherwise show up in the stages before it"
        );
    }

    #[test]
    fn planned_cang_enter_builds_the_opengl_driver_farm_before_the_workload_starts() {
        let operations = planned_enter_operations();
        let pos = |op| {
            operations
                .iter()
                .position(|candidate| candidate == &op)
                .expect("operation should exist")
        };

        assert!(
            pos(CangEnterOperation::ExportShellEnvironment)
                < pos(CangEnterOperation::BuildOpenglDriverFarm)
        );
        assert!(
            pos(CangEnterOperation::BuildOpenglDriverFarm)
                < pos(CangEnterOperation::StartWaylandProxy)
        );
        assert!(
            pos(CangEnterOperation::BuildOpenglDriverFarm) < pos(CangEnterOperation::DropAndExec)
        );
        assert!(
            pos(CangEnterOperation::BuildOpenglDriverFarm)
                < pos(CangEnterOperation::RunManagedSession)
        );
    }

    #[test]
    fn cang_env_no_longer_requires_bind_mount_env() {
        let parsed = EnterEnv::from_env(&env(&[])).expect("prepared-root env should parse");

        assert!(!parsed.enter_as_root);
        assert_eq!(parsed.host_uid, None);
        assert_eq!(parsed.host_gid, None);
        assert_eq!(parsed.pulse, None);
        assert_eq!(parsed.waypipe_port, None);
        assert_eq!(parsed.exec, None);
    }

    #[test]
    fn pulse_server_is_validated_and_canonicalized() {
        let parsed = EnterEnv::from_env(&env(&[(PULSE_SERVER_ENV, "tcp:[2001:db8::10]:4713")]))
            .expect("Pulse endpoint should parse");

        assert_eq!(
            parsed.pulse,
            Some(PulseEndpoint::Direct("tcp:[2001:db8::10]:4713".to_owned()))
        );
    }

    #[test]
    fn pulse_server_rejects_malformed_values() {
        for value in [
            "udp:192.0.2.10:4713",
            "tcp:pulse.example.test:4713",
            "tcp:192.0.2.10:0",
            "tcp:2001:db8::10:4713",
        ] {
            let error = EnterEnv::from_env(&env(&[(PULSE_SERVER_ENV, value)]))
                .expect_err("malformed Pulse endpoint should fail");

            assert!(format!("{error:#}").contains(PULSE_SERVER_ENV));
        }
    }

    #[test]
    fn pulse_bridge_port_is_parsed_and_conflicts_with_direct_endpoint() {
        let parsed = EnterEnv::from_env(&env(&[(PULSE_BRIDGE_PORT_ENV, "50429")]))
            .expect("Pulse bridge port should parse");
        assert_eq!(parsed.pulse, Some(PulseEndpoint::Bridge { port: 50_429 }));

        let error = EnterEnv::from_env(&env(&[
            (PULSE_SERVER_ENV, "tcp:192.0.2.10:4713"),
            (PULSE_BRIDGE_PORT_ENV, "50429"),
        ]))
        .expect_err("direct and bridged Pulse endpoints should conflict");
        assert!(format!("{error:#}").contains("cannot both be configured"));
    }

    #[test]
    fn gpu_drm_is_forwarded_independently_from_wayland() {
        let parsed =
            EnterEnv::from_env(&env(&[("CANG_GPU_DRM", "1")])).expect("DRM env should parse");

        assert!(parsed.cang.gpu_drm);
        assert!(!parsed.cang.wayland);
    }

    #[test]
    fn waypipe_port_is_parsed_for_persistent_service() {
        let parsed = EnterEnv::from_env(&env(&[(WAYPIPE_PORT_ENV, "50427")]))
            .expect("waypipe env should parse");

        assert_eq!(parsed.waypipe_port, Some(50_427));
    }

    #[test]
    fn exec_env_is_all_or_none() {
        let parsed = EnterEnv::from_env(&env(&[
            ("CANG_EXEC_PORT", "50428"),
            ("CANG_EXEC_PROTOCOL_VERSION", "2"),
        ]))
        .expect("exec env should parse");
        assert_eq!(
            parsed.exec,
            Some(ExecConfig {
                port: 50_428,
                protocol_version: 2,
            })
        );

        let err = EnterEnv::from_env(&env(&[("CANG_EXEC_PORT", "50428")])).unwrap_err();
        assert!(format!("{err:#}").contains("must be provided together"));
    }

    #[test]
    fn cang_env_accepts_host_disk_contract_names() {
        let env = EnterEnv::from_env(&env(&[
            ("CANG_NIX_OVERLAY", "1"),
            ("CANG_NIX_HOST_OVERLAY", "1"),
            ("CANG_NIX_DISK_ID", "cang-nix"),
            ("CANG_NIX_DISK_LABEL", "CANG_NIX"),
            ("CANG_CONTAINERS_STORAGE", "1"),
            ("CANG_CONTAINERS_DISK_ID", "cang-containers"),
            ("CANG_CONTAINERS_DISK_LABEL", "CANG_CONTAINERS"),
            (HOST_UID_ENV, "1000"),
            (HOST_GID_ENV, "1001"),
        ]))
        .expect("env should parse");

        assert!(env.cang.nix_overlay);
        assert!(env.cang.nix_host_overlay);
        assert!(env.cang.containers_storage);
        assert_eq!(
            env.cang.container_store_backend,
            ContainerStoreBackend::RawDisk
        );
        assert!(!env.cang.use_passt);
        assert_eq!(env.cang.nix_disk_id, "cang-nix");
        assert_eq!(env.cang.nix_disk_label, "CANG_NIX");
        assert_eq!(env.cang.containers_disk_id, "cang-containers");
        assert_eq!(env.cang.containers_disk_label, "CANG_CONTAINERS");
        assert_eq!(env.cang.host_uid, Some(1000));
        assert_eq!(env.cang.host_gid, Some(1001));
    }

    #[test]
    fn cang_env_rejects_bind_container_store_backend() {
        let err = EnterEnv::from_env(&env(&[
            ("CANG_CONTAINERS_STORAGE", "1"),
            (CONTAINERS_STORE_ENV, "bind"),
        ]))
        .expect_err("bind backend should fail");

        assert!(err.to_string().contains(CONTAINERS_STORE_ENV));
    }

    #[test]
    fn cang_env_rejects_invalid_container_store_backend() {
        let err = EnterEnv::from_env(&env(&[(CONTAINERS_STORE_ENV, "overlay")]))
            .expect_err("invalid backend should fail");

        assert!(err.to_string().contains(CONTAINERS_STORE_ENV));
    }

    #[test]
    fn prepared_root_targets_exclude_raw_disk_container_store_mount() {
        let _raw = EnterEnv::from_env(&env(&[
            ("CANG_CONTAINERS_STORAGE", "1"),
            (CONTAINERS_STORE_ENV, "raw-disk"),
        ]))
        .expect("raw env should parse");

        assert!(
            !prepared_root_targets(&_raw)
                .contains(&crate::guest_init::components::disk::containers::MOUNT_POINT)
        );
    }

    #[test]
    fn a_build_disk_probe_runs_only_when_the_host_named_one() {
        // No launch attaches a build disk any more, and an unnamed disk used to
        // reach a btrfs label scan on every boot: ~100 ms that always failed.
        let plain = EnterEnv::from_env(&env(&[])).expect("env should parse");
        assert!(!plain.build_disk_requested);

        let named = EnterEnv::from_env(&env(&[
            ("CANG_BUILD_DISK_LABEL", "CANG_BUILD"),
            ("CANG_BUILD_DISK_ID", "cang-build"),
        ]))
        .expect("build disk env should parse");
        assert!(named.build_disk_requested);
    }

    #[test]
    fn a_workspace_disk_launch_does_not_expect_grafted_paths_it_owns() {
        let plain = EnterEnv::from_env(&env(&[])).expect("env should parse");
        assert!(
            prepared_root_targets(&plain)
                .contains(&crate::guest_init::components::disk::workspace::CARGO_HOME_TARGET)
        );

        let disk = EnterEnv::from_env(&env(&[
            ("CANG_WORKSPACE_DISK_ID", "cang-workspace"),
            ("CANG_WORKSPACE_DISK_LABEL", "CANG_WORKSPACE"),
        ]))
        .expect("workspace disk env should parse");
        assert!(disk.cang.workspace_disk.is_some());
        assert!(
            !prepared_root_targets(&disk)
                .contains(&crate::guest_init::components::disk::workspace::CARGO_HOME_TARGET)
        );
        assert!(prepared_root_targets(&disk).contains(&"/workspace"));
    }

    #[test]
    fn a_half_set_workspace_disk_contract_is_an_error() {
        let err = EnterEnv::from_env(&env(&[("CANG_WORKSPACE_DISK_ID", "cang-workspace")]))
            .expect_err("half contract should fail");
        assert!(
            err.to_string().contains("CANG_WORKSPACE_DISK_LABEL"),
            "{err}"
        );
    }

    #[test]
    fn cang_identity_requires_host_ids_when_root_drops_to_dev() {
        let missing_ids_env = EnterEnv::from_env(&env(&[])).expect("env should parse");
        let err = resolve_identity(
            &["fish".to_owned(), "-l".to_owned()],
            &missing_ids_env,
            true,
            0,
            0,
        )
        .expect_err("host ids are required");
        assert!(err.to_string().contains(HOST_UID_ENV));

        let valid_env = EnterEnv::from_env(&env(&[(HOST_UID_ENV, "1000"), (HOST_GID_ENV, "1001")]))
            .expect("env should parse");
        let identity = resolve_identity(
            &["fish".to_owned(), "-l".to_owned()],
            &valid_env,
            true,
            0,
            0,
        )
        .expect("identity should resolve");
        assert_eq!(identity.uid, 1000);
        assert_eq!(identity.gid, 1001);
    }

    #[test]
    fn cang_root_shell_still_materializes_non_root_dev_identity() {
        let root_env = EnterEnv::from_env(&env(&[
            (crate::guest_init::components::env::ENTER_AS_ROOT_ENV, "1"),
            (HOST_UID_ENV, "1000"),
            (HOST_GID_ENV, "1001"),
        ]))
        .expect("env should parse");

        let identity =
            resolve_identity(&["fish".to_owned(), "-l".to_owned()], &root_env, true, 0, 0)
                .expect("root shell identity should resolve");

        assert_eq!(identity.uid, 1000);
        assert_eq!(identity.gid, 1001);
    }

    #[test]
    fn prepared_root_validation_requires_existing_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace dir");

        validate_prepared_root_paths(&[workspace.to_str().expect("utf8")])
            .expect("existing directory should pass");

        let missing = dir.path().join("missing");
        let err = validate_prepared_root_paths(&[missing.to_str().expect("utf8")])
            .expect_err("missing prepared-root path should fail");
        assert!(format!("{err:#}").contains("prepared-root path"));

        let file = dir.path().join("file");
        fs::write(&file, "not a dir").expect("file");
        let err = validate_prepared_root_paths(&[file.to_str().expect("utf8")])
            .expect_err("file prepared-root path should fail");
        assert!(format!("{err:#}").contains("not a directory"));
    }

    #[test]
    fn tmp_tmpfs_plan_mounts_absent_tmp_and_remounts_existing_tmpfs() {
        assert_eq!(
            tmp_tmpfs_plan("", Path::new("/tmp")).expect("missing /tmp mount should mount"),
            TmpTmpfsPlan::Mount
        );
        assert_eq!(
            tmp_tmpfs_plan("tmpfs /tmp tmpfs rw,nosuid,nodev 0 0\n", Path::new("/tmp"))
                .expect("existing tmpfs should remount with cang options"),
            TmpTmpfsPlan::Remount
        );
    }

    #[test]
    fn tmp_tmpfs_plan_rejects_non_tmpfs_mountpoint() {
        let err = tmp_tmpfs_plan("other /tmp virtiofs rw 0 0\n", Path::new("/tmp"))
            .expect_err("non-tmpfs /tmp mount should fail");

        assert!(format!("{err:#}").contains("not tmpfs"));
    }
}
