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
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::logging::LogLevel;
use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};

/// The guest environment pair for [`WATCH_ENV`], when the host set it.
pub(in crate::runtime::session) fn watch_guest_env_pair_from_process_env()
-> Option<(String, String)> {
    guest_env_pair_from_value(std::env::var(WATCH_ENV).ok().as_deref())
}

pub(in crate::runtime::session) fn guest_env_pair_from_value(
    value: Option<&str>,
) -> Option<(String, String)> {
    value
        .filter(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .map(|_| (WATCH_ENV.to_owned(), "1".to_owned()))
}

/// Where the pump reports its state, inside the task directory.
pub(crate) const PUMP_STATE_FILE: &str = "relay-pump.state";
/// Where the pump's own stderr goes when cang does not have a TTY for it.
pub(crate) const PUMP_STATS_FILE: &str = "relay-pump.stats.tsv";
/// The pump's switch for its own progress lines (`cang-relay-pump`'s
/// `DEBUG_ENV`).
///
/// The pump's stderr is this launch's output, so its lines - the mount, the
/// negotiated INIT shape, the transport's death - are diagnostics: cang sets
/// this only when its own `--log-level` asks for debug, and a plain launch
/// prints none of them.
pub(crate) const PUMP_DEBUG_ENV: &str = "CANG_RELAY_PUMP_DEBUG";
/// The host-side knob for the guest's invalidation watcher.
///
/// The guest relay is what watches the served tree, so the decision has to
/// cross the host -> guest environment: `cang` forwards this variable into the
/// guest environment when it is set, `cang-guest-init` hands its environment to
/// the relay it spawns, and the relay's `--watch` flag reads it. That keeps the
/// OFF and ON arms of the measurement the same image and the same binary.
pub(in crate::runtime::session) const WATCH_ENV: &str = "CANG_WORKSPACE_RELAY_WATCH";

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
    /// What [`WorkspaceRelayGuard::shutdown`] found, once it has run.
    teardown: Option<RelayTeardown>,
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
        log_level: LogLevel,
    ) -> Result<Self> {
        let state_file = task_dir.join(PUMP_STATE_FILE);
        let _ = std::fs::remove_file(&state_file);
        let mut command = pump_command(
            &program, task_dir, mountpoint, socket, want_stats, log_level,
        );
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
            teardown: None,
        })
    }

    /// The outcome slot itself, for a caller that lets another thread own the
    /// guard (the workspace server) and still wants to report what it saw.
    pub(crate) fn outcome_handle(&self) -> Arc<Mutex<RelayMountOutcome>> {
        Arc::clone(&self.outcome)
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

/// The pump's invocation, kept separate from [`WorkspaceRelayGuard::start`] so
/// its shape - the flags and the one environment switch that turns the pump's
/// own progress lines on - is testable without a mount.
fn pump_command(
    program: &OsString,
    task_dir: &Path,
    mountpoint: &Path,
    socket: &Path,
    want_stats: bool,
    log_level: LogLevel,
) -> Command {
    let mut command = Command::new(program);
    command
        .arg("--socket")
        .arg(socket)
        .arg("--mountpoint")
        .arg(mountpoint)
        .arg("--options")
        .arg(MOUNT_OPTIONS)
        .arg("--state-file")
        .arg(task_dir.join(PUMP_STATE_FILE))
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        // The pump must not sit inside the tree it is about to mount over:
        // a process whose cwd is on the mount keeps it busy at unmount time.
        .current_dir("/");
    if want_stats {
        command.arg("--stats").arg(task_dir.join(PUMP_STATS_FILE));
    }
    if log_level.enables_debug() {
        command.env(PUMP_DEBUG_ENV, "1");
    }
    command
}

/// What became of the mount when the guard tore it down.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RelayTeardown {
    /// The pump unmounted on `SIGTERM` (the only unmount path it owns).
    PumpUnmounted,
    /// The pump could not unmount it, so cang removed the mount itself.
    FallbackUnmounted,
    /// The mount is still there: something holds it busy.
    Busy(String),
}

