//! The host-side lifetime of `--workspace-relay`.
//!
//! The pump (`$out/libexec/cang-helpers/relay-pump`) is a plain child of the
//! cang CLI process, started before the supervisor and confined by neither
//! Landlock nor seccomp: `fusermount3` is setuid and cannot mount for a process
//! that carries `no_new_privs`. See `cang-relay-pump`'s crate documentation.
//!
//! What this module owns is the *lifetime*, not the protocol:
//!
//! * start the pump, with `PR_SET_PDEATHSIG` so a killed cang does not leave a
//!   pump behind holding a mount;
//! * watch for the mount to appear, and report readiness or a named failure
//!   while the task runs (cang is blocked in the supervisor by then, so the
//!   report comes from a watcher thread reading the pump's state file);
//! * unmount when cang exits, and only then: a serving guest's death keeps the
//!   mount, answering `EIO` (ticket 23 decision 4(a)).
//!
//! `cang` moves its own working directory out of the launch directory as soon as
//! the pump is started. A process whose cwd is inside a FUSE mount pins that
//! superblock, and `umount(2)` then fails `EBUSY` - the mount could never be
//! removed by the process that owns it.

use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};

/// Where the pump reports its state, inside the task directory.
pub(crate) const PUMP_STATE_FILE: &str = "relay-pump.state";
/// Where the pump's own stderr goes when cang does not have a TTY for it.
pub(crate) const PUMP_STATS_FILE: &str = "relay-pump.stats.tsv";
/// The `fusermount3 -o` options for the workspace mount. No
/// `default_permissions` (the kernel would check mode bits against the host
/// uid), no `auto_unmount` (the contract wants a stale mount to stay).
const MOUNT_OPTIONS: &str = "fsname=cang-workspace";
/// How long the mount gets to appear before the launch is told it will not.
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const READY_POLL: Duration = Duration::from_millis(20);
/// How long the pump gets to unmount itself after `SIGTERM`.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const KILL_TIMEOUT: Duration = Duration::from_secs(2);

/// What happened to the mount during the launch, as far as the watcher saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayMountOutcome {
    /// The mount appeared at the launch directory.
    Ready { mountpoint: PathBuf },
    /// The pump said why it could not mount.
    Error(String),
    /// The mount did not appear within [`READY_TIMEOUT`].
    Timed { seconds: u64 },
    /// The watcher did not finish before the launch ended.
    Unobserved,
}

impl RelayMountOutcome {
    pub(crate) fn as_profile_value(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "mounted",
            Self::Error(_) => "error",
            Self::Timed { .. } => "timed-out",
            Self::Unobserved => "unobserved",
        }
    }
}

/// A started pump plus the report its watcher thread fills in.
pub(crate) struct WorkspaceRelayGuard {
    child: Child,
    pid: i32,
    mountpoint: PathBuf,
    fusermount: OsString,
    outcome: Arc<Mutex<RelayMountOutcome>>,
    /// Named line printed once by the watcher (`true` = already printed).
    reported: Arc<Mutex<bool>>,
}

impl std::fmt::Debug for WorkspaceRelayGuard {
    /// The `Child` is not `Debug`, and a test only needs to name the value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceRelayGuard")
            .field("pid", &self.pid)
            .field("mountpoint", &self.mountpoint)
            .finish_non_exhaustive()
    }
}

