//! `cang workspace serve`: the long-lived workspace server VM (ticket 29 stage 2).
//!
//! Ticket 23 settled the contract: the host always sees the workspace *image*,
//! served by whichever guest owns it - the task VM while one runs, and a
//! long-lived workspace server in between. Stage 1 built the relay
//! (`--workspace-relay`), whose mount lived exactly as long as one launch; this
//! module is the server's lifetime: one VM serving the disk, one host mount for
//! as long as that VM lives, and the bookkeeping a second `cang` needs in order
//! to (a) refuse by name when it may not touch the image and (b) stop the
//! server.
//!
//! Three things live here, in the order a reader needs them:
//!
//! 1. [`WorkspaceLease`] - the single-writer rule, a `flock` keyed on the disk
//!    path, shaped like [`ImageLock`](crate::runtime::launch::components::persistent_disks::nix_lower).
//!    The server takes it *exclusive* for its lifetime; every task launch that
//!    names the same `--workspace-disk` takes it *shared* and refuses by name
//!    while the server holds it. Never an automatic stop: turning "start a
//!    build" into "kill the session someone is editing in" is not cang's call.
//! 2. [`WorkspaceServerRecord`] - the state file `cang workspace stop` and
//!    `cang workspace status` read: which process is serving, which task and
//!    mountpoint it owns.
//! 3. [`WorkspaceServerMonitor`] - the thread that owns the mount's lifetime
//!    while the main thread sits in the supervisor, and that enforces the
//!    ordering the ticket requires: **flush the guest, unmount, then stop the
//!    VM**.
//!
//! # Why the lease and the state file are not literally beside the image
//!
//! The ticket suggested a lease file "beside the image". They live in
//! `/tmp/cang-<uid>/workspace-server/` instead, keyed by the *canonical* disk
//! path, for two reasons that only show up once the mount is up: the mount lands
//! on the host directory, so a file beside the image is **unreachable** from the
//! very directory `cang workspace stop` has to be run in (`cang workspace serve
//! work.raw` puts the image there), and a block device's directory (`/dev`) is
//! not writable at all. The key is still the disk path and the lock is still a
//! `flock`: only the location moved.
//!
//! # Why the server has no managed attach session
//!
//! A managed attach puts the host terminal into raw mode (see
//! `session::attach`), so `^C` becomes a byte delivered to the guest PTY instead
//! of a host `SIGINT`: the guest payload would die first, and the VM would stop
//! *before* the mount was unmounted - the opposite of the required ordering. The
//! server therefore runs its guest payload (`sleep infinity`) directly (the
//! guest-init path for a launch without `CANG_SESSION_MANAGED`) and keeps the
//! exec socket, which is what `cang exec` needs. `cang ps` and `cang kill` read
//! the same active-task record they already use.

use anyhow::{Context, Result, anyhow, bail};
use std::fs::{self, File, OpenOptions};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::runtime::launch::components::persistent_disks::workspace::canonical_disk_path;
use crate::runtime::session::task_control::{ActiveTaskStatus, ProcessInspector};
use crate::runtime::session::task_control::{ProcessIdentity, ProcfsInspector};
use crate::runtime::session::workspace_relay::{
    RelayMountOutcome, WorkspaceRelayGuard, is_mountpoint,
};

/// The subdirectory of cang's per-user runtime directory that holds the
/// workspace server's lease and state files.
const RUNTIME_SUBDIR: &str = "workspace-server";
const RECORD_VERSION: &str = "1";
/// How long the relay's mount gets to appear before the server gives up and
/// names the reason. Slack over the relay's own 90 s readiness timeout.
const READY_DEADLINE: Duration = Duration::from_secs(120);
/// How long `cang workspace stop` waits for the server process to exit.
const STOP_TIMEOUT: Duration = Duration::from_secs(60);
/// How long it then waits for the mount to disappear (the unmount happens
/// before the process exits, so this is a confirmation, not a wait).
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the monitor waits for the launched helper's active-task record.
const HELPER_APPEARS_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the VM gets to stop after `SIGTERM`, before `SIGKILL`.
const VM_STOP_GRACE: Duration = Duration::from_secs(10);
/// How long the guest gets to flush its filesystem before the VM is stopped.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(60);
/// The guest command that commits the workspace filesystem. Run *inside* the
/// guest, through the exec channel: a host-side `sync -f <mount>` returns 0 and
/// flushes nothing (measured), while the guest's own kernel is the only one that
/// can commit the btrfs tree the relay writes into.
const GUEST_FLUSH_COMMAND: [&str; 3] = ["sync", "-f", "/workspace"];
const KILL_TIMEOUT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(50);

/// Which side of the single-writer rule a process is taking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseRole {
    /// `cang workspace serve`: exclusive, for the server's lifetime.
    Server,
    /// A `--workspace-disk` task launch: shared, for the launch's lifetime.
    Task,
}

/// The host's single-writer lease on one workspace image.
///
/// An open file description holding a `flock`; dropping it releases the lock
/// (and so does the process dying, so a killed cang never leaves a stuck lease).
#[derive(Debug)]
pub(crate) struct WorkspaceLease {
    /// Held for the lock's lifetime; `File` has no other use here.
    _file: File,
    path: PathBuf,
}

impl Drop for WorkspaceLease {
    fn drop(&mut self) {
        // `flock` is released by closing the descriptor; the line makes a
        // concurrent refusal's "held by another process" traceable to its cause.
        tracing::debug!(lease = %self.path.display(), "released the workspace image lease");
    }
}

impl WorkspaceLease {
    /// Take the lease a workspace server must hold, refusing by name when
    /// another process is using the image.
    pub(crate) fn acquire_server(disk: &Path) -> Result<Self> {
        Self::acquire_server_at(&lease_path(disk), disk)
    }

