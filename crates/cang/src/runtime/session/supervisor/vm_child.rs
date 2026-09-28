//! Forked VM child process and direct libkrun entry path.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::logging::{self, LogSettings};
use crate::runtime::fork_child;
use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};
use crate::runtime::landlock;
use crate::runtime::launch::config::{LaunchConfig, NetworkMode};
use crate::runtime::seccomp::{self, AuditMode, SeccompMode};
use crate::runtime::session::nix_overlay;
use crate::runtime::session::profile::{CangHostProfiler, vm_worker_wait_detail_path};
use crate::runtime::session::supervisor::LIBKRUN_VM_WORKER_ARG;
use crate::runtime::session::supervisor::entry::task_state_dir_from_config_path;
use crate::runtime::session::supervisor::identity;
use crate::runtime::session::supervisor::managed_exit_marker::{self, ManagedExitObservation};
use crate::runtime::session::supervisor::readiness_pipe::HelperReadyWriter;
use crate::runtime::vm::libkrun::{DirectLibkrunLauncher, LinkedLibkrunApi};
use crate::runtime::vm::network;
use crate::runtime::vm::prepared_root;

const SANDBOXED_CHILD_POLL: Duration = Duration::from_millis(25);
const MANAGED_EXIT_MARKER_WAIT: Duration = Duration::from_millis(500);
const MANAGED_EXIT_MARKER_POLL: Duration = Duration::from_millis(10);
const SANDBOXED_PARENT_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];
static SANDBOXED_PARENT_SIGNAL: AtomicI32 = AtomicI32::new(0);

pub(crate) struct VmWorkerGuard {
    pid: libc::pid_t,
}

impl VmWorkerGuard {
    pub(crate) fn new(pid: libc::pid_t) -> Self {
        Self { pid }
    }

    pub(crate) fn wait(&mut self) -> Result<i32> {
        let status = network::wait_pid(self.pid)?;
        self.pid = -1;
        Ok(status)
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<i32>> {
        if self.pid <= 0 {
            return Ok(None);
        }
        let mut status = 0;
        // SAFETY: pid is a child process id returned by fork.
        let rc = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if rc == 0 {
            return Ok(None);
        }
        if rc == self.pid {
            self.pid = -1;
            return Ok(Some(status));
        }
        bail!(
            "failed to poll cang VM worker {}: {}",
            self.pid,
            std::io::Error::last_os_error()
        );
    }

    pub(crate) fn terminate(&mut self) {
        if self.pid > 0 {
            network::cleanup_pid(self.pid);
            self.pid = -1;
        }
    }
}

impl Drop for VmWorkerGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) fn start_vm_worker(
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
    seccomp_mode: &SeccompMode,
) -> Result<VmWorkerGuard> {
    match seccomp_mode {
        SeccompMode::Audit(audit_mode) => {
            spawn_traced_vm_worker(config_path, holder_pid, passt_fd, audit_mode)
        }
        SeccompMode::Off | SeccompMode::Enforce { .. } => {
            spawn_vm_worker(config_path, holder_pid, passt_fd).map(VmWorkerGuard::new)
        }
    }
}

/// Start the VM worker as a fresh `cang internal ...` process.
///
/// The host helper is multi-threaded by the time it gets here (the Pulse bridge
/// runs in that process), so `fork()`ing a worker would hand the child every lock
/// another helper thread held at that instant, glibc's malloc arenas included,
/// and a worker that blocks on one of them never enters the target netns or
/// reports readiness. Re-execing this binary instead leaves the worker with a
/// single thread and no inherited lock. The seccomp audit path already starts its
/// worker this way, with strace in front.
pub(crate) fn spawn_vm_worker(
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
) -> Result<libc::pid_t> {
    let executable =
        std::env::current_exe().context("failed to resolve cang executable for the VM worker")?;
    let spec = build_vm_worker_command(&executable, config_path, holder_pid, passt_fd);
    let child = spec.into_command().spawn().with_context(|| {
        format!(
            "failed to start cang VM worker for '{}'",
            config_path.display()
        )
    })?;
    libc::pid_t::try_from(child.id()).context("cang VM worker pid overflowed pid_t")
}