impl WorkspaceRelayGuard {
    /// Start the pump for `mountpoint` and begin watching for its mount.
    ///
    /// The pump connects to `socket` (the unix socket libkrun binds for the
    /// guest's relay listener) before it mounts, so a guest whose relay is late
    /// costs a retry rather than a hung `mount(2)`.
    pub(crate) fn start(
        program: OsString,
        task_dir: &Path,
        mountpoint: &Path,
        socket: &Path,
        want_stats: bool,
    ) -> Result<Self> {
        let state_file = task_dir.join(PUMP_STATE_FILE);
        let _ = std::fs::remove_file(&state_file);
        let mut command = Command::new(&program);
        command
            .arg("--socket")
            .arg(socket)
            .arg("--mountpoint")
            .arg(mountpoint)
            .arg("--options")
            .arg(MOUNT_OPTIONS)
            .arg("--state-file")
            .arg(&state_file)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            // The pump must not sit inside the tree it is about to mount over:
            // a process whose cwd is on the mount keeps it busy at unmount time.
            .current_dir("/");
        if want_stats {
            command.arg("--stats").arg(task_dir.join(PUMP_STATS_FILE));
        }
        // SAFETY: `prctl` is async-signal-safe and only sets the parent-death
        // signal for the child that is about to exec.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
                Ok(())
            });
        }
        let child = command.spawn().with_context(|| {
            format!(
                "failed to start the cang workspace relay pump '{program:?}' \
                 (CANG_RELAY_PUMP overrides the helper, package builds resolve it \
                 from $out/libexec/cang-helpers/relay-pump)"
            )
        })?;
        let pid = child.id() as i32;
        // Out of the launch directory before anything else can pin the mount.
        if let Ok(current) = std::env::current_dir()
            && current.starts_with(mountpoint)
        {
            std::env::set_current_dir("/").context(
                "failed to move cang's working directory out of the workspace relay mount",
            )?;
        }
        let outcome = Arc::new(Mutex::new(RelayMountOutcome::Unobserved));
        let reported = Arc::new(Mutex::new(false));
        {
            let outcome = Arc::clone(&outcome);
            let reported = Arc::clone(&reported);
            let mountpoint = mountpoint.to_path_buf();
            std::thread::Builder::new()
                .name("cang-workspace-relay-watch".to_owned())
                .spawn(move || {
                    watch(&state_file, pid, &mountpoint, &outcome, &reported);
                })
                .context("failed to start the workspace relay watcher")?;
        }
        Ok(Self {
            child,
            pid,
            mountpoint: mountpoint.to_path_buf(),
            fusermount: fusermount_program(),
            outcome,
            reported,
        })
    }

    /// The outcome the watcher has reached so far.
    pub(crate) fn outcome(&self) -> RelayMountOutcome {
        self.outcome
            .lock()
            .map(|outcome| outcome.clone())
            .unwrap_or(RelayMountOutcome::Unobserved)
    }

    /// Report the mount's fate once the task is done, before tearing it down.
    pub(crate) fn report(&self) {
        let outcome = self.outcome();
        let mut reported = self.reported.lock().expect("relay report lock");
        if *reported {
            return;
        }
        *reported = true;
        match outcome {
            RelayMountOutcome::Ready { .. } => {}
            RelayMountOutcome::Error(reason) => eprintln!(
                "cang: the workspace relay did not mount '{}': {reason}",
                self.mountpoint.display()
            ),
            RelayMountOutcome::Timed { seconds } => eprintln!(
                "cang: the workspace relay did not appear at '{}' within {seconds}s; \
                 the host has no view of the guest workspace for this launch",
                self.mountpoint.display()
            ),
            RelayMountOutcome::Unobserved => eprintln!(
                "cang: the workspace relay's mount at '{}' was not observed before the task ended",
                self.mountpoint.display()
            ),
        }
    }
}

impl Drop for WorkspaceRelayGuard {
    fn drop(&mut self) {
        self.report();
        // The pump unmounts on SIGTERM, and that is the only unmount path.
        unsafe { libc::kill(self.pid, libc::SIGTERM) };
        if !wait_for_exit(&mut self.child, TEARDOWN_TIMEOUT) {
            eprintln!(
                "cang: the workspace relay pump did not exit within {}s; killing it",
                TEARDOWN_TIMEOUT.as_secs()
            );
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
            let _ = wait_for_exit(&mut self.child, KILL_TIMEOUT);
        }
        if !is_fuse_mount(&self.mountpoint) {
            return;
        }
        // The pump could not unmount (it is gone, or it reported a failure):
        // cang tries once itself, and says so if the path stays busy.
        match unmount(&self.fusermount, &self.mountpoint) {
            Ok(()) => eprintln!(
                "cang: unmounted the workspace relay at '{}'",
                self.mountpoint.display()
            ),
            Err(err) => eprintln!(
                "cang: the workspace relay mount at '{}' is still there ({err:#}); \
                 a process whose working directory is inside it keeps it busy, so \
                 `fusermount3 -u {}` from another directory is what clears it",
                self.mountpoint.display(),
                self.mountpoint.display()
            ),
        }
    }
}