    fn acquire_server_at(path: &Path, disk: &Path) -> Result<Self> {
        match lock(path, LeaseRole::Server)? {
            Some(file) => {
                tracing::debug!(lease = %path.display(), "took the exclusive workspace image lease");
                Ok(Self {
                    _file: file,
                    path: path.to_path_buf(),
                })
            }
            None => bail!(
                "another cang process is using the workspace image '{}': a workspace server and a \
                 --workspace-disk task launch may not share one image - stop the workspace server \
                 with 'cang workspace stop {}', or wait for the task launch to finish (lease '{}')",
                disk.display(),
                disk.display(),
                path.display()
            ),
        }
    }

    /// Take the shared lease a task launch holds while it has the image
    /// attached.
    ///
    /// Best effort for two reasons, both about not breaking launches that
    /// worked before this lease existed: a lease file that cannot be created
    /// (a read-only runtime directory, a `HOME`-less account) is skipped rather
    /// than failing the launch, and an image leased by a *task* shares with
    /// other tasks exactly as it always did. The one case that refuses is the
    /// one the ticket is about: a workspace server holding it exclusively.
    pub(crate) fn acquire_task(disk: &Path) -> Result<Option<Self>> {
        Self::acquire_task_at(&lease_path(disk), disk)
    }

    fn acquire_task_at(path: &Path, disk: &Path) -> Result<Option<Self>> {
        let file = match lock(path, LeaseRole::Task) {
            Ok(Some(file)) => file,
            Ok(None) => bail!(
                "the workspace image '{}' is being served by a cang workspace server, and only \
                 one guest may mount that image at a time: stop it first with 'cang workspace \
                 stop {}' and launch again",
                disk.display(),
                disk.display()
            ),
            Err(err) => {
                tracing::debug!(
                    lease = %path.display(),
                    error = %err,
                    "workspace image lease unavailable; continuing without it"
                );
                return Ok(None);
            }
        };
        tracing::debug!(lease = %path.display(), "took the shared workspace image lease");
        Ok(Some(Self {
            _file: file,
            path: path.to_path_buf(),
        }))
    }
}

/// `flock(2)` the lease file, `Ok(None)` when another process holds it.
fn lock(path: &Path, role: LeaseRole) -> Result<Option<File>> {
    create_private_parents(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open the workspace lease '{}'", path.display()))?;
    let operation = match role {
        LeaseRole::Server => libc::LOCK_EX,
        LeaseRole::Task => libc::LOCK_SH,
    } | libc::LOCK_NB;
    // SAFETY: `flock` takes a valid fd and a lock operation; a failure is
    // reported by the return value, not by memory.
    let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), operation) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(None);
        }
        return Err(err)
            .with_context(|| format!("failed to lock the workspace lease '{}'", path.display()));
    }
    Ok(Some(file))
}

/// The lease file for one image, under cang's per-user runtime directory.
pub(crate) fn lease_path(disk: &Path) -> PathBuf {
    runtime_subdir().join(format!("{}.lease", disk_key(disk)))
}

/// The server's state file for one image.
pub(crate) fn record_path(disk: &Path) -> PathBuf {
    runtime_subdir().join(format!("{}.server", disk_key(disk)))
}

/// Create the runtime directory a lease or state file lives in, owner-only.
///
/// A lease taken before cang allocates any socket must not be what leaves
/// `/tmp/cang-<uid>` readable to other users: the same directory holds the
/// attach sockets, and `managed_attach_socket` repairs its mode when it creates
/// them - which has not happened yet on the `cang workspace stop` path at all.
fn create_private_parents(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for dir in parent.ancestors().take(2) {
        fs::create_dir_all(dir).with_context(|| format!("failed to create '{}'", dir.display()))?;
        if let Ok(metadata) = fs::metadata(dir)
            && metadata.permissions().mode() & 0o077 != 0
        {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o700);
            let _ = fs::set_permissions(dir, permissions);
        }
    }
    Ok(())
}

fn runtime_subdir() -> PathBuf {
    // The same per-user runtime directory cang's attach sockets use
    // (`managed_attach_socket`): created 0700 by cang and validated owner-only,
    // so another user cannot pre-create a lease or state file in it.
    Path::new("/tmp")
        .join(format!("cang-{}", current_uid()))
        .join(RUNTIME_SUBDIR)
}

/// A stable key for one image: `flock` is per path, so two spellings of the same
/// image (relative, through a symlinked parent) must collapse to one key.
fn disk_key(disk: &Path) -> String {
    let canonical = canonical_disk_path(disk);
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in canonical.as_os_str().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// What `cang workspace status` and `cang workspace stop` read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceServerRecord {
    pub(crate) disk: PathBuf,
    pub(crate) mountpoint: PathBuf,
    pub(crate) task_id: String,
    pub(crate) task_dir: PathBuf,
    pub(crate) started_at_unix_secs: u64,
    /// The server's own process, so a reused pid is never signalled.
    pub(crate) process: ProcessIdentity,
}

impl WorkspaceServerRecord {
    pub(crate) fn new(
        disk: &Path,
        mountpoint: &Path,
        task_id: &str,
        task_dir: &Path,
        process: ProcessIdentity,
    ) -> Result<Self> {
        Ok(Self {
            disk: disk.to_path_buf(),
            mountpoint: mountpoint.to_path_buf(),
            task_id: task_id.to_owned(),
            task_dir: task_dir.to_path_buf(),
            started_at_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before unix epoch")?
                .as_secs(),
            process,
        })
    }

    fn write(&self, path: &Path) -> Result<()> {
        create_private_parents(path)?;
        fs::write(path, encode_record(self))
            .with_context(|| format!("failed to write '{}'", path.display()))
    }
}