fn spawn_traced_vm_worker(
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
    audit_mode: &AuditMode,
) -> Result<VmWorkerGuard> {
    let trace_path = audit_mode.trace_path();
    seccomp::prepare_audit_trace_target(trace_path)?;
    let executable = std::env::current_exe()
        .context("failed to resolve cang executable for traced VM worker")?;
    let strace_filter = match audit_mode {
        AuditMode::Full { .. } => None,
        AuditMode::Gap {
            baseline_policy_path,
            ..
        } => seccomp::strace_exclusion_filter_from_policy(baseline_policy_path)?,
        AuditMode::DefaultGap { .. } => {
            bail!("unresolved default seccomp gap audit cannot reach the VM worker")
        }
    };
    let spec = build_traced_vm_worker_command(
        &executable,
        trace_path,
        strace_filter.as_deref(),
        config_path,
        holder_pid,
        passt_fd,
    );
    tracing::debug!(program = ?spec.program, args = ?spec.args, "cang traced VM worker command constructed");
    let child = spec.into_command().spawn().with_context(|| {
        format!(
            "failed to start traced cang VM worker for '{}'; {}",
            config_path.display(),
            seccomp::ptrace_failure_hint()
        )
    })?;
    let pid = libc::pid_t::try_from(child.id()).context("traced VM worker pid overflowed pid_t")?;
    Ok(VmWorkerGuard::new(pid))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmWorkerCommandSpec {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
}

impl VmWorkerCommandSpec {
    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
        command
    }
}

pub(crate) fn build_vm_worker_command(
    executable: &Path,
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
) -> VmWorkerCommandSpec {
    let mut args = vec![
        OsString::from("internal"),
        OsString::from(LIBKRUN_VM_WORKER_ARG),
        config_path.as_os_str().to_owned(),
        OsString::from(holder_pid.to_string()),
    ];
    if let Some(fd) = passt_fd {
        args.push(OsString::from(fd.to_string()));
    }
    VmWorkerCommandSpec {
        program: executable.as_os_str().to_owned(),
        args,
    }
}

pub(crate) fn build_traced_vm_worker_command(
    executable: &Path,
    trace_path: &Path,
    strace_filter: Option<&str>,
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
) -> VmWorkerCommandSpec {
    let raw_path = seccomp::raw_strace_path(trace_path);
    let mut args = vec![OsString::from("-f"), OsString::from("-qq")];
    if let Some(filter) = strace_filter {
        args.push(OsString::from("-e"));
        args.push(OsString::from(filter));
    }
    args.extend([
        OsString::from("-o"),
        raw_path.into_os_string(),
        OsString::from("--"),
        executable.as_os_str().to_owned(),
    ]);
    args.extend(build_vm_worker_command(executable, config_path, holder_pid, passt_fd).args);
    VmWorkerCommandSpec {
        program: runtime_tool_program(RuntimeTool::Strace),
        args,
    }
}

