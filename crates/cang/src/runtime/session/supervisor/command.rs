//! Host-side libkrun helper command construction and process spawning.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::logging::INTERNAL_LOG_LEVEL_ENV;
use crate::runtime::launch::config::LaunchConfig;
use crate::runtime::seccomp;
use crate::runtime::session::attach::{self, AttachInputPolicy, AttachOutcome};
use crate::runtime::session::profile::{CANG_HOST_PROFILE_ENV, CangHostProfiler};
use crate::runtime::session::supervisor::guest_death;
use crate::runtime::session::supervisor::identity::KeepIdLauncher;
use crate::runtime::session::supervisor::managed_exit_marker;
use crate::runtime::session::supervisor::managed_ready;
use crate::runtime::session::supervisor::readiness_pipe::{ParentReadyPipe, READY_FD_ENV};
use crate::runtime::session::supervisor::render_server::{self, RenderServerEnv};
use crate::runtime::session::supervisor::sigwinch::SigwinchForwarder;
use crate::runtime::session::supervisor::{ChildStatus, LIBKRUN_ENTER_HELPER_ARG};
use crate::runtime::session::task_control::{
    ActiveTaskSpec, ProcessIdentity, remove_active_task, write_active_task,
};
use crate::runtime::vm::gpu::GpuMode;

/// Opts the VM worker's in-process vrend into GBM-backed buffers and the
/// `VIRGL_CAP_V2_RESOURCE_LAYOUT` capability. Without it vrend neither
/// advertises resource-layout queries nor allocates the shared resources
/// through GBM, so the guest synthesizes strides that need not match the host
/// allocation; a dma-buf exported from such a resource is then rejected by
/// venus/RADV with `VK_ERROR_INVALID_DRM_FORMAT_MODIFIER_PLANE_LAYOUT_EXT`.
const VREND_GBM_LAYOUT_ENV: &str = "VIRGL_GBM_LAYOUT_ENABLE";