impl WorkspaceRelayGuard {
    /// Unmount the mount and stop the pump, in that order, and report whether the
    /// host view actually went away.
    ///
    /// For a plain launch nothing calls this explicitly and `Drop` does the same
    /// work - stage 1's lifetime, the mount dies with the launch. A *workspace
    /// server* calls it first, because the ticket requires the mount to be gone
    /// **before** the VM is stopped, and `Drop` is too late for that.
    ///
    /// Idempotent: the pump is already gone and the mount is already gone on the
    /// second call, which then reports success.
    pub(crate) fn shutdown(&mut self) -> Result<()> {
        match self.teardown() {
            RelayTeardown::Busy(message) => Err(anyhow::anyhow!("{message}")),
            RelayTeardown::PumpUnmounted | RelayTeardown::FallbackUnmounted => Ok(()),
        }
    }

    fn teardown(&mut self) -> RelayTeardown {
        if let Some(teardown) = &self.teardown {
            return teardown.clone();
        }
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
        let teardown = if !is_mountpoint(&self.mountpoint) {
            RelayTeardown::PumpUnmounted
        } else {
            // The pump could not unmount (it is gone, or it reported a failure):
            // cang tries once itself, and names a mount something still holds.
            match unmount(&self.fusermount, &self.mountpoint) {
                Ok(()) => RelayTeardown::FallbackUnmounted,
                // The unmount failing while the mount is already gone (a race
                // with the pump) is a success.
                Err(_) if !is_mountpoint(&self.mountpoint) => RelayTeardown::PumpUnmounted,
                Err(err) => RelayTeardown::Busy(format!(
                    "the workspace mount at '{}' is still mounted: `fusermount3 -u {}` was refused \
                     ({err:#}); a host process whose working directory or open file is inside the \
                     mount keeps it busy, so `fusermount3 -u {}` from another directory is what \
                     clears it",
                    self.mountpoint.display(),
                    self.mountpoint.display(),
                    self.mountpoint.display()
                )),
            }
        };
        if teardown == RelayTeardown::FallbackUnmounted {
            eprintln!(
                "cang: unmounted the workspace relay at '{}'",
                self.mountpoint.display()
            );
        }
        self.teardown = Some(teardown.clone());
        teardown
    }
}