pub(crate) fn run_vm_worker_internal(args: Vec<OsString>) -> Result<()> {
    let invocation = parse_vm_worker_internal_args(args)?;
    HelperReadyWriter::close_in_vm_worker_child_from_env();
    std::process::exit(run_vm_worker_child(
        &invocation.config_path,
        invocation.holder_pid,
        invocation.passt_fd,
    ));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmWorkerInternalInvocation {
    pub(crate) config_path: PathBuf,
    pub(crate) holder_pid: libc::pid_t,
    pub(crate) passt_fd: Option<i32>,
}

pub(crate) fn parse_vm_worker_internal_args(
    args: Vec<OsString>,
) -> Result<VmWorkerInternalInvocation> {
    if !(2..=3).contains(&args.len()) {
        bail!(
            "expected internal {LIBKRUN_VM_WORKER_ARG} <launch.conf> <holder-pid> [passt-fd], got {} argument(s)",
            args.len() + 1
        );
    }
    let mut args = args.into_iter();
    let config_path = args
        .next()
        .expect("validated VM worker internal config argument");
    let holder_pid = args
        .next()
        .expect("validated VM worker internal holder pid argument");
    let holder_pid = parse_i32_arg("holder-pid", holder_pid)?;
    let passt_fd = args
        .next()
        .map(|fd| parse_i32_arg("passt-fd", fd))
        .transpose()?;
    Ok(VmWorkerInternalInvocation {
        config_path: PathBuf::from(config_path),
        holder_pid,
        passt_fd,
    })
}

fn parse_i32_arg(label: &str, value: OsString) -> Result<i32> {
    value
        .to_str()
        .ok_or_else(|| anyhow!("internal {label} argument is not valid UTF-8"))?
        .parse::<i32>()
        .with_context(|| format!("internal {label} argument is not a valid i32"))
}

fn run_vm_worker_child(config_path: &Path, holder_pid: libc::pid_t, passt_fd: Option<i32>) -> i32 {
    let mut profiler = CangHostProfiler::from_env_started_now();
    profiler.record_metadata("profile_scope", "vm_worker");
    profiler.record_metadata(
        "profile_launch_config_path",
        config_path.display().to_string(),
    );

    let result = run_vm_worker(config_path, holder_pid, passt_fd, &mut profiler);
    let exit_code = if result.is_ok() { 0 } else { 1 };
    profiler.record_metadata("profile_exit_code", exit_code.to_string());
    if let Err(err) = &result {
        eprintln!("cang internal VM worker: {err:#}");
    }
    if let Ok(task_state_dir) = task_state_dir_from_config_path(config_path) {
        let _ = profiler.write_vm_worker_wait_details(task_state_dir);
    }
    let _ = profiler.finalize_result(result);
    exit_code
}

fn run_vm_worker(
    config_path: &Path,
    holder_pid: libc::pid_t,
    passt_fd: Option<i32>,
    profiler: &mut CangHostProfiler,
) -> Result<()> {
    let config = profiler.measure_result("vm_worker_config_read", || {
        LaunchConfig::read_from(config_path)
    })?;
    profiler.measure_result("vm_worker_tracing_init", || {
        logging::init_tracing(&LogSettings::for_internal_helper(config.log_level))
    })?;
    profiler.measure_result("vm_worker_identity_configure", || {
        identity::configure_vm_worker_filesystem_identity()
    })?;
    profiler.measure_result("vm_worker_enter_netns", || network::enter_netns(holder_pid))?;
    let task_state_dir = task_state_dir_from_config_path(config_path)?;
    profiler.record_metadata(
        "profile_task_state_dir",
        task_state_dir.display().to_string(),
    );
    let config = match config.network_mode {
        NetworkMode::Tsi => config,
        NetworkMode::Passt => profiler.measure_result("vm_worker_passt_fd_handoff", || {
            passt_fd
                .map(|fd| config.with_passt_fd(fd))
                .ok_or_else(|| anyhow::anyhow!("cang passt mode requires inherited passt fd"))
        })?,
    };
    run_libkrun_in_current_namespace(&config, task_state_dir, profiler)
}

fn run_libkrun_in_current_namespace(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profiler: &mut CangHostProfiler,
) -> Result<()> {
    let nix_overlay_mount = match config.host_nix_overlay.as_ref() {
        Some(intent) => Some(profiler.measure_result("vm_worker_mount_nix_overlay", || {
            nix_overlay::materialize_in_worker(intent)
        })?),
        None => None,
    };
    let prepared_root = profiler.measure_result("vm_worker_prepare_root", || {
        prepared_root::prepare(config, task_state_dir)
    })?;
    let run_result =
        run_libkrun_in_sandboxed_child(config, task_state_dir, profiler, &prepared_root);
    let nix_overlay_cleanup = nix_overlay_mount.map(|mount| move || mount.unmount());
    finalize_vm_worker_run(
        run_result,
        || prepared_root.unmount(),
        nix_overlay_cleanup,
        config.managed_session.is_some().then_some(task_state_dir),
    )
}

fn run_libkrun_in_sandboxed_child(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profiler: &mut CangHostProfiler,
    prepared_root: &prepared_root::PreparedRoot,
) -> Result<()> {
    let _signal_handlers = SandboxedParentSignalHandlers::install()?;
    let mut child = fork_sandboxed_libkrun_child(config, task_state_dir, profiler, prepared_root)?;
    let status = wait_for_sandboxed_child(&mut child)?;
    sandboxed_child_status_result(
        status,
        config.managed_session.is_some().then_some(task_state_dir),
    )
}

fn fork_sandboxed_libkrun_child(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profiler: &mut CangHostProfiler,
    prepared_root: &prepared_root::PreparedRoot,
) -> Result<VmWorkerGuard> {
    // SAFETY: getpid only reads this process id.
    let cleanup_parent_pid = unsafe { libc::getpid() };
    // SAFETY: fork creates a child inside the already-prepared VM worker mount
    // namespace. The parent intentionally stays unlandlocked so it can clean up
    // prepared-root and host /nix overlay mounts after the sandboxed child exits.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!(
            "failed to fork sandboxed cang VM worker child: {}",
            std::io::Error::last_os_error()
        );
    }
    if pid == 0 {
        // SAFETY: the child leaves through `_exit` instead of the Rust runtime's
        // exit path. The fork is taken by the VM worker, which `spawn_vm_worker`
        // leaves as a freshly exec'd single-threaded process, so this child cannot
        // inherit a lock from another thread of its parent.
        fork_child::exit_child(run_sandboxed_libkrun_child(
            config,
            task_state_dir,
            profiler,
            prepared_root,
            cleanup_parent_pid,
        ));
    }
    Ok(VmWorkerGuard::new(pid))
}

fn run_sandboxed_libkrun_child(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profiler: &mut CangHostProfiler,
    prepared_root: &prepared_root::PreparedRoot,
    cleanup_parent_pid: libc::pid_t,
) -> i32 {
    if let Err(err) = prepare_sandboxed_child_signal_lifecycle(cleanup_parent_pid) {
        report_sandboxed_child_error(&err);
        return 1;
    }
    let result = run_libkrun_with_prepared_root(config, task_state_dir, profiler, prepared_root);
    if let Err(err) = &result {
        report_sandboxed_child_error(err);
    }
    if result.is_ok() { 0 } else { 1 }
}