pub(crate) fn run_helper_process(
    config: &LaunchConfig,
    config_path: &Path,
    profiler: &mut CangHostProfiler,
    active_task: &ActiveTaskSpec,
    daemon_initial_attach: bool,
    attach_input_policy: AttachInputPolicy,
) -> Result<ChildStatus> {
    let host_profile_enabled = profiler.is_enabled();
    let mut ready_pipe = if config.managed_session.is_some() {
        Some(ParentReadyPipe::create()?)
    } else {
        None
    };
    let ready_fd = ready_pipe.as_ref().and_then(ParentReadyPipe::writer_fd);
    let mut render_server = if config.gpu_mode == GpuMode::Drm {
        Some(render_server::spawn_render_server(ready_fd)?)
    } else {
        None
    };
    let spec = profiler.measure_result("helper_command_build", || {
        let executable = std::env::current_exe()
            .context("failed to resolve cang executable for libkrun helper")?;
        build_helper_command(
            &executable,
            config_path,
            config,
            host_profile_enabled,
            ready_fd,
            render_server.as_ref().map(|guard| &guard.env),
        )
    })?;
    tracing::debug!(program = ?spec.program, args = ?spec.args, log_level = config.log_level.as_str(), "cang libkrun helper command constructed");
    let audit_trace_path = config.seccomp.audit_trace_path().map(Path::to_path_buf);
    let mut command = spec.into_command();
    let managed_helper_logs = match &config.managed_session {
        Some(managed) => {
            managed_exit_marker::reset_observed_guest_exit(&active_task.task_dir)?;
            Some(ManagedHelperLogs::create(
                &active_task.task_dir,
                &managed.guest_kernel_console_log,
            )?)
        }
        None => None,
    };
    if config.managed_session.is_some() {
        let logs = managed_helper_logs
            .as_ref()
            .expect("managed helper logs must be initialized");
        command
            .stdin(Stdio::null())
            // The virtio-console device feeds the guest console to the helper's
            // stdout, so capturing it keeps the guest kernel's own account of a
            // death instead of discarding it.
            .stdout(logs.stdout.spawn_stdio()?)
            .stderr(logs.stderr.spawn_stdio()?);
    } else {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    // SAFETY: this pre-exec hook only calls async-signal-safe libc setsid in
    // the child process before exec. A dedicated session and process group give
    // cang a bounded task-level control identity for `cang kill`.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() >= 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = profiler.measure_result("helper_spawn_process", || {
        command.spawn().with_context(|| {
        format!(
            "failed to start cang libkrun helper for '{}'; rootless direct-libkrun launches require util-linux unshare plus newuidmap/newgidmap and usable /etc/subuid + /etc/subgid entries",
            config_path.display()
        )
        })
    })?;
    if let Some(pipe) = &mut ready_pipe {
        pipe.close_parent_writer();
    }
    tracing::debug!(pid = child.id(), "cang libkrun helper spawned");
    let process = match ProcessIdentity::from_spawned_process(child.id(), child.id(), child.id()) {
        Ok(process) => process,
        Err(err) => {
            terminate_spawned_child_group(&mut child);
            let _ = remove_active_task(&active_task.task_dir);
            return Err(err).context("failed to identify active cang task after helper spawn");
        }
    };
    let _sigwinch_forwarder = if config.managed_session.is_some() {
        None
    } else {
        match start_sigwinch_forwarder(process.pgid) {
            Ok(forwarder) => forwarder,
            Err(err) => {
                terminate_spawned_child_group(&mut child);
                let _ = remove_active_task(&active_task.task_dir);
                return Err(err)
                    .context("failed to start cang helper SIGWINCH forwarder after helper spawn");
            }
        }
    };
    if let Err(err) = write_active_task(active_task.clone(), process) {
        terminate_spawned_child_group(&mut child);
        let _ = remove_active_task(&active_task.task_dir);
        return Err(err).context("failed to record active cang task after helper spawn");
    }
    if let Some(managed) = &config.managed_session {
        let ready_pipe = ready_pipe.as_mut().ok_or_else(|| {
            anyhow::anyhow!("managed cang helper readiness pipe was not initialized")
        })?;
        let helper_ready_timeout = managed_ready::managed_helper_ready_timeout()?;
        if let Err(err) = profiler.measure_result("helper_managed_attach_ready", || {
            ready_pipe.wait_for_ready(&mut child, helper_ready_timeout)
        }) {
            terminate_spawned_child_group(&mut child);
            let _ = remove_active_task(&active_task.task_dir);
            replay_managed_helper_stderr(managed_helper_logs.as_ref());
            return Err(context_audit_error(
                err,
                audit_trace_path.as_deref(),
                "failed while waiting for managed cang attach readiness",
            ));
        }
        let exit_observer =
            |code| managed_exit_marker::write_observed_guest_exit(&active_task.task_dir, code);
        let attach_result = profiler.measure_result("helper_initial_attach", || {
            attach::attach_to_ready_socket_with_input_policy_and_exit_observer(
                &managed.attach_socket,
                daemon_initial_attach,
                attach_input_policy,
                Some(&exit_observer),
            )
        });
        return match attach_result {
            Ok(AttachOutcome::Detached) => {
                // The launcher exits while the VM worker keeps running. The
                // render server's real peer is the worker's inherited
                // socketpair copy, so disarm the guard: dropping it must only
                // close the launcher's parent_fd, not SIGTERM the runner.
                if let Some(guard) = render_server.as_mut() {
                    guard.disarm();
                }
                Ok(ChildStatus::detached())
            }
            Ok(AttachOutcome::Exited(code)) => {
                let status = match child.wait() {
                    Ok(status) => status,
                    Err(err) => {
                        replay_managed_helper_stderr(managed_helper_logs.as_ref());
                        return Err(err)
                            .context("failed to wait for managed cang helper after guest exit");
                    }
                };
                managed_helper_exit_result(
                    status,
                    code,
                    &active_task.task_dir,
                    managed_helper_logs.as_ref(),
                )
            }
            Err(err) => {
                terminate_spawned_child_group(&mut child);
                let _ = remove_active_task(&active_task.task_dir);
                replay_managed_helper_stderr(managed_helper_logs.as_ref());
                Err(context_audit_error(
                    err,
                    audit_trace_path.as_deref(),
                    "failed to attach to managed cang guest session",
                ))
            }
        };
    }

    let status = profiler.measure_result("helper_wait_process", || {
        child
            .wait()
            .context("failed to wait for cang libkrun helper")
    })?;
    tracing::debug!(?status, "cang libkrun helper exited");
    Ok(match status.code() {
        Some(code) => ChildStatus::exited(code),
        None => ChildStatus::signaled(),
    })
}