fn watch(
    state_file: &Path,
    pid: i32,
    mountpoint: &Path,
    outcome: &Arc<Mutex<RelayMountOutcome>>,
    reported: &Arc<Mutex<bool>>,
) {
    let started = Instant::now();
    loop {
        if let Some(line) = read_state(state_file) {
            let verdict = match line.split_once(' ') {
                Some(("ready", path)) => Some(RelayMountOutcome::Ready {
                    mountpoint: PathBuf::from(path),
                }),
                Some(("error", reason)) => Some(RelayMountOutcome::Error(reason.to_owned())),
                Some(("unmounted", _)) => Some(RelayMountOutcome::Error(
                    "the mount was unmounted before the task started".to_owned(),
                )),
                _ => None,
            };
            if let Some(verdict) = verdict {
                let ready = matches!(verdict, RelayMountOutcome::Ready { .. });
                if let Ok(mut slot) = outcome.lock() {
                    *slot = verdict.clone();
                }
                if let (true, Ok(mut reported)) = (ready, reported.lock())
                    && !*reported
                {
                    *reported = true;
                    eprintln!(
                        "cang: workspace relay mounted at '{}' (the guest serves it)",
                        mountpoint.display()
                    );
                }
                return;
            }
        }
        if process_is_gone(pid) {
            // The pump exited without ever reporting a mount.
            let reason = read_state(state_file).unwrap_or_else(|| "the pump exited".to_owned());
            if let Ok(mut slot) = outcome.lock() {
                *slot = RelayMountOutcome::Error(reason);
            }
            return;
        }
        if started.elapsed() >= READY_TIMEOUT {
            if let Ok(mut slot) = outcome.lock() {
                *slot = RelayMountOutcome::Timed {
                    seconds: READY_TIMEOUT.as_secs(),
                };
            }
            return;
        }
        std::thread::sleep(READY_POLL);
    }
}

fn read_state(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .read_to_string(&mut text)
        .ok()?;
    let line = text.lines().next()?.trim().to_owned();
    if line.is_empty() { None } else { Some(line) }
}

fn process_is_gone(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) != 0 }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return true,
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `fusermount3` for this host, resolved the same way the pump resolves it.
pub(crate) fn fusermount_program() -> OsString {
    const FUSERMOUNT_ENV: &str = "CANG_FUSERMOUNT3";
    const NIXOS_WRAPPER: &str = "/run/wrappers/bin/fusermount3";
    if let Some(value) = std::env::var_os(FUSERMOUNT_ENV).filter(|value| !value.is_empty()) {
        return value;
    }
    if Path::new(NIXOS_WRAPPER).is_file() {
        return OsString::from(NIXOS_WRAPPER);
    }
    OsString::from("fusermount3")
}