/// Report a sandboxed-child failure on stderr without taking the stdio lock.
///
/// Rendering the message allocates, which is fine for this child: the VM worker
/// it is forked from is a freshly exec'd single-threaded process, so no other
/// thread can hold the allocator.
fn report_sandboxed_child_error(err: &anyhow::Error) {
    fork_child::report_child_error(format!("cang sandboxed VM worker: {err:#}\n").as_bytes());
}

fn prepare_sandboxed_child_signal_lifecycle(cleanup_parent_pid: libc::pid_t) -> Result<()> {
    restore_default_sandboxed_parent_signals()?;
    set_parent_death_signal(libc::SIGTERM)?;
    ensure_sandboxed_parent_still_alive(cleanup_parent_pid)
}

fn wait_for_sandboxed_child(child: &mut VmWorkerGuard) -> Result<i32> {
    loop {
        if let Some(signal) = take_sandboxed_parent_signal() {
            child.terminate();
            bail!(
                "cang VM worker cleanup parent was interrupted by signal {signal}; terminated sandboxed child before mount cleanup"
            );
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(SANDBOXED_CHILD_POLL);
    }
}

fn take_sandboxed_parent_signal() -> Option<libc::c_int> {
    match SANDBOXED_PARENT_SIGNAL.swap(0, Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

extern "C" fn record_sandboxed_parent_signal(signal: libc::c_int) {
    SANDBOXED_PARENT_SIGNAL.store(signal, Ordering::SeqCst);
}

struct SandboxedParentSignalHandlers {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl SandboxedParentSignalHandlers {
    fn install() -> Result<Self> {
        SANDBOXED_PARENT_SIGNAL.store(0, Ordering::SeqCst);
        let mut previous = Vec::with_capacity(SANDBOXED_PARENT_SIGNALS.len());
        for signal in SANDBOXED_PARENT_SIGNALS {
            previous.push((signal, install_signal_handler(signal)?));
        }
        Ok(Self { previous })
    }
}

impl Drop for SandboxedParentSignalHandlers {
    fn drop(&mut self) {
        for (signal, previous) in &self.previous {
            // SAFETY: Restores handlers captured from successful sigaction calls
            // in this process. Errors are ignored because the VM worker is
            // already leaving the sandbox-child wait scope.
            let _ = unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
        SANDBOXED_PARENT_SIGNAL.store(0, Ordering::SeqCst);
    }
}

fn install_signal_handler(signal: libc::c_int) -> Result<libc::sigaction> {
    let mut action = zeroed_sigaction();
    action.sa_sigaction = record_sandboxed_parent_signal as *const () as libc::sighandler_t;
    action.sa_flags = 0;
    // SAFETY: Initializes an empty mask for this local sigaction value.
    let mask_rc = unsafe { libc::sigemptyset(&mut action.sa_mask) };
    if mask_rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to initialize signal mask");
    }
    let mut previous = MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: Installs a simple async-signal-safe handler for the VM worker
    // cleanup parent and writes the previous handler to `previous`.
    let rc = unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to install sandboxed child signal handler {signal}"));
    }
    // SAFETY: `sigaction` succeeded and initialized `previous`.
    Ok(unsafe { previous.assume_init() })
}

fn restore_default_sandboxed_parent_signals() -> Result<()> {
    for signal in SANDBOXED_PARENT_SIGNALS {
        restore_default_signal(signal)?;
    }
    Ok(())
}

fn restore_default_signal(signal: libc::c_int) -> Result<()> {
    let mut action = zeroed_sigaction();
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: Initializes an empty mask for this local sigaction value.
    let mask_rc = unsafe { libc::sigemptyset(&mut action.sa_mask) };
    if mask_rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to initialize signal mask");
    }
    // SAFETY: Restores the default disposition for the sandboxed child so it
    // does not inherit the cleanup parent's handler.
    let rc = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to restore default signal handler {signal}"));
    }
    Ok(())
}

fn set_parent_death_signal(signal: libc::c_int) -> Result<()> {
    // SAFETY: prctl with PR_SET_PDEATHSIG only affects this sandboxed child and
    // asks the kernel to deliver `signal` if the unlandlocked cleanup parent dies.
    let rc = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, signal, 0, 0, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
            .context("failed to configure sandboxed VM worker parent-death signal")
    }
}

fn ensure_sandboxed_parent_still_alive(expected_parent_pid: libc::pid_t) -> Result<()> {
    // SAFETY: getppid only reads this process's current parent pid.
    let current_parent_pid = unsafe { libc::getppid() };
    sandboxed_parent_liveness_result(expected_parent_pid, current_parent_pid)
}

fn sandboxed_parent_liveness_result(
    expected_parent_pid: libc::pid_t,
    current_parent_pid: libc::pid_t,
) -> Result<()> {
    if current_parent_pid == expected_parent_pid {
        return Ok(());
    }
    bail!(
        "sandboxed VM worker cleanup parent exited before parent-death signal was armed: expected parent pid {expected_parent_pid}, current parent pid {current_parent_pid}"
    )
}