const MANAGED_HELPER_STDERR_LOG: &str = "helper.stderr.log";

/// The managed helper's stdout carries the guest entrypoint's own output, which
/// the virtio-console port feeds there. It would otherwise be discarded.
const MANAGED_HELPER_STDOUT_LOG: &str = "helper.stdout.log";

#[derive(Debug, Clone)]
struct ManagedHelperLog {
    path: PathBuf,
    label: &'static str,
}

impl ManagedHelperLog {
    fn create(task_state_dir: &Path, file_name: &str, label: &'static str) -> Result<Self> {
        let path = task_state_dir.join(file_name);
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .with_context(|| format!("failed to create {label} '{}'", path.display()))?;
        Ok(Self { path, label })
    }

    fn spawn_stdio(&self) -> Result<Stdio> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| {
                format!(
                    "failed to open {label} '{path}' for helper",
                    label = self.label,
                    path = self.path.display()
                )
            })?;
        Ok(Stdio::from(file))
    }

    fn read_to_string(&self) -> Result<String> {
        let mut file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .with_context(|| {
                format!(
                    "failed to read {label} '{path}'",
                    label = self.label,
                    path = self.path.display()
                )
            })?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(contents)
    }
}

/// The two streams a managed helper writes for the host to read afterwards: the
/// helper's own stderr and the guest console.
#[derive(Debug, Clone)]
struct ManagedHelperLogs {
    stderr: ManagedHelperLog,
    stdout: ManagedHelperLog,
    /// Written by libkrun itself, so it is only ever read from here.
    kernel_console: ManagedHelperLog,
}

impl ManagedHelperLogs {
    fn create(task_state_dir: &Path, kernel_console_log: &Path) -> Result<Self> {
        Ok(Self {
            stderr: ManagedHelperLog::create(
                task_state_dir,
                MANAGED_HELPER_STDERR_LOG,
                "managed cang helper stderr log",
            )?,
            stdout: ManagedHelperLog::create(
                task_state_dir,
                MANAGED_HELPER_STDOUT_LOG,
                "managed cang helper stdout log",
            )?,
            kernel_console: ManagedHelperLog {
                path: kernel_console_log.to_path_buf(),
                label: "guest kernel console log",
            },
        })
    }

    /// Text for guest-death classification, kernel console first: the guest
    /// kernel explains an OOM kill or a panic only there.
    ///
    /// Best effort by design: an unreadable log (libkrun may never have created
    /// the console file) simply contributes nothing.
    fn read_for_diagnosis(&self) -> String {
        let mut contents = String::new();
        for log in [&self.kernel_console, &self.stderr, &self.stdout] {
            if let Ok(text) = log.read_to_string() {
                contents.push_str(&text);
            }
            contents.push('\n');
        }
        contents
    }
}

fn managed_helper_exit_result(
    status: ExitStatus,
    guest_code: i32,
    task_state_dir: &Path,
    logs: Option<&ManagedHelperLogs>,
) -> Result<ChildStatus> {
    managed_helper_exit_result_with_replay(status, guest_code, task_state_dir, logs, true)
}

fn managed_helper_exit_result_with_replay(
    status: ExitStatus,
    guest_code: i32,
    task_state_dir: &Path,
    logs: Option<&ManagedHelperLogs>,
    replay_stderr: bool,
) -> Result<ChildStatus> {
    tracing::debug!(
        ?status,
        guest_code,
        "managed cang helper exited after guest exit"
    );
    let stderr = match logs.map(|logs| logs.stderr.read_to_string()).transpose() {
        Ok(Some(contents)) => contents,
        Ok(None) => String::new(),
        Err(err) => format!("failed to read managed helper stderr log: {err:#}\n"),
    };
    // The guest kernel explains an OOM kill or a panic only on its console, so
    // classify the death before deciding how the helper exited: otherwise a
    // guest killed under memory pressure looks exactly like a finished task.
    let death_cause = guest_death::GuestDeathCause::diagnose(
        &logs
            .map(ManagedHelperLogs::read_for_diagnosis)
            .unwrap_or_default(),
    );
    if let Some(description) = death_cause.describe() {
        eprintln!("cang: {description}");
    }
    if status.success() {
        return Ok(ChildStatus::exited(guest_code));
    }
    let observation =
        managed_exit_marker::read_matching_observed_guest_exit(task_state_dir, guest_code);
    if matches!(
        observation,
        managed_exit_marker::ManagedExitObservation::ObservedGuestExit(_)
    ) && managed_helper_status_is_marker_explained(&status, guest_code, &stderr)
    {
        return Ok(ChildStatus::exited(guest_code));
    }
    if replay_stderr {
        replay_stderr_text(stderr.as_bytes());
    }
    let cause = death_cause
        .describe()
        .map(|description| format!("; {description}"))
        .unwrap_or_default();
    match status.code() {
        Some(code) => bail!(
            "managed cang helper exited with status {code} after guest exited with status {guest_code}{cause}"
        ),
        None => {
            bail!(
                "managed cang helper was terminated after guest exited with status {guest_code}{cause}"
            )
        }
    }
}