fn encode_record(record: &WorkspaceServerRecord) -> String {
    let mut output = String::new();
    let mut push = |key: &str, value: &str| {
        output.push_str(key);
        output.push('=');
        output.push_str(&value.replace('\n', " "));
        output.push('\n');
    };
    push("version", RECORD_VERSION);
    push("disk", &record.disk.display().to_string());
    push("mountpoint", &record.mountpoint.display().to_string());
    push("task_id", &record.task_id);
    push("task_dir", &record.task_dir.display().to_string());
    push(
        "started_at_unix_secs",
        &record.started_at_unix_secs.to_string(),
    );
    push("pid", &record.process.pid.to_string());
    push("pgid", &record.process.pgid.to_string());
    push("sid", &record.process.sid.to_string());
    push(
        "proc_start_time_ticks",
        &record.process.proc_start_time_ticks.to_string(),
    );
    push("boot_id", &record.process.boot_id);
    output
}

fn decode_record(contents: &str) -> Result<WorkspaceServerRecord> {
    let mut fields = std::collections::BTreeMap::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("workspace server record line is missing '=': {line}");
        };
        fields.insert(key.to_owned(), value.to_owned());
    }
    if required(&fields, "version")? != RECORD_VERSION {
        bail!("unsupported workspace server record version");
    }
    Ok(WorkspaceServerRecord {
        disk: PathBuf::from(required(&fields, "disk")?),
        mountpoint: PathBuf::from(required(&fields, "mountpoint")?),
        task_id: required(&fields, "task_id")?.to_owned(),
        task_dir: PathBuf::from(required(&fields, "task_dir")?),
        started_at_unix_secs: parse_required(&fields, "started_at_unix_secs")?,
        process: ProcessIdentity {
            pid: parse_required(&fields, "pid")?,
            pgid: parse_required(&fields, "pgid")?,
            sid: parse_required(&fields, "sid")?,
            proc_start_time_ticks: parse_required(&fields, "proc_start_time_ticks")?,
            boot_id: required(&fields, "boot_id")?.to_owned(),
        },
    })
}

fn required<'a>(
    fields: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str> {
    fields
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("workspace server record is missing '{key}'"))
}

fn parse_required<T>(fields: &std::collections::BTreeMap<String, String>, key: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    required(fields, key)?
        .parse::<T>()
        .with_context(|| format!("workspace server record field '{key}' has invalid value"))
}