fn zeroed_sigaction() -> libc::sigaction {
    // SAFETY: A zeroed sigaction is immediately initialized before installation.
    unsafe { std::mem::zeroed() }
}

fn sandboxed_child_status_result(status: i32, managed_task_state_dir: Option<&Path>) -> Result<()> {
    sandboxed_child_status_result_with_marker_wait(
        status,
        managed_task_state_dir,
        MANAGED_EXIT_MARKER_WAIT,
        MANAGED_EXIT_MARKER_POLL,
    )
}

fn sandboxed_child_status_result_with_marker_wait(
    status: i32,
    managed_task_state_dir: Option<&Path>,
    marker_wait: Duration,
    marker_poll: Duration,
) -> Result<()> {
    if let Some(code) = network::status_exit_code(status) {
        if code == 0 {
            return Ok(());
        }
        if let Some(task_state_dir) = managed_task_state_dir {
            let observation = managed_exit_marker::wait_for_matching_observed_guest_exit(
                task_state_dir,
                code,
                marker_wait,
                marker_poll,
            );
            if let ManagedExitObservation::ObservedGuestExit(_) = observation {
                return Ok(());
            }
            bail!(
                "sandboxed cang VM worker child exited with status {code}{}",
                managed_exit_observation_suffix(&observation)
            );
        }
        bail!("sandboxed cang VM worker child exited with status {code}");
    }
    if libc::WIFSIGNALED(status) {
        bail!(
            "sandboxed cang VM worker child was terminated by signal {}",
            libc::WTERMSIG(status)
        );
    }
    bail!("sandboxed cang VM worker child ended with unexpected wait status {status}")
}

fn managed_exit_observation_suffix(observation: &ManagedExitObservation) -> String {
    match observation {
        ManagedExitObservation::ObservedGuestExit(_) => String::new(),
        ManagedExitObservation::NoObservedGuestExit => {
            "; no parent-observed managed guest exit marker was found".to_owned()
        }
        ManagedExitObservation::ObservedGuestExitDifferentCode { expected, observed } => {
            format!("; parent observed managed guest exit status {observed}, expected {expected}")
        }
        ManagedExitObservation::InvalidMarker(err) => {
            format!("; invalid parent-observed managed guest exit marker: {err}")
        }
    }
}

fn run_libkrun_with_prepared_root(
    config: &LaunchConfig,
    task_state_dir: &Path,
    profiler: &mut CangHostProfiler,
    prepared_root: &prepared_root::PreparedRoot,
) -> Result<()> {
    let launch_config = config.with_root_export(prepared_root.root().to_path_buf());
    let guest_config_path = profiler.measure_result("vm_worker_guest_config_write", || {
        launch_config.write_guest_config_to_rootfs()
    })?;
    let enforce_seccomp_policy = profiler.measure_result(
        "vm_worker_seccomp_policy_compile",
        || match &launch_config.seccomp {
            SeccompMode::Enforce { policy_path } => {
                seccomp::compile_enforce_policy(policy_path).map(Some)
            }
            SeccompMode::Off | SeccompMode::Audit(_) => Ok(None),
        },
    )?;
    tracing::debug!(
        task_state = %task_state_dir.display(),
        source_rootfs = %config.task_rootfs.display(),
        rootfs = %launch_config.task_rootfs.display(),
        guest_config = %guest_config_path.display(),
        prepared_root_bind_count = launch_config.mounts.len(),
        disks = launch_config.disks.len(),
        ram_mib = launch_config.ram_mib,
        vcpus = launch_config.vcpus,
        exec_path = %launch_config.exec_path,
        argv_len = launch_config.argv.len(),
        env_len = launch_config.env.len(),
        guest_config_env_len = launch_config.guest_config_env.len(),
        "cang internal: launch config loaded"
    );
    tracing::debug!("libkrun API open: begin");
    let api = profiler.measure_result("vm_worker_libkrun_open", || Ok(LinkedLibkrunApi::new()))?;
    tracing::debug!("libkrun API open: complete");
    let session_started_at = Instant::now();
    let configure_started_at = Instant::now();
    let mut configure_duration = std::time::Duration::ZERO;
    let mut pre_enter_reached = false;
    let libkrun_profile_path = if profiler.is_enabled() {
        Some(vm_worker_wait_detail_path(task_state_dir))
    } else {
        None
    };
    let result = DirectLibkrunLauncher::new(api).start_enter_profiled_with_pre_enter_hook(
        &launch_config,
        libkrun_profile_path.as_deref(),
        || {
            configure_duration = configure_started_at.elapsed();
            pre_enter_reached = true;
            profiler.record_vm_worker_libkrun_configure(configure_duration);
            let _ = profiler.write_vm_worker_wait_details(task_state_dir);
            landlock::apply(&launch_config, task_state_dir, profiler.is_enabled())?;
            if let Some(policy) = &enforce_seccomp_policy {
                seccomp::apply_compiled_enforce_policy(policy)?;
            }
            tracing::debug!(
                landlock = launch_config.landlock.as_config_value(),
                seccomp = launch_config.seccomp.as_config_value(),
                "cang internal: pre-enter hook complete"
            );
            Ok(())
        },
    );
    let session_duration = session_started_at.elapsed();
    profiler.record_vm_worker_libkrun_session(session_duration);
    if pre_enter_reached {
        profiler
            .record_vm_worker_libkrun_enter(session_duration.saturating_sub(configure_duration));
    }
    result
}