fn managed_helper_status_is_marker_explained(
    status: &ExitStatus,
    guest_code: i32,
    stderr: &str,
) -> bool {
    if status.code() != Some(1) {
        return false;
    }
    let mut saw_duplicate_worker_failure = false;
    let mut saw_duplicate_helper_failure = false;
    let expected_worker_line = format!(
        "cang internal VM worker: sandboxed cang VM worker child exited with status {guest_code}"
    );
    for line in stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if line == expected_worker_line {
            saw_duplicate_worker_failure = true;
            continue;
        }
        if line == "cang internal: cang VM worker exited with status 1" {
            saw_duplicate_helper_failure = true;
            continue;
        }
        return false;
    }
    saw_duplicate_worker_failure && saw_duplicate_helper_failure
}

fn replay_managed_helper_stderr(logs: Option<&ManagedHelperLogs>) {
    let Some(logs) = logs else {
        return;
    };
    match logs.stderr.read_to_string() {
        Ok(contents) => replay_stderr_text(contents.as_bytes()),
        Err(err) => replay_stderr_text(
            format!("failed to read managed helper stderr log: {err:#}\n").as_bytes(),
        ),
    }
}

fn replay_stderr_text(contents: &[u8]) {
    if contents.is_empty() {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    let _ = write_normalized_stderr(&mut stderr, contents);
}

fn write_normalized_stderr<W: Write>(stderr: &mut W, contents: &[u8]) -> std::io::Result<()> {
    if contents.is_empty() {
        return Ok(());
    }
    stderr.write_all(b"\r\n")?;
    stderr.write_all(contents)?;
    if !contents.ends_with(b"\n") {
        stderr.write_all(b"\n")?;
    }
    stderr.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn render_server_test_env() -> RenderServerEnv {
        RenderServerEnv {
            exec_path: PathBuf::from(
                "/nix/store/vgl-virglrenderer-1.3.0/libexec/virgl_render_server",
            ),
            mesa_lib_dir: PathBuf::from("/nix/store/mesa-mesa-26.1.8/lib"),
            mesa_icd: PathBuf::from(
                "/nix/store/mesa-mesa-26.1.8/share/vulkan/icd.d/radeon_icd.x86_64.json",
            ),
            vulkan_loader_lib_dir: PathBuf::from("/nix/store/vk-vulkan-loader-1.4.341.0/lib"),
            mimalloc_lib: None,
        }
    }

    #[test]
    fn drm_gpu_mode_enables_the_vrend_gbm_layout_for_the_vm_worker() {
        let env = helper_env(
            crate::logging::LogLevel::Info,
            false,
            None,
            Some(&render_server_test_env()),
        );

        assert!(env.contains(&(OsString::from(VREND_GBM_LAYOUT_ENV), OsString::from("1"))));
    }

    #[test]
    fn non_drm_gpu_mode_leaves_the_vrend_gbm_layout_opt_in_alone() {
        let env = helper_env(crate::logging::LogLevel::Info, false, None, None);

        assert!(!env.iter().any(|(name, _)| name == VREND_GBM_LAYOUT_ENV));
    }

    #[test]
    fn normalized_stderr_replay_starts_on_fresh_terminal_line() {
        let mut output = Vec::new();

        write_normalized_stderr(&mut output, b"cang internal: failed").expect("normalized write");

        assert_eq!(output, b"\r\ncang internal: failed\n");
    }

    #[test]
    fn managed_helper_stderr_sink_is_file_backed_for_detached_lifecycle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");

        let status = Command::new("sh")
            .arg("-c")
            .arg("printf 'detached helper diagnostic\\n' >&2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(logs.stderr.spawn_stdio().expect("spawn stdio"))
            .status()
            .expect("run stderr writer");

        assert!(status.success());

        assert_eq!(
            logs.stderr.read_to_string().expect("read log"),
            "detached helper diagnostic\n"
        );
        assert!(
            temp.path().join(MANAGED_HELPER_STDERR_LOG).exists(),
            "detached/preserved task state should retain helper stderr log"
        );
    }

    #[test]
    fn managed_guest_kernel_console_feeds_the_death_diagnosis() {
        let temp = tempfile::tempdir().expect("tempdir");
        let console_path = temp.path().join("guest-kernel-console.log");
        let logs = ManagedHelperLogs::create(temp.path(), &console_path).expect("helper logs");
        std::fs::write(
            &console_path,
            "Out of memory: Killed process 42 (python3) total-vm:5242880kB, anon-rss:4194304kB,\n",
        )
        .expect("kernel console text");

        let cause = guest_death::GuestDeathCause::diagnose(&logs.read_for_diagnosis());

        assert_eq!(
            cause,
            guest_death::GuestDeathCause::OomKilled {
                task: "python3".to_owned(),
                pid: Some(42),
                anon_rss_kib: Some(4_194_304),
                kills: 1,
            }
        );
    }

    #[test]
    fn managed_helper_status_accepts_marker_explained_duplicate_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 130).expect("marker");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");
        std::fs::write(
            &logs.stderr.path,
            "cang internal VM worker: sandboxed cang VM worker child exited with status 130\n\
             cang internal: cang VM worker exited with status 1\n",
        )
        .expect("duplicate stderr text");

        let result = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect("matching marker should explain duplicate helper status");

        assert_eq!(result, ChildStatus::exited(130));
    }

    #[test]
    fn managed_helper_status_rejects_empty_stderr_despite_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 130).expect("marker");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");

        let err = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect_err("empty stderr must not be classified as known duplicate noise");

        assert!(format!("{err:#}").contains("managed cang helper exited with status 1"));
    }

    #[test]
    fn managed_helper_status_rejects_mismatched_duplicate_worker_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 130).expect("marker");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");
        std::fs::write(
            &logs.stderr.path,
            "cang internal VM worker: sandboxed cang VM worker child exited with status 129\n\
             cang internal: cang VM worker exited with status 1\n",
        )
        .expect("duplicate stderr text");

        let err = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect_err("different worker status must not be classified as known duplicate noise");

        assert!(format!("{err:#}").contains("managed cang helper exited with status 1"));
    }

    #[test]
    fn managed_helper_status_rejects_duplicate_worker_line_with_extra_text() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 130).expect("marker");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");
        std::fs::write(
            &logs.stderr.path,
            "cang internal VM worker: sandboxed cang VM worker child exited with status 130: extra\n\
             cang internal: cang VM worker exited with status 1\n",
        )
        .expect("duplicate stderr text");

        let err = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect_err("extra worker stderr text must not be classified as known duplicate noise");

        assert!(format!("{err:#}").contains("managed cang helper exited with status 1"));
    }

    #[test]
    fn managed_helper_status_rejects_cleanup_failure_despite_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        managed_exit_marker::write_observed_guest_exit(temp.path(), 130).expect("marker");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");
        std::fs::write(
            &logs.stderr.path,
            "failed after managed guest exited with status 130: cleanup failure\n",
        )
        .expect("stderr text");

        let err = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect_err("cleanup failure must take precedence");

        assert!(format!("{err:#}").contains("managed cang helper exited with status 1"));
    }

    #[test]
    fn managed_helper_status_rejects_nonzero_without_matching_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let logs =
            ManagedHelperLogs::create(temp.path(), &temp.path().join("guest-kernel-console.log"))
                .expect("helper logs");

        let err = managed_helper_exit_result_with_replay(
            ExitStatus::from_raw(1 << 8),
            130,
            temp.path(),
            Some(&logs),
            false,
        )
        .expect_err("missing marker must not be suppressed");

        assert!(format!("{err:#}").contains("managed cang helper exited with status 1"));
    }
}