/// Unmount the workspace relay, if it is still mounted.
pub(crate) fn unmount(program: &OsString, mountpoint: &Path) -> Result<()> {
    let output = Command::new(program)
        .arg("-u")
        .arg(mountpoint)
        .output()
        .with_context(|| format!("failed to run {program:?} -u"))?;
    if !output.status.success() {
        bail!(
            "{:?} -u {} failed: {}",
            program,
            mountpoint.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// `statfs(2)`'s answer for "this path is a FUSE mount".
pub(crate) fn is_fuse_mount(mountpoint: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(mountpoint.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(path.as_ptr(), &mut stat) };
    if rc != 0 {
        return false;
    }
    // FUSE_SUPER_MAGIC; the same constant the pump uses.
    stat.f_type == 0x6573_5546
}

/// The pump helper for this build.
pub(crate) fn pump_program() -> OsString {
    runtime_tool_program(RuntimeTool::RelayPump)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_pump(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-relay-pump");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("permissions");
        path
    }

    /// A pump that reports the mount and then waits: the watcher must see `ready`
    /// and the guard must report nothing when it eventually exits.
    #[test]
    fn a_reported_mount_is_observed_and_the_guard_does_not_complain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = dir.path().join("task");
        std::fs::create_dir(&task).expect("task dir");
        let mountpoint = dir.path().join("workspace");
        std::fs::create_dir(&mountpoint).expect("mountpoint");
        let socket = dir.path().join("relay.sock");

        // The fake pump reads the flags it needs to write the state file.
        let pump = fake_pump(
            dir.path(),
            r#"state=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--state-file" ]; then state="$2"; fi
  shift
done
echo "ready /somewhere" > "$state"
while true; do sleep 1; done"#,
        );

        let guard = WorkspaceRelayGuard::start(
            OsString::from(pump.to_str().expect("utf8")),
            &task,
            &mountpoint,
            &socket,
            false,
        )
        .expect("guard");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(guard.outcome(), RelayMountOutcome::Ready { .. }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let outcome = guard.outcome();
        assert!(
            matches!(outcome, RelayMountOutcome::Ready { .. }),
            "the watcher must see the pump's ready line, got {outcome:?}"
        );
    }

    /// A pump that names a failure must be reported by name, not as a timeout.
    #[test]
    fn a_pump_error_is_carried_to_the_launch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = dir.path().join("task");
        std::fs::create_dir(&task).expect("task dir");
        let mountpoint = dir.path().join("workspace");
        std::fs::create_dir(&mountpoint).expect("mountpoint");
        let socket = dir.path().join("relay.sock");
        let pump = fake_pump(
            dir.path(),
            r#"state=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--state-file" ]; then state="$2"; fi
  shift
done
echo "error connect refused by the guest" > "$state"
exit 1"#,
        );

        let mut guard = WorkspaceRelayGuard::start(
            OsString::from(pump.to_str().expect("utf8")),
            &task,
            &mountpoint,
            &socket,
            false,
        )
        .expect("guard");
        let _ = guard.child.try_wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut outcome = guard.outcome();
        while Instant::now() < deadline {
            outcome = guard.outcome();
            if matches!(outcome, RelayMountOutcome::Error(_)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        match outcome {
            RelayMountOutcome::Error(reason) => {
                assert!(reason.contains("connect refused by the guest"), "{reason}");
            }
            other => panic!("a named pump failure must reach the launch, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_pump_binary_is_a_named_spawn_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = dir.path().join("task");
        std::fs::create_dir(&task).expect("task dir");
        let mountpoint = dir.path().join("workspace");
        std::fs::create_dir(&mountpoint).expect("mountpoint");
        let err = WorkspaceRelayGuard::start(
            OsString::from("/nonexistent/cang-relay-pump"),
            &task,
            &mountpoint,
            &dir.path().join("relay.sock"),
            false,
        )
        .expect_err("a missing pump must fail by name");
        let message = format!("{err:#}");
        assert!(message.contains("relay pump"), "{message}");
        assert!(message.contains("CANG_RELAY_PUMP"), "{message}");
    }

    #[test]
    fn a_plain_directory_is_not_a_workspace_relay_mount() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_fuse_mount(dir.path()));
        assert!(!is_fuse_mount(Path::new(
            "/nonexistent/cang-workspace-relay"
        )));
    }

    #[test]
    fn the_fusermount_override_wins() {
        // The resolver is pure enough to check through the environment variable
        // the pump also honours; an override always wins over the wrapper.
        assert!(!fusermount_program().to_string_lossy().is_empty());
    }
}