#[cfg(test)]
fn pre_enter_security_order_for_test(config: &LaunchConfig) -> Vec<&'static str> {
    let mut steps = Vec::new();
    if matches!(config.seccomp, SeccompMode::Enforce { .. }) {
        steps.push("seccomp-compile");
    }
    if config.landlock != crate::runtime::landlock::LandlockMode::Off {
        steps.push("landlock");
    }
    if matches!(config.seccomp, SeccompMode::Enforce { .. }) {
        steps.push("seccomp-apply");
    }
    steps
}

fn finalize_vm_worker_run<PreparedCleanup, NixCleanup>(
    run_result: Result<()>,
    cleanup_prepared_root: PreparedCleanup,
    cleanup_nix_overlay: Option<NixCleanup>,
    managed_task_state_dir: Option<&Path>,
) -> Result<()>
where
    PreparedCleanup: FnOnce() -> Result<()>,
    NixCleanup: FnOnce() -> Result<()>,
{
    let prepared_root_cleanup = cleanup_prepared_root();
    let nix_overlay_cleanup = cleanup_nix_overlay.map(|cleanup| cleanup());
    let cleanup_result = combine_cleanup_results(prepared_root_cleanup, nix_overlay_cleanup);
    let observed_guest_exit = cleanup_result
        .as_ref()
        .err()
        .and(managed_task_state_dir)
        .and_then(managed_exit_marker::observed_guest_exit_code);
    combine_run_and_cleanup_results(run_result, cleanup_result, observed_guest_exit)
}

fn combine_cleanup_results(
    prepared_root_cleanup: Result<()>,
    nix_overlay_cleanup: Option<Result<()>>,
) -> Result<()> {
    match (prepared_root_cleanup, nix_overlay_cleanup) {
        (Ok(()), Some(Ok(()))) | (Ok(()), None) => Ok(()),
        (Ok(()), Some(Err(cleanup_error))) => Err(cleanup_error),
        (Err(cleanup_error), Some(Ok(()))) | (Err(cleanup_error), None) => Err(cleanup_error),
        (Err(prepared_error), Some(Err(nix_error))) => Err(prepared_error.context(format!(
            "also failed to unmount cang host /nix overlay: {nix_error:#}"
        ))),
    }
}

fn combine_run_and_cleanup_results(
    run_result: Result<()>,
    cleanup_result: Result<()>,
    observed_guest_exit: Option<i32>,
) -> Result<()> {
    match (run_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(cleanup_error)) => Err(context_cleanup_after_guest_exit(
            cleanup_error,
            observed_guest_exit,
            "failed to clean up cang VM worker mounts",
        )),
        (Err(run_error), Ok(())) => Err(run_error),
        (Err(run_error), Err(cleanup_error)) => {
            let context = match observed_guest_exit {
                Some(code) => format!(
                    "failed to clean up cang VM worker mounts after managed guest exited with status {code}; libkrun error: {run_error:#}"
                ),
                None => {
                    format!(
                        "failed to clean up cang VM worker mounts after libkrun error: {run_error:#}"
                    )
                }
            };
            Err(cleanup_error.context(context))
        }
    }
}