fn context_audit_error(
    err: anyhow::Error,
    audit_trace_path: Option<&Path>,
    context: &'static str,
) -> anyhow::Error {
    if audit_trace_path.is_some() {
        err.context(format!("{context}; {}", seccomp::ptrace_failure_hint()))
    } else {
        err.context(context)
    }
}

fn terminate_spawned_child_group(child: &mut Child) {
    let _ = kill_spawned_process_group(child.id(), libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => break,
        }
    }
    let _ = kill_spawned_process_group(child.id(), libc::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

fn start_sigwinch_forwarder(pgid: u32) -> Result<Option<SigwinchForwarder>> {
    if !stdin_is_tty() {
        return Ok(None);
    }
    SigwinchForwarder::start(pgid).map(Some)
}

fn stdin_is_tty() -> bool {
    // Only interactive tty-bound helpers need SIGWINCH rebroadcasting.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

fn kill_spawned_process_group(pgid: u32, signal: i32) -> Result<()> {
    let pgid = i32::try_from(pgid).context("spawned process group id does not fit in i32")?;
    if pgid <= 1 {
        bail!("refusing to signal unsafe spawned process group id {pgid}");
    }
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error())
        .with_context(|| format!("failed to signal spawned helper process group {pgid}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperCommandSpec {
    pub(crate) program: OsString,
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) args: Vec<OsString>,
}

impl HelperCommandSpec {
    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
        command.envs(self.env);
        command
    }
}

fn build_helper_command(
    executable: &Path,
    config_path: &Path,
    config: &LaunchConfig,
    host_profile_enabled: bool,
    managed_ready_fd: Option<i32>,
    render_server_env: Option<&RenderServerEnv>,
) -> Result<HelperCommandSpec> {
    let launcher = KeepIdLauncher::from_current_system()?;
    tracing::debug!(summary = %launcher.diagnostic_summary(), "cang libkrun helper keep-id namespace resolved");
    Ok(build_helper_command_with_launcher(
        executable,
        config_path,
        config.log_level,
        host_profile_enabled,
        managed_ready_fd,
        render_server_env,
        &launcher,
    ))
}

pub(crate) fn build_helper_command_with_launcher(
    executable: &Path,
    config_path: &Path,
    log_level: crate::logging::LogLevel,
    host_profile_enabled: bool,
    managed_ready_fd: Option<i32>,
    render_server_env: Option<&RenderServerEnv>,
    launcher: &KeepIdLauncher,
) -> HelperCommandSpec {
    HelperCommandSpec {
        program: launcher.program(),
        env: helper_env(
            log_level,
            host_profile_enabled,
            managed_ready_fd,
            render_server_env,
        ),
        args: launcher.args(executable, LIBKRUN_ENTER_HELPER_ARG, config_path),
    }
}

fn helper_env(
    log_level: crate::logging::LogLevel,
    host_profile_enabled: bool,
    managed_ready_fd: Option<i32>,
    render_server_env: Option<&RenderServerEnv>,
) -> Vec<(OsString, OsString)> {
    let mut env = vec![(
        OsString::from(INTERNAL_LOG_LEVEL_ENV),
        OsString::from(log_level.as_str()),
    )];
    if host_profile_enabled {
        env.push((OsString::from(CANG_HOST_PROFILE_ENV), OsString::from("1")));
    }
    if let Some(fd) = managed_ready_fd {
        env.push((OsString::from(READY_FD_ENV), OsString::from(fd.to_string())));
    }
    if let Some(_render_server) = render_server_env {
        // The runner child exports its own environment before exec; the helper
        // only needs the parent end of the socketpair, carried by fd number so
        // it survives the helper exec and the VM-worker fork.
        if let Some(fd) = std::env::var_os(crate::runtime::vm::libkrun::RENDER_SERVER_FD_ENV) {
            env.push((
                OsString::from(crate::runtime::vm::libkrun::RENDER_SERVER_FD_ENV),
                fd,
            ));
        }
    }
    if render_server_env.is_some() {
        // The guest's resource-layout queries are answered by the VM worker's
        // in-process vrend, not by the standalone render server, so this opt-in
        // has to reach the helper (and the VM worker forked from it).
        env.push((OsString::from(VREND_GBM_LAYOUT_ENV), OsString::from("1")));
    }
    env
}