fn read_record(path: &Path) -> Result<Option<WorkspaceServerRecord>> {
    match fs::read_to_string(path) {
        Ok(contents) => decode_record(&contents)
            .map(Some)
            .with_context(|| format!("failed to parse '{}'", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

/// A `SIGINT`/`SIGTERM`/`SIGHUP` request for an ordered shutdown.
///
/// The handler only records the request and writes one byte to a pipe; the
/// monitor thread polls that pipe. `SA_RESTART` is set so the forwarded signals
/// do not turn another thread's ordinary read into `EINTR`.
pub(crate) struct ShutdownSignals {
    read_fd: libc::c_int,
}

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

impl ShutdownSignals {
    /// Install the handlers once per process and hand back the pipe to poll.
    pub(crate) fn install() -> Result<Self> {
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe2` writes two valid fds into `fds`; both are closed on
        // exec so the helper and the relay pump never inherit them.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to create the cang workspace server shutdown pipe");
        }
        SHUTDOWN_WRITE_FD.store(fds[1], Ordering::SeqCst);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            install_handler(signal)?;
        }
        Ok(Self { read_fd: fds[0] })
    }

    /// Wait up to `timeout` for a byte on the pipe; returns whether the process
    /// has been asked to shut down.
    fn wait(&self, timeout: Duration) -> bool {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
            return true;
        }
        let mut poll = libc::pollfd {
            fd: self.read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `poll` reads one valid `pollfd`; a failure is reported by the
        // return value. `EINTR` is not an error here: the caller's loop re-checks
        // the request flag anyway.
        let rc = unsafe { libc::poll(&mut poll, 1, millis) };
        if rc > 0 {
            let mut byte = [0u8; 1];
            // SAFETY: a readable non-blocking pipe fd; the read cannot block and
            // one byte is a valid buffer.
            let _ = unsafe {
                libc::read(
                    self.read_fd,
                    byte.as_mut_ptr().cast::<libc::c_void>(),
                    byte.len(),
                )
            };
        }
        SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
    }

    /// Wake a waiting monitor without requesting a shutdown.
    fn wake(&self) {
        let fd = SHUTDOWN_WRITE_FD.load(Ordering::SeqCst);
        if fd < 0 {
            return;
        }
        let byte = [1u8; 1];
        // SAFETY: a non-blocking pipe write of one byte; a full pipe (EAGAIN) is
        // already a wake, so the result is ignored.
        let _ = unsafe { libc::write(fd, byte.as_ptr().cast::<libc::c_void>(), byte.len()) };
    }
}

fn install_handler(signal: libc::c_int) -> Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = record_shutdown_request as *const () as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: an empty mask is a valid `sigset_t` after `sigemptyset`.
    let rc = unsafe { libc::sigemptyset(&mut action.sa_mask) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to initialize signal mask");
    }
    // SAFETY: installing an async-signal-safe handler; the previous handler is
    // intentionally left in place of no interest (cang is exiting).
    let rc = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!("failed to install the shutdown handler for signal {signal}")
        });
    }
    Ok(())
}

extern "C" fn record_shutdown_request(_signal: libc::c_int) {
    // Only async-signal-safe work: one atomic store and one pipe write.
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    let fd = SHUTDOWN_WRITE_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    let byte = [1u8; 1];
    // SAFETY: a non-blocking pipe write of one byte; `write` is async-signal-safe.
    let _ = unsafe { libc::write(fd, byte.as_ptr().cast::<libc::c_void>(), byte.len()) };
}

/// What happened to the server's mount and VM when it tore down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShutdownReport {
    /// The guest could not be told to flush its workspace filesystem: host
    /// writes made through the mount may not have reached the image.
    pub(crate) flush_failure: Option<String>,
    /// The mount is still there: `fusermount3 -u` was refused.
    pub(crate) mount_failure: Option<String>,
    /// The VM could not be stopped (or its process could not be found).
    pub(crate) vm_failure: Option<String>,
    /// The server never got a usable host view, so it is stopping itself.
    pub(crate) readiness_failure: Option<String>,
}

impl ShutdownReport {
    /// The line that decides the exit code, most specific first.
    pub(crate) fn failure(&self) -> Option<&str> {
        self.readiness_failure
            .as_deref()
            .or(self.mount_failure.as_deref())
            .or(self.flush_failure.as_deref())
            .or(self.vm_failure.as_deref())
    }
}

/// The two steps of the server's teardown, as a seam: the ordering is the
/// contract, and it is tested without a VM or a mount.
pub(crate) trait ServerTeardown {
    /// Commit what the guest has of the workspace tree to its disk, through the
    /// guest's own kernel. A stop that skips this loses every host write the
    /// guest has not committed yet: the VM is stopped with a signal, not shut
    /// down, so its page cache and the current btrfs transaction are dropped.
    fn flush_guest(&mut self) -> Result<()>;
    /// Unmount the host view. Called while the guest is still alive.
    fn unmount(&mut self) -> Result<()>;
    /// Stop the VM that was serving it. Called only after `unmount`.
    fn stop_vm(&mut self) -> Result<()>;
}

/// Flush the guest's filesystem, unmount the host view, *then* stop the VM.
pub(crate) fn teardown_in_order(
    ops: &mut impl ServerTeardown,
    readiness_failure: Option<String>,
) -> ShutdownReport {
    let flush_failure = ops.flush_guest().err().map(|err| format!("{err:#}"));
    let mount_failure = ops.unmount().err().map(|err| format!("{err:#}"));
    let vm_failure = ops.stop_vm().err().map(|err| format!("{err:#}"));
    ShutdownReport {
        readiness_failure,
        flush_failure,
        mount_failure,
        vm_failure,
    }
}

/// The live implementation: the relay guard owns the mount, and the VM is the
/// launched helper whose identity the supervisor recorded.
struct HostTeardown {
    guard: WorkspaceRelayGuard,
    task_dir: PathBuf,
    task_id: String,
    mountpoint: PathBuf,
}

impl ServerTeardown for HostTeardown {
    fn flush_guest(&mut self) -> Result<()> {
        flush_guest_workspace(&self.mountpoint, &self.task_id)
    }

    fn unmount(&mut self) -> Result<()> {
        self.guard.shutdown()
    }

    fn stop_vm(&mut self) -> Result<()> {
        stop_task_process(&self.task_dir)
    }
}

/// Ask the guest to commit `/workspace` to its disk, and wait for it.
///
/// A bounded child process rather than an in-process exec client: the teardown
/// thread must not read the terminal (the exec client forwards stdin) and must
/// not be able to block for ever, and `cang exec` already owns both ends of that
/// protocol.
fn flush_guest_workspace(mountpoint: &Path, task_id: &str) -> Result<()> {
    let executable = std::env::current_exe()
        .context("failed to resolve the cang executable to flush the guest filesystem")?;
    let mut child = Command::new(executable)
        .arg("exec")
        .arg(task_id)
        .arg("--")
        .args(GUEST_FLUSH_COMMAND)
        // `cang exec` finds its task through the current directory's state root,
        // and this runs before the mount goes away.
        .current_dir(mountpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| {
            format!(
                "failed to run 'cang exec {task_id} -- {}' to flush the guest's filesystem",
                GUEST_FLUSH_COMMAND.join(" ")
            )
        })?;
    let deadline = Instant::now() + FLUSH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => bail!(
                "the guest did not flush '/workspace' before the server stopped: 'cang exec {task_id} -- {}' exited with {status}",
                GUEST_FLUSH_COMMAND.join(" ")
            ),
            Ok(None) => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to wait for the guest filesystem flush of '{task_id}'")
                });
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "the guest did not flush '/workspace' within {}s; host writes made through the mount may not be in the image",
                FLUSH_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(POLL);
    }
}

/// `SIGTERM` the task's helper process group, escalating to `SIGKILL` when it
/// does not stop in [`VM_STOP_GRACE`].
///
/// The helper is the process the supervisor spawned and recorded in the task
/// directory's active-task record - the same identity `cang kill` signals, so
///(a) this can never signal a reused pid and (b) `cang kill` and a server stop
/// agree about what "the VM" is.
fn stop_task_process(task_dir: &Path) -> Result<()> {
    let deadline = Instant::now() + HELPER_APPEARS_TIMEOUT;
    let record = loop {
        match crate::runtime::session::task_control::read_registered_task(task_dir)? {
            Some(record) => break Some(record),
            None if Instant::now() >= deadline => break None,
            None => std::thread::sleep(POLL),
        }
    };
    let Some(record) = record else {
        bail!(
            "the cang workspace server's VM has no active-task record in '{}'; nothing to stop",
            task_dir.display()
        );
    };
    match ProcfsInspector.status(&record.process) {
        ActiveTaskStatus::Running => {}
        // Already gone: the guest died on its own and the mount is what was left.
        ActiveTaskStatus::Stale | ActiveTaskStatus::PidReused => return Ok(()),
        ActiveTaskStatus::Unreadable => bail!(
            "the cang workspace server's VM process identity (pid {}) is unreadable; refusing to signal it",
            record.process.pid
        ),
    }
    signal_process_group(record.process.pgid, libc::SIGTERM)?;
    let deadline = Instant::now() + VM_STOP_GRACE;
    while Instant::now() < deadline {
        if ProcfsInspector.status(&record.process) != ActiveTaskStatus::Running {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    signal_process_group(record.process.pgid, libc::SIGKILL)?;
    let deadline = Instant::now() + KILL_TIMEOUT;
    while Instant::now() < deadline {
        if ProcfsInspector.status(&record.process) != ActiveTaskStatus::Running {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    bail!(
        "the cang workspace server's VM (process group {}) did not stop after SIGTERM then SIGKILL",
        record.process.pgid
    )
}

/// Signal one process group, treating a vanished group as "already stopped".
fn signal_process_group(pgid: u32, signal: libc::c_int) -> Result<()> {
    let pgid = libc::pid_t::try_from(pgid).context("process group id does not fit in i32")?;
    if pgid <= 1 {
        bail!("refusing to signal unsafe process group id {pgid}");
    }
    // SAFETY: `kill` takes a pgid and a signal; a failure is reported by the
    // return value.
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err).with_context(|| format!("failed to send signal {signal} to process group {pgid}"))
}

/// The running server's monitor: owns the mount for the server's lifetime and
/// performs the ordered teardown when the process is asked to stop.
pub(crate) struct WorkspaceServerMonitor {
    finished: Arc<AtomicBool>,
    handle: JoinHandle<Option<ShutdownReport>>,
    signals: Arc<ShutdownSignals>,
    /// The mount's fate, shared with the relay guard the monitor owns.
    outcome: Arc<Mutex<RelayMountOutcome>>,
}

impl WorkspaceServerMonitor {
    /// Start the monitor thread: the only owner of the mount from here on.
    pub(crate) fn start(
        signals: Arc<ShutdownSignals>,
        guard: WorkspaceRelayGuard,
        mountpoint: PathBuf,
        task_dir: PathBuf,
        task_id: String,
    ) -> Result<Self> {
        let outcome = guard.outcome_handle();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = Arc::clone(&finished);
        let thread_signals = Arc::clone(&signals);
        let handle = std::thread::Builder::new()
            .name("cang-workspace-server".to_owned())
            .spawn(move || {
                monitor(
                    thread_signals.as_ref(),
                    &thread_finished,
                    guard,
                    mountpoint,
                    task_dir,
                    task_id,
                )
            })
            .context("failed to start the cang workspace server monitor")?;
        Ok(Self {
            finished,
            handle,
            signals,
            outcome,
        })
    }

    /// What the mount did while the monitor owned it.
    pub(crate) fn outcome(&self) -> RelayMountOutcome {
        self.outcome
            .lock()
            .map(|outcome| outcome.clone())
            .unwrap_or(RelayMountOutcome::Unobserved)
    }

    /// The session is over: let the monitor finish, and report what it did
    /// (`None` when the VM ended on its own and there was nothing to tear down).
    pub(crate) fn finish(self) -> Option<ShutdownReport> {
        self.finished.store(true, Ordering::SeqCst);
        self.signals.wake();
        self.handle.join().ok().flatten()
    }
}

fn monitor(
    signals: &ShutdownSignals,
    finished: &AtomicBool,
    guard: WorkspaceRelayGuard,
    mountpoint: PathBuf,
    task_dir: PathBuf,
    task_id: String,
) -> Option<ShutdownReport> {
    let mut ops = HostTeardown {
        guard,
        task_dir,
        task_id,
        mountpoint: mountpoint.clone(),
    };

    // 1. The mount may only be announced as up once it is actually there: the
    //    server's whole product is the host's continuous view, so a relay that
    //    never mounts stops the server instead of leaving a VM nobody can see.
    let deadline = Instant::now() + READY_DEADLINE;
    let readiness_failure = loop {
        if signals.wait(POLL) {
            break None;
        }
        if finished.load(Ordering::SeqCst) {
            return None;
        }
        match ops.guard.outcome() {
            RelayMountOutcome::Ready { .. } => break None,
            RelayMountOutcome::Error(reason) => {
                break Some(format!(
                    "the cang workspace server has no host view: the guest relay did not mount '{}': {reason}",
                    mountpoint.display()
                ));
            }
            RelayMountOutcome::Timed { seconds } => {
                break Some(format!(
                    "the cang workspace server has no host view: the guest relay did not appear at '{}' within {seconds}s",
                    mountpoint.display()
                ));
            }
            RelayMountOutcome::Unobserved => {}
        }
        if Instant::now() >= deadline {
            break Some(format!(
                "the cang workspace server has no host view: the guest relay did not appear at '{}' within {}s",
                mountpoint.display(),
                READY_DEADLINE.as_secs()
            ));
        }
    };

    // 2. Either a stop request (signal, `cang workspace stop`) tears the server
    //    down in order, or the VM ended on its own and the supervisor's own
    //    teardown below takes the mount with it.
    loop {
        if signals.wait(POLL) {
            break;
        }
        if finished.load(Ordering::SeqCst) {
            return None;
        }
    }
    // Named, and before the VM is signalled: the log of a server stopping has to
    // read in the order the guarantee is made in.
    eprintln!(
        "cang: stopping the workspace server: flushing the guest's filesystem, unmounting '{}', \
         then stopping the VM",
        mountpoint.display()
    );
    Some(teardown_in_order(&mut ops, readiness_failure))
}

/// `cang workspace stop <disk>`: ask the recorded server to shut down and wait
/// for it, reporting by name what it could not finish.
pub(crate) fn run_stop(disk: &Path) -> Result<String> {
    run_stop_at(&record_path(disk), disk)
}

fn run_stop_at(path: &Path, disk: &Path) -> Result<String> {
    let Some(record) = read_record(path)? else {
        return Ok(format!(
            "no cang workspace server is running for '{}'\n",
            disk.display()
        ));
    };
    match ProcfsInspector.status(&record.process) {
        ActiveTaskStatus::Running => {}
        ActiveTaskStatus::Stale | ActiveTaskStatus::PidReused => {
            let _ = fs::remove_file(path);
            return Ok(format!(
                "no cang workspace server is running for '{}': its recorded process {} is gone; removed the stale record '{}'\n",
                disk.display(),
                record.process.pid,
                path.display()
            ));
        }
        ActiveTaskStatus::Unreadable => bail!(
            "the cang workspace server for '{}' (pid {}) has an unreadable process identity; refusing to signal it",
            disk.display(),
            record.process.pid
        ),
    }
    // SAFETY: `kill` takes a pid and a signal; a failure is reported by the
    // return value. The identity above was just checked against this pid.
    let rc = unsafe { libc::kill(record.process.pid as libc::pid_t, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err).with_context(|| {
                format!(
                    "failed to signal the cang workspace server for '{}' (pid {})",
                    disk.display(),
                    record.process.pid
                )
            });
        }
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if ProcfsInspector.status(&record.process) != ActiveTaskStatus::Running {
            break;
        }
        std::thread::sleep(POLL);
    }
    if ProcfsInspector.status(&record.process) == ActiveTaskStatus::Running {
        bail!(
            "the cang workspace server for '{}' (pid {}) did not exit within {}s; it is still starting or wedged - run 'cang kill {}' if the VM is still there",
            disk.display(),
            record.process.pid,
            STOP_TIMEOUT.as_secs(),
            record.task_id
        );
    }
    let deadline = Instant::now() + UNMOUNT_TIMEOUT;
    while is_mountpoint(&record.mountpoint) && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
    if is_mountpoint(&record.mountpoint) {
        bail!(
            "the cang workspace server for '{}' exited, but its mount at '{}' is still there: `fusermount3 -u {}` was refused, so a host process with its working directory or an open file inside the mount keeps it busy - leave that directory and run `fusermount3 -u {}` yourself",
            disk.display(),
            record.mountpoint.display(),
            record.mountpoint.display(),
            record.mountpoint.display()
        );
    }
    let _ = fs::remove_file(path);
    Ok(format!(
        "stopped the cang workspace server for '{}' (task '{}', pid {}, mount '{}' unmounted)\n",
        disk.display(),
        record.task_id,
        record.process.pid,
        record.mountpoint.display()
    ))
}

/// `cang workspace status <disk>`: what the host's view of one image is doing.
pub(crate) fn run_status(disk: &Path) -> Result<String> {
    run_status_at(&record_path(disk), disk)
}

fn run_status_at(path: &Path, disk: &Path) -> Result<String> {
    let Some(record) = read_record(path)? else {
        return Ok(format!(
            "no cang workspace server for '{}' (no record at '{}')\n",
            disk.display(),
            path.display()
        ));
    };
    let status = ProcfsInspector.status(&record.process);
    let mounted = is_mountpoint(&record.mountpoint);
    let mut lines = vec![
        format!("workspace server for '{}'", disk.display()),
        format!(
            "  task:     {} (pid {}, status {})",
            record.task_id,
            record.process.pid,
            status.as_str()
        ),
        format!("  task dir: {}", record.task_dir.display()),
        format!(
            "  mount:    {} ({})",
            record.mountpoint.display(),
            if mounted { "mounted" } else { "not mounted" }
        ),
        format!("  started:  {} (unix seconds)", record.started_at_unix_secs),
        format!("  lease:    {}", lease_path(disk).display()),
    ];
    if status != ActiveTaskStatus::Running {
        lines.push(format!(
            "  note:     the recorded process is not running; run 'cang workspace stop {}' to clear the stale record",
            disk.display()
        ));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// Refuse a mountpoint that something is already mounted on: the server's own
/// mount would either fail or shadow a mount it does not own, and the ordered
/// teardown's `fusermount3 -u` would then remove somebody else's mount.
pub(crate) fn refuse_occupied_mountpoint(mountpoint: &Path) -> Result<()> {
    if is_mountpoint(mountpoint) {
        bail!(
            "'{}' already has a mount on it: unmount it first with `fusermount3 -u {}`, then start the workspace server again",
            mountpoint.display(),
            mountpoint.display()
        );
    }
    Ok(())
}

/// Install the shutdown handlers for a workspace server as early as the plan is
/// known: a stop request that arrives while the rootfs is still being
/// materialized must not kill cang with the default action and orphan a VM.
pub(crate) fn install_signals() -> Result<Arc<ShutdownSignals>> {
    Ok(Arc::new(ShutdownSignals::install()?))
}

/// The host-side bookkeeping of one running workspace server: the state file
/// `cang workspace stop`/`status` read, and the shutdown signals.
pub(crate) struct WorkspaceServerSession {
    record_path: PathBuf,
    mountpoint: PathBuf,
    signals: Arc<ShutdownSignals>,
}

impl WorkspaceServerSession {
    /// Announce the server (state file) and take over its mount lifetime.
    pub(crate) fn start(
        disk: &Path,
        mountpoint: &Path,
        task_id: &str,
        task_dir: &Path,
        signals: Arc<ShutdownSignals>,
    ) -> Result<Self> {
        let record_path = record_path(disk);
        let record = WorkspaceServerRecord::new(
            disk,
            mountpoint,
            task_id,
            task_dir,
            crate::runtime::session::task_control::current_process_identity()?,
        )?;
        record.write(&record_path)?;
        Ok(Self {
            record_path,
            mountpoint: mountpoint.to_path_buf(),
            signals,
        })
    }

    /// Hand the mount to the monitor, which from now on owns the unmount-before-
    /// stop ordering for it.
    pub(crate) fn monitor(
        &self,
        guard: WorkspaceRelayGuard,
        task_dir: &Path,
        task_id: &str,
    ) -> Result<WorkspaceServerMonitor> {
        WorkspaceServerMonitor::start(
            Arc::clone(&self.signals),
            guard,
            self.mountpoint.clone(),
            task_dir.to_path_buf(),
            task_id.to_owned(),
        )
    }
}

impl Drop for WorkspaceServerSession {
    fn drop(&mut self) {
        // The lease itself is released by its own `File`; only the record has to
        // be removed, and removing it twice (a `cang workspace stop` already did)
        // is not an error.
        let _ = fs::remove_file(&self.record_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Leases in a temporary runtime directory: a unit test must not leave files
    /// in the account's real runtime directory (the same one the attach sockets
    /// live in).
    fn lease_in(dir: &Path) -> PathBuf {
        dir.join("image.lease")
    }

    fn temp_disk(dir: &Path, name: &str) -> PathBuf {
        let disk = dir.join(name);
        fs::write(&disk, vec![0u8; 4096]).expect("image");
        disk
    }

    /// Broker: two leases on one image must not both be satisfied, in either
    /// direction, and each refusal must name the side that has to be stopped.
    #[test]
    fn the_server_and_a_task_launch_refuse_each_other_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = temp_disk(dir.path(), "work.raw");

        let lease = lease_in(dir.path());
        let server = WorkspaceLease::acquire_server_at(&lease, &disk).expect("server lease");
        assert!(server.path.is_file(), "{}", server.path.display());
        let err = WorkspaceLease::acquire_task_at(&lease, &disk)
            .expect_err("a task launch must refuse while the server holds the image");
        let message = format!("{err:#}");
        assert!(
            message.contains("cang workspace server"),
            "the task refusal must name the server: {message}"
        );
        assert!(
            message.contains(&format!("cang workspace stop {}", disk.display())),
            "the task refusal must tell the user what to stop: {message}"
        );
        drop(server);

        let _task = WorkspaceLease::acquire_task_at(&lease, &disk)
            .expect("task lease")
            .expect("a free image must be leased");
        let err = WorkspaceLease::acquire_server_at(&lease, &disk)
            .expect_err("a server must refuse while a task launch holds the image");
        let message = format!("{err:#}");
        assert!(
            message.contains("workspace server and a --workspace-disk task launch"),
            "the server refusal must name both sides: {message}"
        );
        assert!(
            message.contains(&disk.display().to_string()),
            "the server refusal must name the image: {message}"
        );
    }

    /// Two servers on one image refuse the same way, and two task launches share.
    #[test]
    fn two_servers_refuse_and_two_task_launches_share() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = temp_disk(dir.path(), "work.raw");

        let lease = lease_in(dir.path());
        let first = WorkspaceLease::acquire_server_at(&lease, &disk).expect("first server");
        let err = WorkspaceLease::acquire_server_at(&lease, &disk)
            .expect_err("second server must refuse");
        assert!(format!("{err:#}").contains("another cang process is using the workspace image"));
        drop(first);

        let a = WorkspaceLease::acquire_task(&disk)
            .expect("task a")
            .expect("leased");
        let b = WorkspaceLease::acquire_task(&disk)
            .expect("task b must share with another task launch")
            .expect("leased");
        assert!(
            a.path == b.path,
            "one image, one lease file: {}",
            a.path.display()
        );
    }

    /// The identity of the key: every spelling of one image resolves to one lease
    /// file, and the lease lands in cang's per-user runtime directory.
    #[test]
    fn one_image_has_one_lease_whatever_the_spelling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = temp_disk(dir.path(), "work.raw");
        let absolute = lease_path(&disk);
        let via_symlinked_parent = {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(dir.path(), &link).expect("symlink parent");
            lease_path(&link.join("work.raw"))
        };
        assert_eq!(absolute, via_symlinked_parent);
        assert!(
            absolute.starts_with(runtime_subdir()),
            "{}",
            absolute.display()
        );
        assert_eq!(
            absolute.parent().expect("parent").file_name(),
            Some(std::ffi::OsStr::new(RUNTIME_SUBDIR))
        );
    }

    /// A missing image still gets a stable key from its parent directory, so a
    /// launch that creates it and a later one that reuses it agree.
    #[test]
    fn a_not_yet_created_image_keeps_its_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = dir.path().join("later.raw");
        let before = lease_path(&disk);
        fs::write(&disk, vec![0u8; 4096]).expect("create");
        let after = lease_path(&disk);
        assert_eq!(before, after);
    }

    /// A lease file the launch cannot create is bookkeeping, not a launch
    /// failure: every `--workspace-disk` launch that predates this lease keeps
    /// working.
    #[test]
    fn a_lease_file_that_cannot_be_created_does_not_fail_a_task_launch() {
        if unsafe { libc::geteuid() } == 0 {
            // root may write into a directory that has no write bit for it.
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = temp_disk(dir.path(), "work.raw");
        let runtime = dir.path().join("runtime");
        fs::create_dir(&runtime).expect("runtime dir");
        let mut permissions = fs::metadata(&runtime).expect("metadata").permissions();
        permissions.set_mode(0o500);
        fs::set_permissions(&runtime, permissions).expect("read-only runtime dir");
        let path = runtime.join(format!("{}.lease", disk_key(&disk)));

        assert!(
            WorkspaceLease::acquire_task_at(&path, &disk)
                .expect("an unavailable lease must not fail a launch")
                .is_none(),
            "an unavailable lease file must not fail a launch"
        );
        // The server, by contrast, must not run without its lease.
        let err = WorkspaceLease::acquire_server_at(&path, &disk)
            .expect_err("a server without a lease must fail closed");
        assert!(
            format!("{err:#}").contains("failed to open the workspace lease"),
            "{err:#}"
        );
    }

    #[test]
    fn status_and_stop_name_a_missing_and_a_stale_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = temp_disk(dir.path(), "work.raw");
        let path = dir.path().join("ws.server");

        let status = run_status_at(&path, &disk).expect("status without a record");
        assert!(
            status.starts_with("no cang workspace server for"),
            "{status}"
        );
        assert!(status.contains(&disk.display().to_string()), "{status}");

        let stop = run_stop_at(&path, &disk).expect("stop without a record");
        assert!(
            stop.contains("no cang workspace server is running"),
            "{stop}"
        );

        // A record whose process is gone: `stop` clears it by name rather than
        // signalling a pid that may have been reused.
        let record = WorkspaceServerRecord::new(
            &disk,
            &dir.path().join("work"),
            "task-stale",
            &dir.path().join("tasks/task-stale"),
            ProcessIdentity {
                pid: u32::MAX,
                pgid: u32::MAX,
                sid: u32::MAX,
                proc_start_time_ticks: 1,
                boot_id: "not-this-boot".to_owned(),
            },
        )
        .expect("record");
        record.write(&path).expect("write");
        let status = run_status_at(&path, &disk).expect("status of a stale record");
        assert!(status.contains("task-stale"), "{status}");
        assert!(status.contains("not running"), "{status}");
        assert!(status.contains("cang workspace stop"), "{status}");

        let stop = run_stop_at(&path, &disk).expect("stop of a stale record");
        assert!(stop.contains("its recorded process"), "{stop}");
        assert!(stop.contains(&u32::MAX.to_string()), "{stop}");
        assert!(!path.exists(), "a stale record is removed by stop");
    }

    /// The ordering the ticket requires, with seams instead of a VM: the mount is
    /// gone before the VM is signalled, and a refused unmount decides the exit.
    #[test]
    fn teardown_unmounts_before_it_stops_the_vm() {
        struct Recording {
            calls: RefCell<Vec<&'static str>>,
            unmount_error: Option<String>,
            flush_error: Option<String>,
        }
        impl ServerTeardown for Recording {
            fn flush_guest(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("flush");
                match &self.flush_error {
                    Some(message) => Err(anyhow!("{message}")),
                    None => Ok(()),
                }
            }

            fn unmount(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("unmount");
                match &self.unmount_error {
                    Some(message) => Err(anyhow!("{message}")),
                    None => Ok(()),
                }
            }

            fn stop_vm(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("stop-vm");
                Ok(())
            }
        }

        let mut ops = Recording {
            calls: RefCell::new(Vec::new()),
            unmount_error: None,
            flush_error: None,
        };
        let report = teardown_in_order(&mut ops, None);
        assert_eq!(
            *ops.calls.borrow(),
            vec!["flush", "unmount", "stop-vm"],
            "the guest must be flushed before the mount is unmounted and the VM stopped"
        );
        assert!(report.failure().is_none(), "{report:?}");

        let mut busy = Recording {
            calls: RefCell::new(Vec::new()),
            flush_error: None,
            unmount_error: Some(
                "the workspace mount at '/x' is still mounted: `fusermount3 -u /x` was refused \
                 (Device or resource busy)"
                    .to_owned(),
            ),
        };
        let report = teardown_in_order(&mut busy, None);
        assert_eq!(
            *busy.calls.borrow(),
            vec!["flush", "unmount", "stop-vm"],
            "the VM must still be stopped after a refused unmount"
        );
        let failure = report
            .failure()
            .expect("a busy mount must decide the exit code");
        assert!(failure.contains("still mounted"), "{failure}");
        assert!(failure.contains("Device or resource busy"), "{failure}");
        assert!(failure.contains("fusermount3 -u"), "{failure}");
    }

    /// A server that never gets a host view stops itself and says why.
    #[test]
    fn a_readiness_failure_is_the_reported_failure() {
        struct Quiet;
        impl ServerTeardown for Quiet {
            fn flush_guest(&mut self) -> Result<()> {
                Ok(())
            }

            fn unmount(&mut self) -> Result<()> {
                Ok(())
            }

            fn stop_vm(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let mut ops = Quiet;
        let report = teardown_in_order(
            &mut ops,
            Some("the guest relay did not mount '/x': connection refused".to_owned()),
        );
        assert_eq!(
            report.failure(),
            Some("the guest relay did not mount '/x': connection refused")
        );
        // The mount and the VM are still handled even when the mount never came.
        assert!(report.mount_failure.is_none());
        assert!(report.vm_failure.is_none());
    }

    /// A stop that cannot flush the guest says so: the writes made through the
    /// mount in the last seconds may not be in the image, and that is not a
    /// silent success.
    #[test]
    fn a_refused_flush_decides_the_exit_code_after_the_mount_and_vm_are_handled() {
        struct Flushless {
            calls: RefCell<Vec<&'static str>>,
        }
        impl ServerTeardown for Flushless {
            fn flush_guest(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("flush");
                Err(anyhow!(
                    "the guest did not flush '/workspace' within 60s; host writes made \
                     through the mount may not be in the image"
                ))
            }

            fn unmount(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("unmount");
                Ok(())
            }

            fn stop_vm(&mut self) -> Result<()> {
                self.calls.borrow_mut().push("stop-vm");
                Ok(())
            }
        }
        let mut ops = Flushless {
            calls: RefCell::new(Vec::new()),
        };
        let report = teardown_in_order(&mut ops, None);

        assert_eq!(*ops.calls.borrow(), vec!["flush", "unmount", "stop-vm"]);
        let failure = report.failure().expect("a refused flush must be reported");
        assert!(failure.contains("did not flush"), "{failure}");
        assert!(failure.contains("/workspace"), "{failure}");
    }

    #[test]
    fn a_record_round_trips_and_names_the_server_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ws.server");
        let record = WorkspaceServerRecord::new(
            Path::new("/tmp/work.raw"),
            Path::new("/tmp/work"),
            "task-7",
            Path::new("/tmp/state/tasks/task-7"),
            ProcessIdentity {
                pid: 4242,
                pgid: 4200,
                sid: 4200,
                proc_start_time_ticks: 990,
                boot_id: "boot".to_owned(),
            },
        )
        .expect("record");

        record.write(&path).expect("write");
        let read = read_record(&path).expect("read").expect("present");

        assert_eq!(read.disk, PathBuf::from("/tmp/work.raw"));
        assert_eq!(read.mountpoint, PathBuf::from("/tmp/work"));
        assert_eq!(read.task_id, "task-7");
        assert_eq!(read.process.pid, 4242);
        assert_eq!(read.process.proc_start_time_ticks, 990);
        assert_eq!(read.process.boot_id, "boot");
    }

    #[test]
    fn a_malformed_record_is_an_error_rather_than_a_silent_no_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ws.server");
        fs::write(&path, "version=1\ndisk=/tmp/work.raw\n").expect("short record");

        let err = read_record(&path).expect_err("a record missing fields must be refused");
        assert!(
            format!("{err:#}").contains("workspace server record"),
            "{err:#}"
        );
    }
}