fn context_cleanup_after_guest_exit(
    cleanup_error: anyhow::Error,
    observed_guest_exit: Option<i32>,
    context: &'static str,
) -> anyhow::Error {
    match observed_guest_exit {
        Some(code) => cleanup_error.context(format!(
            "{context} after managed guest exited with status {code}"
        )),
        None => cleanup_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::ffi::OsStr;

    #[test]
    fn pre_enter_security_order_compiles_seccomp_before_landlock_then_applies_after() {
        let mut config = LaunchConfig {
            task_rootfs: Path::new("/rootfs").to_path_buf(),
            hostname: "cang-test".to_owned(),
            mounts: Vec::new(),
            host_nix_overlay: None,
            guest_init_override: None,
            disks: Vec::new(),
            ram_mib: 1024,
            vcpus: 1,
            log_level: crate::logging::LogLevel::Off,
            network_mode: NetworkMode::Tsi,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            publish: Vec::new(),
            workdir: "/workspace".to_owned(),
            exec_path: "/init".to_owned(),
            argv: Vec::new(),
            env: Vec::new(),
            guest_config_env: Vec::new(),
            passt_fd: None,
            pulse_bridge: None,
            waypipe: None,
            exec: None,
            managed_session: None,
            seccomp: SeccompMode::Enforce {
                policy_path: Path::new("/policy.json").to_path_buf(),
            },
            landlock: crate::runtime::landlock::LandlockMode::All,
        };

        assert_eq!(
            pre_enter_security_order_for_test(&config),
            ["seccomp-compile", "landlock", "seccomp-apply"]
        );

        config.landlock = crate::runtime::landlock::LandlockMode::Off;
        assert_eq!(
            pre_enter_security_order_for_test(&config),
            ["seccomp-compile", "seccomp-apply"]
        );
    }

    #[test]
    fn sandboxed_child_status_accepts_clean_exit() {
        assert!(sandboxed_child_status_result(0, None).is_ok());
    }

    #[test]
    fn sandboxed_child_status_rejects_nonzero_exit() {
        let status = 7 << 8;
        let err =
            sandboxed_child_status_result(status, None).expect_err("nonzero exit should fail");

        assert!(format!("{err:#}").contains("exited with status 7"));
    }

    #[test]
    fn managed_sandboxed_child_status_accepts_matching_observed_guest_exit_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 127).expect("write marker");

        let result = sandboxed_child_status_result_with_marker_wait(
            127 << 8,
            Some(temp.path()),
            Duration::ZERO,
            Duration::from_millis(1),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn managed_sandboxed_child_status_rejects_nonzero_without_observed_marker() {
        let temp = tempfile::tempdir().expect("tempdir");

        let err = sandboxed_child_status_result_with_marker_wait(
            127 << 8,
            Some(temp.path()),
            Duration::ZERO,
            Duration::from_millis(1),
        )
        .expect_err("missing parent-observed exit marker should fail");
        let message = format!("{err:#}");

        assert!(message.contains("exited with status 127"));
        assert!(message.contains("no parent-observed managed guest exit marker"));
    }

    #[test]
    fn managed_sandboxed_child_status_rejects_different_observed_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 126).expect("write marker");

        let err = sandboxed_child_status_result_with_marker_wait(
            127 << 8,
            Some(temp.path()),
            Duration::ZERO,
            Duration::from_millis(1),
        )
        .expect_err("different parent-observed exit marker should fail");
        let message = format!("{err:#}");

        assert!(message.contains("exited with status 127"));
        assert!(message.contains("parent observed managed guest exit status 126"));
    }

    #[test]
    fn sandboxed_child_status_rejects_signaled_exit() {
        let err = sandboxed_child_status_result(libc::SIGTERM, None)
            .expect_err("signaled child should fail");

        assert!(format!("{err:#}").contains("terminated by signal"));
    }

    #[test]
    fn sandboxed_child_wait_terminates_child_when_cleanup_parent_is_signaled() {
        // SAFETY: test forks a child that only waits for a signal and exits.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork should succeed");
        if pid == 0 {
            loop {
                // SAFETY: pause blocks until the parent sends a terminating signal.
                unsafe { libc::pause() };
            }
        }

        let mut child = VmWorkerGuard::new(pid);
        SANDBOXED_PARENT_SIGNAL.store(libc::SIGTERM, Ordering::SeqCst);
        let err =
            wait_for_sandboxed_child(&mut child).expect_err("signal should interrupt child wait");

        assert!(format!("{err:#}").contains("interrupted by signal"));
        // SAFETY: kill(pid, 0) probes whether the process still exists.
        let rc = unsafe { libc::kill(pid, 0) };
        assert_eq!(rc, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn sandboxed_parent_liveness_check_closes_parent_death_signal_race() {
        assert!(sandboxed_parent_liveness_result(123, 123).is_ok());

        let err = sandboxed_parent_liveness_result(123, 1)
            .expect_err("reparented child should fail before libkrun starts");

        assert!(format!("{err:#}").contains("parent-death signal was armed"));
        assert!(format!("{err:#}").contains("expected parent pid 123"));
        assert!(format!("{err:#}").contains("current parent pid 1"));
    }

    #[test]
    fn finalization_unmounts_prepared_root_before_nix_overlay() {
        let calls = RefCell::new(Vec::new());

        finalize_vm_worker_run(
            Ok(()),
            || {
                calls.borrow_mut().push("prepared-root");
                Ok(())
            },
            Some(|| {
                calls.borrow_mut().push("nix-overlay");
                Ok(())
            }),
            None,
        )
        .expect("finalization should succeed");

        assert_eq!(calls.into_inner(), vec!["prepared-root", "nix-overlay"]);
    }

    #[test]
    fn finalization_attempts_nix_overlay_after_prepared_root_cleanup_failure() {
        let calls = RefCell::new(Vec::new());

        let err = finalize_vm_worker_run(
            Ok(()),
            || {
                calls.borrow_mut().push("prepared-root");
                Err(anyhow!("prepared-root busy"))
            },
            Some(|| {
                calls.borrow_mut().push("nix-overlay");
                Ok(())
            }),
            None,
        )
        .expect_err("prepared-root failure should surface");

        assert_eq!(calls.into_inner(), vec!["prepared-root", "nix-overlay"]);
        assert!(format!("{err:#}").contains("prepared-root busy"));
    }

    #[test]
    fn finalization_contextualizes_cleanup_failure_after_run_failure() {
        let err = finalize_vm_worker_run(
            Err(anyhow!("libkrun failed")),
            || Err(anyhow!("prepared-root busy")),
            None::<fn() -> Result<()>>,
            None,
        )
        .expect_err("cleanup failure should surface with run context");

        let message = format!("{err:#}");
        assert!(message.contains("prepared-root busy"));
        assert!(message.contains("libkrun failed"));
    }

    #[test]
    fn finalization_contextualizes_cleanup_failure_after_observed_guest_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 127).expect("write marker");

        let err = finalize_vm_worker_run(
            Ok(()),
            || Err(anyhow!("prepared-root busy")),
            None::<fn() -> Result<()>>,
            Some(temp.path()),
        )
        .expect_err("cleanup failure should surface with guest status context");
        let message = format!("{err:#}");

        assert!(message.contains("prepared-root busy"));
        assert!(message.contains("managed guest exited with status 127"));
    }

    #[test]
    fn traced_vm_worker_command_targets_private_worker_entrypoint() {
        let spec = build_traced_vm_worker_command(
            Path::new("/nix/store/bin/cang"),
            Path::new("/tmp/cang-seccomp.trace.jsonl"),
            None,
            Path::new("/tmp/task/launch.conf"),
            12345,
            None,
        );

        assert_eq!(
            spec.args,
            vec![
                OsString::from("-f"),
                OsString::from("-qq"),
                OsString::from("-o"),
                OsString::from("/tmp/cang-seccomp.trace.jsonl.strace"),
                OsString::from("--"),
                OsString::from("/nix/store/bin/cang"),
                OsString::from("internal"),
                OsString::from(LIBKRUN_VM_WORKER_ARG),
                OsString::from("/tmp/task/launch.conf"),
                OsString::from("12345"),
            ]
        );
    }

    #[test]
    fn traced_vm_worker_command_includes_gap_audit_filter() {
        let spec = build_traced_vm_worker_command(
            Path::new("/nix/store/bin/cang"),
            Path::new("/tmp/cang-seccomp.denied.jsonl"),
            Some("trace=!read,write"),
            Path::new("/tmp/task/launch.conf"),
            12345,
            None,
        );

        assert_eq!(
            spec.args,
            vec![
                OsString::from("-f"),
                OsString::from("-qq"),
                OsString::from("-e"),
                OsString::from("trace=!read,write"),
                OsString::from("-o"),
                OsString::from("/tmp/cang-seccomp.denied.jsonl.strace"),
                OsString::from("--"),
                OsString::from("/nix/store/bin/cang"),
                OsString::from("internal"),
                OsString::from(LIBKRUN_VM_WORKER_ARG),
                OsString::from("/tmp/task/launch.conf"),
                OsString::from("12345"),
            ]
        );
    }

    #[test]
    fn traced_vm_worker_command_preserves_passt_fd_argument() {
        let spec = build_traced_vm_worker_command(
            Path::new("/bin/cang"),
            Path::new("/tmp/trace.jsonl"),
            None,
            Path::new("/tmp/task/launch.conf"),
            99,
            Some(42),
        );

        assert_eq!(
            spec.args.last().map(OsString::as_os_str),
            Some(OsStr::new("42"))
        );
    }

    #[test]
    fn vm_worker_internal_args_parse_required_and_optional_values() {
        let invocation = parse_vm_worker_internal_args(vec![
            "/tmp/task/launch.conf".into(),
            "123".into(),
            "44".into(),
        ])
        .expect("VM worker internal args should parse");

        assert_eq!(
            invocation.config_path,
            PathBuf::from("/tmp/task/launch.conf")
        );
        assert_eq!(invocation.holder_pid, 123);
        assert_eq!(invocation.passt_fd, Some(44));

        let invocation =
            parse_vm_worker_internal_args(vec!["/tmp/task/launch.conf".into(), "123".into()])
                .expect("VM worker internal args should parse without passt fd");
        assert_eq!(invocation.passt_fd, None);
    }

    #[test]
    fn vm_worker_internal_args_reject_bad_shape_and_numbers() {
        let err = parse_vm_worker_internal_args(vec!["/tmp/task/launch.conf".into()])
            .expect_err("missing holder pid should fail");
        assert!(format!("{err:#}").contains("expected internal"));

        let err =
            parse_vm_worker_internal_args(vec!["/tmp/task/launch.conf".into(), "not-a-pid".into()])
                .expect_err("invalid holder pid should fail");
        assert!(format!("{err:#}").contains("holder-pid"));
    }
}