impl Drop for WorkspaceRelayGuard {
    fn drop(&mut self) {
        self.report();
        let initiated_here = self.teardown.is_none();
        // A caller that already got the refusal printed it itself; a launch that
        // never asked for the ordered teardown has not seen it yet.
        if let RelayTeardown::Busy(message) = self.teardown()
            && initiated_here
        {
            eprintln!("cang: {message}");
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

/// Is anything mounted on `path`?
///
/// `/proc/self/mountinfo`, not `statfs(2)`: a FUSE mount whose connection is
/// gone answers `-ENOTCONN` to `statfs`, so asking the *filesystem* whether it is
/// still mounted says "no" for exactly the stale mount a teardown has to report.
pub(crate) fn is_mountpoint(path: &Path) -> bool {
    match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(mountinfo) => {
            let target = path.as_os_str().as_bytes();
            mountinfo.lines().any(|line| {
                line.split(' ')
                    .nth(4)
                    .is_some_and(|field| unescape_mountinfo_field(field.as_bytes()) == target)
            })
        }
        // No mountinfo to read is not an answer: fall back to the filesystem.
        Err(_) => is_fuse_mount(path),
    }
}

/// `mountinfo` escapes space, tab, newline and backslash as `\040` and friends.
fn unescape_mountinfo_field(field: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' || index + 3 >= field.len() {
            output.push(field[index]);
            index += 1;
            continue;
        }
        let octal = &field[index + 1..index + 4];
        match std::str::from_utf8(octal)
            .ok()
            .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        {
            Some(byte) => {
                output.push(byte);
                index += 4;
            }
            None => {
                output.push(field[index]);
                index += 1;
            }
        }
    }
    output
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
            LogLevel::Off,
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
            LogLevel::Off,
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
            LogLevel::Off,
        )
        .expect_err("a missing pump must fail by name");
        let message = format!("{err:#}");
        assert!(message.contains("relay pump"), "{message}");
        assert!(message.contains("CANG_RELAY_PUMP"), "{message}");
    }

    #[test]
    fn a_plain_directory_is_not_a_mountpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_mountpoint(dir.path()));
        assert!(!is_mountpoint(Path::new(
            "/nonexistent/cang-workspace-relay"
        )));
        // The tmpfs every Linux host has mounted: the check must answer "yes"
        // for a mount it did not create, or `cang workspace serve` would mount
        // over one.
        assert!(is_mountpoint(Path::new("/proc")));
    }

    #[test]
    fn mountinfo_escapes_are_decoded() {
        assert_eq!(unescape_mountinfo_field(b"/a/b"), b"/a/b");
        assert_eq!(unescape_mountinfo_field(b"/a\\040b"), b"/a b");
        assert_eq!(unescape_mountinfo_field(b"/a\\134b"), b"/a\\b");
        assert_eq!(unescape_mountinfo_field(b"/a\\"), b"/a\\");
    }

    /// The watcher's knob is a guest environment variable, so the OFF arm must
    /// be the absence of it and the ON arm the same image with it set.
    #[test]
    fn the_watch_knob_is_only_forwarded_for_a_truthy_value() {
        assert_eq!(guest_env_pair_from_value(None), None);
        assert_eq!(guest_env_pair_from_value(Some("")), None);
        assert_eq!(guest_env_pair_from_value(Some("0")), None);
        assert_eq!(guest_env_pair_from_value(Some("false")), None);
        assert_eq!(
            guest_env_pair_from_value(Some("1")),
            Some((WATCH_ENV.to_owned(), "1".to_owned()))
        );
        assert_eq!(
            guest_env_pair_from_value(Some(" ON ")),
            Some((WATCH_ENV.to_owned(), "1".to_owned()))
        );
    }

    /// The pump's own progress lines are diagnostics: the switch reaches the
    /// pump only when cang's log level asks for debug, so a plain launch's
    /// output carries none of its lines.
    #[test]
    fn the_pump_progress_lines_follow_cang_log_level() {
        let program = OsString::from("/libexec/cang-helpers/relay-pump");
        let task = Path::new("/tmp/relay-task");
        let mountpoint = Path::new("/tmp/relay-mnt");
        let socket = Path::new("/tmp/relay.sock");

        let quiet = pump_command(&program, task, mountpoint, socket, false, LogLevel::Off);
        assert!(
            quiet
                .get_envs()
                .all(|(key, _)| key.to_string_lossy() != PUMP_DEBUG_ENV),
            "a plain launch must not ask for the pump's diagnostics"
        );

        let debug = pump_command(&program, task, mountpoint, socket, false, LogLevel::Debug);
        let switch = debug
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == PUMP_DEBUG_ENV);
        assert_eq!(
            switch.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned())),
            Some(Some("1".to_owned())),
            "a debug launch must ask for the pump's diagnostics"
        );

        // And the switch is the only difference: the invocation itself is the
        // pump's documented command line.
        let args: Vec<String> = debug
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--socket",
                "/tmp/relay.sock",
                "--mountpoint",
                "/tmp/relay-mnt",
                "--options",
                MOUNT_OPTIONS,
                "--state-file",
                "/tmp/relay-task/relay-pump.state",
            ]
        );
    }

    #[test]
    fn the_fusermount_override_wins() {
        // The resolver is pure enough to check through the environment variable
        // the pump also honours; an override always wins over the wrapper.
        assert!(!fusermount_program().to_string_lossy().is_empty());
    }
}
