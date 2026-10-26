use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::session::rootfs::task::{HostBtrfsRootfsCommands, cleanup_task_rootfs_dir};

const ACTIVE_RECORD_FILE: &str = "active-task";
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
const PROC_ROOT: &str = "/proc";
const MIN_HANDLE_PREFIX_LEN: usize = 2;
const TERM_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskControlCommand {
    Ps,
    Kill { task_id: String },
    Attach { task_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecTaskSpec {
    pub(crate) socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTaskSpec {
    pub(crate) attach_socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaypipeTaskSpec {
    pub(crate) control_socket: PathBuf,
    pub(crate) initial_target: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveTaskSpec {
    pub(crate) task_id: String,
    pub(crate) workspace_slug: String,
    pub(crate) workspace_dir: PathBuf,
    pub(crate) task_dir: PathBuf,
    pub(crate) image_reference: String,
    pub(crate) image_digest: Option<String>,
    pub(crate) waypipe: Option<WaypipeTaskSpec>,
    pub(crate) exec: Option<ExecTaskSpec>,
    pub(crate) managed: Option<ManagedTaskSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) pgid: u32,
    pub(crate) sid: u32,
    pub(crate) proc_start_time_ticks: u64,
    pub(crate) boot_id: String,
}

impl ProcessIdentity {
    pub(crate) fn from_spawned_process(pid: u32, pgid: u32, sid: u32) -> Result<Self> {
        read_process_identity(
            Path::new(PROC_ROOT),
            Path::new(BOOT_ID_PATH),
            pid,
            pgid,
            sid,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecTaskRecord {
    pub(crate) socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedTaskRecord {
    pub(crate) attach_socket: PathBuf,
    pub(crate) guest_port: u32,
    pub(crate) protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaypipeTaskRecord {
    pub(crate) control_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveTaskRecord {
    pub(crate) task_id: String,
    pub(crate) workspace_slug: String,
    pub(crate) workspace_dir: PathBuf,
    pub(crate) task_dir: PathBuf,
    pub(crate) image_reference: String,
    pub(crate) image_digest: Option<String>,
    pub(crate) started_at_unix_secs: u64,
    pub(crate) process: ProcessIdentity,
    pub(crate) waypipe: Option<WaypipeTaskRecord>,
    pub(crate) exec: Option<ExecTaskRecord>,
    pub(crate) managed: Option<ManagedTaskRecord>,
}

impl ActiveTaskRecord {
    fn from_spec(spec: ActiveTaskSpec, process: ProcessIdentity, now: SystemTime) -> Result<Self> {
        let started_at_unix_secs = now
            .duration_since(UNIX_EPOCH)
            .context("system clock is before unix epoch")?
            .as_secs();
        Ok(Self {
            task_id: spec.task_id,
            workspace_slug: spec.workspace_slug,
            workspace_dir: spec.workspace_dir,
            task_dir: spec.task_dir,
            image_reference: spec.image_reference,
            image_digest: spec.image_digest,
            started_at_unix_secs,
            process,
            waypipe: spec.waypipe.map(|waypipe| WaypipeTaskRecord {
                control_socket: waypipe.control_socket,
            }),
            exec: spec.exec.map(|exec| ExecTaskRecord {
                socket: exec.socket,
                guest_port: exec.guest_port,
                protocol_version: exec.protocol_version,
            }),
            managed: spec.managed.map(|managed| ManagedTaskRecord {
                attach_socket: managed.attach_socket,
                guest_port: managed.guest_port,
                protocol_version: managed.protocol_version,
            }),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveTaskStatus {
    Running,
    Stale,
    PidReused,
    Unreadable,
}

impl ActiveTaskStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stale => "stale",
            Self::PidReused => "pid-reused",
            Self::Unreadable => "unreadable",
        }
    }
}

pub(crate) trait ProcessInspector {
    fn status(&self, identity: &ProcessIdentity) -> ActiveTaskStatus;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcfsInspector;

impl ProcessInspector for ProcfsInspector {
    fn status(&self, identity: &ProcessIdentity) -> ActiveTaskStatus {
        inspect_process_identity(Path::new(PROC_ROOT), Path::new(BOOT_ID_PATH), identity)
            .unwrap_or(ActiveTaskStatus::Unreadable)
    }
}

pub(crate) trait TaskSignaler {
    fn signal_process_group(&mut self, pgid: u32, signal: i32) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostTaskSignaler;

impl TaskSignaler for HostTaskSignaler {
    fn signal_process_group(&mut self, pgid: u32, signal: i32) -> Result<()> {
        let pgid = i32::try_from(pgid).context("process group id does not fit in i32")?;
        if pgid <= 1 {
            bail!("refusing to signal unsafe process group id {pgid}");
        }
        let target = -pgid;
        let rc = unsafe { libc::kill(target, signal) };
        if rc == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to send signal {signal} to process group {pgid}"))
    }
}

trait TaskRootfsCleaner {
    fn cleanup_task_rootfs(&self, task_dir: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct HostTaskRootfsCleaner;

impl TaskRootfsCleaner for HostTaskRootfsCleaner {
    fn cleanup_task_rootfs(&self, task_dir: &Path) -> Result<()> {
        cleanup_task_rootfs_dir(task_dir, &HostBtrfsRootfsCommands)
    }
}

pub(crate) fn write_active_task(spec: ActiveTaskSpec, process: ProcessIdentity) -> Result<()> {
    let record = ActiveTaskRecord::from_spec(spec, process, SystemTime::now())?;
    write_active_task_record(&record)
}

pub(crate) fn remove_active_task(task_dir: &Path) -> Result<()> {
    let path = active_record_path(task_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove '{}'", path.display())),
    }
}

pub(crate) fn run_task_control_command(
    command: TaskControlCommand,
    app_dir: &Path,
) -> Result<String> {
    match command {
        TaskControlCommand::Ps => render_ps(app_dir, &ProcfsInspector),
        TaskControlCommand::Kill { task_id } => kill_task(
            app_dir,
            &task_id,
            &ProcfsInspector,
            &mut HostTaskSignaler,
            thread::sleep,
        ),
        TaskControlCommand::Attach { task_id } => {
            crate::runtime::session::attach::attach_to_task(app_dir, &task_id, &ProcfsInspector)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceTaskGateReport {
    pub(crate) stale_task_ids: Vec<String>,
}

pub(crate) fn ensure_workspace_has_no_running_tasks(
    workspace_state_root: &Path,
    inspector: &impl ProcessInspector,
) -> Result<WorkspaceTaskGateReport> {
    let mut stale_task_ids = Vec::new();
    let mut blockers = Vec::new();

    for record in list_workspace_records(workspace_state_root)? {
        match inspector.status(&record.process) {
            ActiveTaskStatus::Stale => stale_task_ids.push(record.task_id),
            status @ (ActiveTaskStatus::Running
            | ActiveTaskStatus::PidReused
            | ActiveTaskStatus::Unreadable) => {
                blockers.push(format!("{} ({})", record.task_id, status.as_str()))
            }
        }
    }

    if !blockers.is_empty() {
        bail!(
            "refusing container-store disk maintenance while current workspace has active or unsafe task records: {}",
            blockers.join(", ")
        );
    }

    Ok(WorkspaceTaskGateReport { stale_task_ids })
}

/// Refuse to start a second task in a workspace that already runs one.
///
/// Until this gate existed a launch into a busy workspace produced a second VM
/// whose attach socket nobody watched, and `cang -- <command>` there looked like
/// it did nothing at all. The task records live under the workspace's own state
/// root, but the workspace directory is compared as well: a shared state-location
/// override can put several workspaces under one state root.
pub(crate) fn ensure_workspace_has_no_running_task(
    app_dir: &Path,
    workspace_dir: &Path,
    inspector: &impl ProcessInspector,
) -> Result<()> {
    let mut blockers = Vec::new();
    for record in list_records(app_dir)? {
        if record.workspace_dir != workspace_dir {
            continue;
        }
        let status = inspector.status(&record.process);
        match status {
            ActiveTaskStatus::Running | ActiveTaskStatus::Unreadable => {
                let handle = task_handle(&record.task_id).unwrap_or(&record.task_id);
                blockers.push(format!(
                    "task '{}' (handle {handle}, pid {}, status {})",
                    record.task_id,
                    record.process.pid,
                    status.as_str()
                ));
            }
            // A stale or pid-reused record describes a task that is already gone,
            // so it must not block a fresh launch.
            ActiveTaskStatus::Stale | ActiveTaskStatus::PidReused => {}
        }
    }
    if blockers.is_empty() {
        return Ok(());
    }
    bail!(
        "workspace '{}' already has a running cang task: {}; wait for it to finish, run 'cang attach' to join it, or run 'cang kill <handle>' before launching again",
        workspace_dir.display(),
        blockers.join(", ")
    )
}

fn render_ps(app_dir: &Path, inspector: &impl ProcessInspector) -> Result<String> {
    let rows = list_records(app_dir)?
        .into_iter()
        .map(|record| {
            let status = inspector.status(&record.process);
            TaskRow { record, status }
        })
        .collect::<Vec<_>>();

    Ok(render_task_table(&rows))
}

fn kill_task(
    app_dir: &Path,
    task_selector: &str,
    inspector: &impl ProcessInspector,
    signaler: &mut impl TaskSignaler,
    sleep: impl FnOnce(Duration),
) -> Result<String> {
    kill_task_with_cleaner(
        app_dir,
        task_selector,
        inspector,
        signaler,
        &HostTaskRootfsCleaner,
        sleep,
    )
}

fn kill_task_with_cleaner(
    app_dir: &Path,
    task_selector: &str,
    inspector: &impl ProcessInspector,
    signaler: &mut impl TaskSignaler,
    cleaner: &impl TaskRootfsCleaner,
    sleep: impl FnOnce(Duration),
) -> Result<String> {
    let records = list_records(app_dir)?;
    let record = resolve_task_selector(&records, task_selector)?;

    kill_record(record, inspector, signaler, cleaner, sleep)
}

pub(crate) fn resolve_task_selector<'a>(
    records: &'a [ActiveTaskRecord],
    task_selector: &str,
) -> Result<&'a ActiveTaskRecord> {
    let exact_matches = records
        .iter()
        .filter(|record| record.task_id == task_selector)
        .collect::<Vec<_>>();

    match exact_matches.as_slice() {
        [record] => return Ok(record),
        [] => {}
        _ => bail!(
            "multiple active cang task records matched id '{task_selector}'; refusing to kill"
        ),
    }

    let handle_matches = records
        .iter()
        .filter(|record| task_handle(&record.task_id) == Some(task_selector))
        .collect::<Vec<_>>();

    match handle_matches.as_slice() {
        [record] => return Ok(record),
        [] => {}
        _ => bail!(
            "multiple active cang task records matched handle '{task_selector}'; refusing to kill; candidates: {}",
            format_task_candidates(&handle_matches)
        ),
    }

    if task_selector.chars().count() < MIN_HANDLE_PREFIX_LEN {
        bail!(
            "handle prefix '{task_selector}' is too short; use at least {MIN_HANDLE_PREFIX_LEN} characters or an exact task id/handle"
        );
    }

    let handle_prefix_matches = records
        .iter()
        .filter(|record| {
            task_handle(&record.task_id).is_some_and(|handle| handle.starts_with(task_selector))
        })
        .collect::<Vec<_>>();

    match handle_prefix_matches.as_slice() {
        [record] => return Ok(record),
        [] => {}
        _ => bail!(
            "multiple active cang task records matched handle prefix '{task_selector}'; refusing to kill; candidates: {}",
            format_task_candidates(&handle_prefix_matches)
        ),
    }

    let Some(abbreviated_selector) = parse_abbreviated_handle_selector(task_selector)? else {
        bail!("no active cang task with id, handle, or handle prefix '{task_selector}'");
    };

    let abbreviated_matches = records
        .iter()
        .filter(|record| {
            task_handle(&record.task_id)
                .is_some_and(|handle| abbreviated_handle_matches(handle, &abbreviated_selector))
        })
        .collect::<Vec<_>>();

    match abbreviated_matches.as_slice() {
        [record] => Ok(record),
        [] => bail!(
            "no active cang task with id, handle, or handle prefix, and no abbreviated handle selector matched '{task_selector}'"
        ),
        _ => bail!(
            "multiple active cang task records matched abbreviated handle selector '{task_selector}'; refusing to kill; candidates: {}",
            format_task_candidates(&abbreviated_matches)
        ),
    }
}

struct AbbreviatedHandleSelector<'a> {
    name_prefix: &'a str,
    number_prefix: &'a str,
}

fn parse_abbreviated_handle_selector(
    task_selector: &str,
) -> Result<Option<AbbreviatedHandleSelector<'_>>> {
    let Some((name_prefix, number_prefix)) = task_selector.rsplit_once('-') else {
        return Ok(None);
    };

    if name_prefix.chars().count() < MIN_HANDLE_PREFIX_LEN {
        bail!(
            "abbreviated handle selector '{task_selector}' has a name prefix that is too short; use at least {MIN_HANDLE_PREFIX_LEN} characters before '-'"
        );
    }

    if number_prefix.is_empty() || !number_prefix.chars().all(|ch| ch.is_ascii_digit()) {
        bail!(
            "abbreviated handle selector '{task_selector}' must end with a non-empty numeric displayed-handle segment prefix"
        );
    }

    Ok(Some(AbbreviatedHandleSelector {
        name_prefix,
        number_prefix,
    }))
}

fn abbreviated_handle_matches(handle: &str, selector: &AbbreviatedHandleSelector<'_>) -> bool {
    let Some((handle_name, handle_number)) = handle.rsplit_once('-') else {
        return false;
    };

    handle_name.starts_with(selector.name_prefix)
        && handle_number.starts_with(selector.number_prefix)
}

fn task_handle(task_id: &str) -> Option<&str> {
    let (handle, suffix) = task_id.rsplit_once('-')?;
    if handle.is_empty()
        || !handle.contains('-')
        || suffix.is_empty()
        || !suffix.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some(handle)
}

fn format_task_candidates(records: &[&ActiveTaskRecord]) -> String {
    records
        .iter()
        .map(|record| {
            let handle = task_handle(&record.task_id).unwrap_or(&record.task_id);
            format!(
                "{} (handle {handle}, workspace {})",
                record.task_id, record.workspace_slug
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn kill_record(
    record: &ActiveTaskRecord,
    inspector: &impl ProcessInspector,
    signaler: &mut impl TaskSignaler,
    cleaner: &impl TaskRootfsCleaner,
    sleep: impl FnOnce(Duration),
) -> Result<String> {
    match inspector.status(&record.process) {
        ActiveTaskStatus::Running => {}
        ActiveTaskStatus::Stale => {
            return finish_kill_success(
                record,
                format!(
                    "cang task '{}' already exited before kill (session {}, process group {})\n",
                    record.task_id, record.process.sid, record.process.pgid
                ),
                cleaner,
            );
        }
        ActiveTaskStatus::PidReused => {
            return finish_kill_success(
                record,
                format!(
                    "cang task '{}' recorded process id was reused; skipped signal and cleaning task state only (session {}, process group {})\n",
                    record.task_id, record.process.sid, record.process.pgid
                ),
                cleaner,
            );
        }
        ActiveTaskStatus::Unreadable => bail!(
            "task '{}' process identity is unreadable; refusing to signal or clean up",
            record.task_id
        ),
    }
    if ignore_missing_process_group(
        signaler.signal_process_group(record.process.pgid, libc::SIGTERM),
    )?
    .is_missing()
    {
        return finish_kill_success(
            record,
            format!(
                "cang task '{}' already exited before SIGTERM (session {}, process group {})\n",
                record.task_id, record.process.sid, record.process.pgid
            ),
            cleaner,
        );
    }
    sleep(TERM_GRACE);
    if inspector.status(&record.process) == ActiveTaskStatus::Running {
        if inspector.status(&record.process) != ActiveTaskStatus::Running {
            return finish_kill_success(
                record,
                format!(
                    "sent SIGTERM to cang task '{}' (session {}, process group {})\n",
                    record.task_id, record.process.sid, record.process.pgid
                ),
                cleaner,
            );
        }
        ignore_missing_process_group(
            signaler.signal_process_group(record.process.pgid, libc::SIGKILL),
        )?;
        return finish_kill_success(
            record,
            format!(
                "sent SIGTERM then SIGKILL to cang task '{}' (session {}, process group {})\n",
                record.task_id, record.process.sid, record.process.pgid
            ),
            cleaner,
        );
    }
    finish_kill_success(
        record,
        format!(
            "sent SIGTERM to cang task '{}' (session {}, process group {})\n",
            record.task_id, record.process.sid, record.process.pgid
        ),
        cleaner,
    )
}

fn finish_kill_success(
    record: &ActiveTaskRecord,
    message: String,
    cleaner: &impl TaskRootfsCleaner,
) -> Result<String> {
    if let Err(cleanup_err) = cleaner.cleanup_task_rootfs(&record.task_dir) {
        let cleanup_message = format!("{cleanup_err:#}");
        if let Err(restore_err) = write_active_task_record(record) {
            bail!(
                "failed to clean cang task '{}' rootfs state after kill handling; rerun `cang kill {}` to retry cleanup; cleanup error: {cleanup_message}; also failed to restore active task record: {restore_err:#}",
                record.task_id,
                record.task_id
            );
        }
        return Err(cleanup_err).context(format!(
            "failed to clean cang task '{}' rootfs state after kill handling; active task record remains for retry; rerun `cang kill {}` to retry cleanup",
            record.task_id, record.task_id
        ));
    }
    remove_active_task(&record.task_dir)?;
    Ok(message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalOutcome {
    Sent,
    Missing,
}

impl SignalOutcome {
    fn is_missing(self) -> bool {
        self == Self::Missing
    }
}

fn ignore_missing_process_group(result: Result<()>) -> Result<SignalOutcome> {
    match result {
        Ok(()) => Ok(SignalOutcome::Sent),
        Err(err) if is_esrch(&err) => Ok(SignalOutcome::Missing),
        Err(err) => Err(err),
    }
}

fn is_esrch(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.raw_os_error() == Some(libc::ESRCH))
}

fn list_workspace_records(workspace_state_root: &Path) -> Result<Vec<ActiveTaskRecord>> {
    let mut records = Vec::new();
    let tasks_dir = workspace_state_root.join("tasks");
    if !tasks_dir.exists() {
        return Ok(records);
    }
    if !tasks_dir.is_dir() {
        bail!(
            "failed to scan current workspace tasks: '{}' is not a directory",
            tasks_dir.display()
        );
    }

    for task_entry in fs::read_dir(&tasks_dir)
        .with_context(|| format!("failed to read tasks directory '{}'", tasks_dir.display()))?
    {
        let task_entry = task_entry?;
        let task_path = task_entry.path();
        if !task_path.is_dir() {
            continue;
        }
        let record_path = active_record_path(&task_path);
        if record_path.is_file() {
            records.push(read_active_task_record(&record_path)?);
        }
    }
    records.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(records)
}

pub(crate) fn list_records(app_dir: &Path) -> Result<Vec<ActiveTaskRecord>> {
    let mut records = Vec::new();
    if !app_dir.exists() {
        return Ok(records);
    }

    for workspace_entry in fs::read_dir(app_dir).with_context(|| {
        format!(
            "failed to read cang state directory '{}'",
            app_dir.display()
        )
    })? {
        let workspace_entry = workspace_entry?;
        let workspace_path = workspace_entry.path();
        if !workspace_path.is_dir() || is_non_workspace_state_dir(workspace_path.file_name()) {
            continue;
        }
        let tasks_dir = workspace_path.join("tasks");
        if !tasks_dir.is_dir() {
            continue;
        }
        for task_entry in fs::read_dir(&tasks_dir)
            .with_context(|| format!("failed to read tasks directory '{}'", tasks_dir.display()))?
        {
            let task_entry = task_entry?;
            let task_path = task_entry.path();
            if !task_path.is_dir() {
                continue;
            }
            let record_path = active_record_path(&task_path);
            if record_path.is_file() {
                records.push(read_active_task_record(&record_path)?);
            }
        }
    }
    records.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(records)
}

fn is_non_workspace_state_dir(name: Option<&OsStr>) -> bool {
    matches!(
        name.and_then(OsStr::to_str),
        Some("microvm") | Some("sccache")
    )
}

fn render_task_table(rows: &[TaskRow]) -> String {
    let mut lines = vec![format!(
        "{:<24} {:<36} {:<10} {:>7} {:>7} {:>12}  {:<24} WORKSPACE",
        "HANDLE", "TASK ID", "STATUS", "PID", "SID", "STARTED", "IMAGE"
    )];
    for row in rows {
        let digest = row
            .record
            .image_digest
            .as_deref()
            .map(|digest| format!("@{digest}"))
            .unwrap_or_default();
        let handle = task_handle(&row.record.task_id).unwrap_or(&row.record.task_id);
        lines.push(format!(
            "{:<24} {:<36} {:<10} {:>7} {:>7} {:>12}  {:<24} {}",
            truncate(handle, 24),
            truncate(&row.record.task_id, 36),
            row.status.as_str(),
            row.record.process.pid,
            row.record.process.sid,
            row.record.started_at_unix_secs,
            truncate(&format!("{}{}", row.record.image_reference, digest), 24),
            row.record.workspace_slug,
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskRow {
    record: ActiveTaskRecord,
    status: ActiveTaskStatus,
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    let mut output = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn active_record_path(task_dir: &Path) -> PathBuf {
    task_dir.join(ACTIVE_RECORD_FILE)
}

fn write_active_task_record(record: &ActiveTaskRecord) -> Result<()> {
    fs::create_dir_all(&record.task_dir)
        .with_context(|| format!("failed to create '{}'", record.task_dir.display()))?;
    let path = active_record_path(&record.task_dir);
    fs::write(&path, encode_record(record))
        .with_context(|| format!("failed to write '{}'", path.display()))
}

fn read_active_task_record(path: &Path) -> Result<ActiveTaskRecord> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read active task record '{}'", path.display()))?;
    decode_record(&contents).with_context(|| format!("failed to parse '{}'", path.display()))
}

fn encode_record(record: &ActiveTaskRecord) -> String {
    let mut output = String::new();
    push_kv(&mut output, "version", "1");
    push_kv(&mut output, "task_id", &record.task_id);
    push_kv(&mut output, "workspace_slug", &record.workspace_slug);
    push_kv(
        &mut output,
        "workspace_dir",
        &record.workspace_dir.display().to_string(),
    );
    push_kv(
        &mut output,
        "task_dir",
        &record.task_dir.display().to_string(),
    );
    push_kv(&mut output, "image_reference", &record.image_reference);
    if let Some(digest) = &record.image_digest {
        push_kv(&mut output, "image_digest", digest);
    }
    push_kv(
        &mut output,
        "started_at_unix_secs",
        &record.started_at_unix_secs.to_string(),
    );
    push_kv(&mut output, "pid", &record.process.pid.to_string());
    push_kv(&mut output, "pgid", &record.process.pgid.to_string());
    push_kv(&mut output, "sid", &record.process.sid.to_string());
    push_kv(
        &mut output,
        "proc_start_time_ticks",
        &record.process.proc_start_time_ticks.to_string(),
    );
    push_kv(&mut output, "boot_id", &record.process.boot_id);
    if let Some(waypipe) = &record.waypipe {
        push_kv(
            &mut output,
            "waypipe_control_socket",
            &waypipe.control_socket.display().to_string(),
        );
    }
    if let Some(exec) = &record.exec {
        push_kv(
            &mut output,
            "exec_socket",
            &exec.socket.display().to_string(),
        );
        push_kv(&mut output, "exec_guest_port", &exec.guest_port.to_string());
        push_kv(
            &mut output,
            "exec_protocol_version",
            &exec.protocol_version.to_string(),
        );
    }
    if let Some(managed) = &record.managed {
        push_kv(
            &mut output,
            "managed_attach_socket",
            &managed.attach_socket.display().to_string(),
        );
        push_kv(
            &mut output,
            "managed_guest_port",
            &managed.guest_port.to_string(),
        );
        push_kv(
            &mut output,
            "managed_protocol_version",
            &managed.protocol_version.to_string(),
        );
    }
    output
}

fn push_kv(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push('=');
    output.push_str(value.replace('\n', " ").trim());
    output.push('\n');
}

fn decode_record(contents: &str) -> Result<ActiveTaskRecord> {
    let mut map = std::collections::BTreeMap::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("active task record line is missing '=': {line}");
        };
        map.insert(key.to_owned(), value.to_owned());
    }
    if required(&map, "version")? != "1" {
        bail!("unsupported active task record version");
    }
    Ok(ActiveTaskRecord {
        task_id: required(&map, "task_id")?.to_owned(),
        workspace_slug: required(&map, "workspace_slug")?.to_owned(),
        workspace_dir: PathBuf::from(required(&map, "workspace_dir")?),
        task_dir: PathBuf::from(required(&map, "task_dir")?),
        image_reference: required(&map, "image_reference")?.to_owned(),
        image_digest: map.get("image_digest").cloned(),
        started_at_unix_secs: parse_required(&map, "started_at_unix_secs")?,
        process: ProcessIdentity {
            pid: parse_required(&map, "pid")?,
            pgid: parse_required(&map, "pgid")?,
            sid: parse_required(&map, "sid")?,
            proc_start_time_ticks: parse_required(&map, "proc_start_time_ticks")?,
            boot_id: required(&map, "boot_id")?.to_owned(),
        },
        waypipe: parse_waypipe_record(&map)?,
        exec: parse_exec_record(&map)?,
        managed: parse_managed_record(&map)?,
    })
}

fn parse_waypipe_record(
    map: &std::collections::BTreeMap<String, String>,
) -> Result<Option<WaypipeTaskRecord>> {
    let Some(control_socket) = map.get("waypipe_control_socket") else {
        return Ok(None);
    };
    Ok(Some(WaypipeTaskRecord {
        control_socket: PathBuf::from(control_socket),
    }))
}

fn parse_exec_record(
    map: &std::collections::BTreeMap<String, String>,
) -> Result<Option<ExecTaskRecord>> {
    let keys = ["exec_socket", "exec_guest_port", "exec_protocol_version"];
    if !keys.iter().any(|key| map.contains_key(*key)) {
        return Ok(None);
    }
    Ok(Some(ExecTaskRecord {
        socket: PathBuf::from(required(map, "exec_socket")?),
        guest_port: parse_required(map, "exec_guest_port")?,
        protocol_version: parse_required(map, "exec_protocol_version")?,
    }))
}

fn parse_managed_record(
    map: &std::collections::BTreeMap<String, String>,
) -> Result<Option<ManagedTaskRecord>> {
    let has_any = map.contains_key("managed_attach_socket")
        || map.contains_key("managed_guest_port")
        || map.contains_key("managed_protocol_version");
    if !has_any {
        return Ok(None);
    }
    Ok(Some(ManagedTaskRecord {
        attach_socket: PathBuf::from(required(map, "managed_attach_socket")?),
        guest_port: parse_required(map, "managed_guest_port")?,
        protocol_version: parse_required(map, "managed_protocol_version")?,
    }))
}

fn required<'a>(map: &'a std::collections::BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    map.get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("active task record is missing '{key}'"))
}

fn parse_required<T>(map: &std::collections::BTreeMap<String, String>, key: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    required(map, key)?
        .parse::<T>()
        .with_context(|| format!("active task record field '{key}' has invalid value"))
}

fn read_process_identity(
    proc_root: &Path,
    boot_id_path: &Path,
    pid: u32,
    pgid: u32,
    sid: u32,
) -> Result<ProcessIdentity> {
    Ok(ProcessIdentity {
        pid,
        pgid,
        sid,
        proc_start_time_ticks: read_proc_start_time_ticks(proc_root, pid)?,
        boot_id: read_boot_id(boot_id_path)?,
    })
}

fn inspect_process_identity(
    proc_root: &Path,
    boot_id_path: &Path,
    identity: &ProcessIdentity,
) -> Result<ActiveTaskStatus> {
    if read_boot_id(boot_id_path)? != identity.boot_id {
        return Ok(ActiveTaskStatus::PidReused);
    }
    if !proc_root.join(identity.pid.to_string()).exists() {
        return Ok(ActiveTaskStatus::Stale);
    }
    let start_time = read_proc_start_time_ticks(proc_root, identity.pid)?;
    if start_time != identity.proc_start_time_ticks {
        return Ok(ActiveTaskStatus::PidReused);
    }
    let actual_pgid = unsafe { libc::getpgid(identity.pid as libc::pid_t) };
    if actual_pgid < 0 {
        return Ok(ActiveTaskStatus::Stale);
    }
    if u32::try_from(actual_pgid).ok() != Some(identity.pgid) {
        return Ok(ActiveTaskStatus::PidReused);
    }
    let actual_sid = unsafe { libc::getsid(identity.pid as libc::pid_t) };
    if actual_sid < 0 {
        return Ok(ActiveTaskStatus::Stale);
    }
    if u32::try_from(actual_sid).ok() != Some(identity.sid) {
        return Ok(ActiveTaskStatus::PidReused);
    }
    Ok(ActiveTaskStatus::Running)
}

fn read_proc_start_time_ticks(proc_root: &Path, pid: u32) -> Result<u64> {
    let stat_path = proc_root.join(pid.to_string()).join("stat");
    let stat = fs::read_to_string(&stat_path)
        .with_context(|| format!("failed to read '{}'", stat_path.display()))?;
    parse_proc_stat_start_time(&stat)
}

fn parse_proc_stat_start_time(stat: &str) -> Result<u64> {
    let after_comm = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .ok_or_else(|| anyhow!("/proc stat line is missing process command terminator"))?;
    let fields = after_comm.split_whitespace().collect::<Vec<_>>();
    let start_time = fields
        .get(19)
        .ok_or_else(|| anyhow!("/proc stat line is missing starttime field"))?;
    start_time
        .parse::<u64>()
        .context("/proc stat starttime is not an integer")
}

fn read_boot_id(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)
        .with_context(|| format!("failed to read '{}'", path.display()))?
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;

    #[derive(Debug)]
    struct StaticInspector {
        statuses: std::cell::RefCell<Vec<ActiveTaskStatus>>,
    }

    impl StaticInspector {
        fn new(statuses: Vec<ActiveTaskStatus>) -> Self {
            Self {
                statuses: std::cell::RefCell::new(statuses),
            }
        }
    }

    impl ProcessInspector for StaticInspector {
        fn status(&self, _identity: &ProcessIdentity) -> ActiveTaskStatus {
            self.statuses.borrow_mut().remove(0)
        }
    }

    #[derive(Debug, Default)]
    struct RecordingSignaler {
        signals: Vec<(u32, i32)>,
        fail_on_signal: Option<i32>,
    }

    impl TaskSignaler for RecordingSignaler {
        fn signal_process_group(&mut self, pgid: u32, signal: i32) -> Result<()> {
            self.signals.push((pgid, signal));
            if self.fail_on_signal == Some(signal) {
                return Err(anyhow!(std::io::Error::from_raw_os_error(libc::ESRCH)))
                    .with_context(|| format!("failed to send signal {signal}"));
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum CleanerOutcome {
        RemoveTaskDir,
        Fail(&'static str),
        RemoveActiveRecordThenFail(&'static str),
    }

    #[derive(Debug)]
    struct RecordingCleaner {
        calls: std::cell::RefCell<Vec<PathBuf>>,
        outcomes: std::cell::RefCell<VecDeque<CleanerOutcome>>,
    }

    impl RecordingCleaner {
        fn new(outcomes: Vec<CleanerOutcome>) -> Self {
            Self {
                calls: std::cell::RefCell::new(Vec::new()),
                outcomes: std::cell::RefCell::new(outcomes.into()),
            }
        }

        fn calls(&self) -> Vec<PathBuf> {
            self.calls.borrow().clone()
        }
    }

    impl TaskRootfsCleaner for RecordingCleaner {
        fn cleanup_task_rootfs(&self, task_dir: &Path) -> Result<()> {
            self.calls.borrow_mut().push(task_dir.to_path_buf());
            match self
                .outcomes
                .borrow_mut()
                .pop_front()
                .unwrap_or(CleanerOutcome::RemoveTaskDir)
            {
                CleanerOutcome::RemoveTaskDir => {
                    if task_dir.exists() {
                        fs::remove_dir_all(task_dir).with_context(|| {
                            format!("failed to remove fake task dir '{}'", task_dir.display())
                        })?;
                    }
                    Ok(())
                }
                CleanerOutcome::Fail(message) => bail!(message),
                CleanerOutcome::RemoveActiveRecordThenFail(message) => {
                    let _ = fs::remove_file(active_record_path(task_dir));
                    bail!(message);
                }
            }
        }
    }

    fn record(task_id: &str, task_dir: PathBuf) -> ActiveTaskRecord {
        ActiveTaskRecord {
            task_id: task_id.to_owned(),
            workspace_slug: "workspace-a".to_owned(),
            workspace_dir: PathBuf::from("/src/workspace-a"),
            task_dir,
            image_reference: "localhost/cang:latest".to_owned(),
            image_digest: Some("sha256:abc".to_owned()),
            started_at_unix_secs: 12,
            process: ProcessIdentity {
                pid: 123,
                pgid: 123,
                sid: 123,
                proc_start_time_ticks: 456,
                boot_id: "boot".to_owned(),
            },
            waypipe: None,
            exec: None,
            managed: None,
        }
    }

    #[test]
    fn active_record_round_trips_without_external_format_dependency() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_dir = dir.path().join("workspace-a/tasks/task-a");
        let original = record("task-a", task_dir.clone());

        write_active_task_record(&original).expect("write record");
        let decoded = read_active_task_record(&active_record_path(&task_dir)).expect("read record");

        assert_eq!(decoded, original);
    }

    #[test]
    fn active_record_round_trips_waypipe_capability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_dir = dir.path().join("workspace-a/tasks/task-a");
        let mut original = record("task-a", task_dir.clone());
        original.waypipe = Some(WaypipeTaskRecord {
            control_socket: PathBuf::from("/tmp/cang-1000/wc-0123456789abcdef.sock"),
        });

        write_active_task_record(&original).expect("write record");
        let decoded = read_active_task_record(&active_record_path(&task_dir)).expect("read record");

        assert_eq!(decoded, original);
    }

    #[test]
    fn active_record_round_trips_external_managed_attach_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_dir = dir.path().join("workspace-a/tasks/task-a");
        let mut original = record("task-a", task_dir.clone());
        original.managed = Some(ManagedTaskRecord {
            attach_socket: PathBuf::from("/tmp/cang-1000/a-0123456789abcdef.sock"),
            guest_port: 1025,
            protocol_version: 1,
        });

        write_active_task_record(&original).expect("write record");
        let decoded = read_active_task_record(&active_record_path(&task_dir)).expect("read record");

        assert_eq!(decoded, original);
    }

    #[test]
    fn list_records_scans_workspace_tasks_and_skips_app_support_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        fs::create_dir_all(app_dir.join("microvm/tasks/ignored")).expect("support dir");
        fs::create_dir_all(app_dir.join("sccache/tasks/ignored")).expect("support dir");

        let records = list_records(&app_dir).expect("list records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, "task-a");
    }

    #[test]
    fn ps_renders_human_readable_table_with_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");

        let output = render_ps(
            &app_dir,
            &StaticInspector::new(vec![ActiveTaskStatus::Running]),
        )
        .expect("render ps");

        assert!(output.contains("TASK ID"));
        assert!(output.contains("running"));
        assert!(output.contains("workspace-a"));
    }

    #[test]
    fn ps_renders_short_handle_column_for_new_task_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let task_id = "cang-4138-178109091122334455";
        write_active_task_record(&record(task_id, task_dir)).expect("write task record");

        let output = render_ps(
            &app_dir,
            &StaticInspector::new(vec![ActiveTaskStatus::Running]),
        )
        .expect("render ps");

        assert!(output.contains("HANDLE"));
        assert!(output.contains("cang-4138"));
        assert!(output.contains(task_id));
    }

    #[test]
    fn task_handle_extracts_workspace_pid_from_task_id() {
        assert_eq!(
            task_handle("cang-4138-178109091122334455"),
            Some("cang-4138")
        );
        assert_eq!(task_handle("cang-4138"), None);
        assert_eq!(task_handle("cang-4138-suffix"), None);
    }

    #[test]
    fn kill_accepts_exact_derived_short_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        let task_id = "cang-4138-178109091122334455";
        write_active_task_record(&record(task_id, task_dir)).expect("write task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "cang-4138", &inspector, &mut signaler, |_| {})
            .expect("kill task by handle");

        assert!(output.contains(task_id));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_accepts_unique_handle_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        let task_id = "cang-4138-178109091122334455";
        write_active_task_record(&record(task_id, task_dir)).expect("write task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output =
            kill_task(&app_dir, "ca", &inspector, &mut signaler, |_| {}).expect("kill by prefix");

        assert!(output.contains(task_id));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_accepts_unique_abbreviated_handle_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let target_dir = app_dir.join("workspace-a/tasks/target");
        let other_cang_dir = app_dir.join("workspace-b/tasks/other-cang");
        let muvm_dir = app_dir.join("workspace-c/tasks/muvm");
        let target_path = active_record_path(&target_dir);
        let other_cang_path = active_record_path(&other_cang_dir);
        let muvm_path = active_record_path(&muvm_dir);
        write_active_task_record(&record("cang-1845-111", target_dir))
            .expect("write target task record");
        write_active_task_record(&record("cang-8874-222", other_cang_dir))
            .expect("write other cang task record");
        write_active_task_record(&record("muvm-3871-333", muvm_dir))
            .expect("write muvm task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "ca-18", &inspector, &mut signaler, |_| {})
            .expect("kill by abbreviated handle selector");

        assert!(output.contains("cang-1845-111"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!target_path.exists());
        assert!(other_cang_path.exists());
        assert!(muvm_path.exists());
    }

    #[test]
    fn kill_accepts_numeric_prefix_in_abbreviated_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let cang_dir = app_dir.join("workspace-a/tasks/cang");
        let target_dir = app_dir.join("workspace-b/tasks/muvm");
        let cang_path = active_record_path(&cang_dir);
        let target_path = active_record_path(&target_dir);
        write_active_task_record(&record("cang-1845-111", cang_dir))
            .expect("write cang task record");
        write_active_task_record(&record("muvm-3871-222", target_dir))
            .expect("write target task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "mu-38", &inspector, &mut signaler, |_| {})
            .expect("kill by abbreviated numeric prefix");

        assert!(output.contains("muvm-3871-222"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(cang_path.exists());
        assert!(!target_path.exists());
    }

    #[test]
    fn kill_literal_hyphenated_handle_prefix_precedes_abbreviated_matching() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let literal_prefix_dir = app_dir.join("workspace-a/tasks/literal-prefix");
        let abbreviated_match_dir = app_dir.join("workspace-b/tasks/abbreviated-match");
        let literal_prefix_path = active_record_path(&literal_prefix_dir);
        let abbreviated_match_path = active_record_path(&abbreviated_match_dir);
        write_active_task_record(&record("ca-18-test-111", literal_prefix_dir))
            .expect("write literal-prefix task record");
        write_active_task_record(&record("cang-1845-222", abbreviated_match_dir))
            .expect("write abbreviated-match task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "ca-18", &inspector, &mut signaler, |_| {})
            .expect("literal prefix should win");

        assert!(output.contains("ca-18-test-111"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!literal_prefix_path.exists());
        assert!(abbreviated_match_path.exists());
    }

    #[test]
    fn kill_rejects_one_character_handle_prefix_even_when_unique() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-4138-178109091122334455", task_dir))
            .expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "c", &inspector, &mut signaler, |_| {})
            .expect_err("one-character prefix should not resolve");

        assert!(format!("{err:#}").contains("too short"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_rejects_too_short_abbreviated_name_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-1845-111", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "c-18", &inspector, &mut signaler, |_| {})
            .expect_err("too-short abbreviated name prefix should not resolve");

        assert!(format!("{err:#}").contains("name prefix that is too short"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_rejects_ambiguous_short_handle_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let first_dir = app_dir.join("workspace-a/tasks/task-a");
        let second_dir = app_dir.join("workspace-b/tasks/task-b");
        let first_path = active_record_path(&first_dir);
        let second_path = active_record_path(&second_dir);
        write_active_task_record(&record("cang-4138-111", first_dir))
            .expect("write first task record");
        write_active_task_record(&record("cang-4138-222", second_dir))
            .expect("write second task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "cang-4138", &inspector, &mut signaler, |_| {})
            .expect_err("ambiguous handle should refuse kill");
        let message = format!("{err:#}");

        assert!(message.contains("matched handle 'cang-4138'"));
        assert!(message.contains("cang-4138-111"));
        assert!(message.contains("cang-4138-222"));
        assert!(signaler.signals.is_empty());
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn kill_rejects_ambiguous_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let first_dir = app_dir.join("workspace-a/tasks/task-a");
        let second_dir = app_dir.join("workspace-b/tasks/task-b");
        let first_path = active_record_path(&first_dir);
        let second_path = active_record_path(&second_dir);
        write_active_task_record(&record("cang-3415-111", first_dir))
            .expect("write first task record");
        write_active_task_record(&record("cargo-2222-333", second_dir))
            .expect("write second task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca", &inspector, &mut signaler, |_| {})
            .expect_err("ambiguous prefix should refuse kill");
        let message = format!("{err:#}");

        assert!(message.contains("matched handle prefix 'ca'"));
        assert!(message.contains("cang-3415-111"));
        assert!(message.contains("cargo-2222-333"));
        assert!(signaler.signals.is_empty());
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn kill_rejects_ambiguous_abbreviated_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let first_dir = app_dir.join("workspace-a/tasks/task-a");
        let second_dir = app_dir.join("workspace-b/tasks/task-b");
        let first_path = active_record_path(&first_dir);
        let second_path = active_record_path(&second_dir);
        write_active_task_record(&record("cang-1845-111", first_dir))
            .expect("write first task record");
        write_active_task_record(&record("cargo-1800-222", second_dir))
            .expect("write second task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca-18", &inspector, &mut signaler, |_| {})
            .expect_err("ambiguous abbreviated selector should refuse kill");
        let message = format!("{err:#}");

        assert!(message.contains("matched abbreviated handle selector 'ca-18'"));
        assert!(message.contains("cang-1845-111"));
        assert!(message.contains("cargo-1800-222"));
        assert!(signaler.signals.is_empty());
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn kill_rejects_malformed_abbreviated_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let first_dir = app_dir.join("workspace-a/tasks/task-a");
        let second_dir = app_dir.join("workspace-b/tasks/task-b");
        let first_path = active_record_path(&first_dir);
        let second_path = active_record_path(&second_dir);
        write_active_task_record(&record("cang-1845-111", first_dir))
            .expect("write first task record");
        write_active_task_record(&record("muvm-3871-222", second_dir))
            .expect("write second task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca-x", &inspector, &mut signaler, |_| {})
            .expect_err("non-numeric abbreviated selector should not resolve");

        assert!(format!("{err:#}").contains("non-empty numeric displayed-handle segment prefix"));
        assert!(signaler.signals.is_empty());
        assert!(first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn kill_rejects_empty_numeric_abbreviated_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-1845-111", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca-", &inspector, &mut signaler, |_| {})
            .expect_err("empty numeric abbreviated selector should not resolve");

        assert!(format!("{err:#}").contains("non-empty numeric displayed-handle segment prefix"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_rejects_unmatched_abbreviated_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-1845-111", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca-99", &inspector, &mut signaler, |_| {})
            .expect_err("unmatched abbreviated selector should not resolve");

        assert!(format!("{err:#}").contains("no abbreviated handle selector matched 'ca-99'"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_rejects_compressed_handle_prefix_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-1845-111", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "ca18", &inspector, &mut signaler, |_| {})
            .expect_err("compressed selector should not resolve");

        assert!(format!("{err:#}").contains("id, handle, or handle prefix"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_rejects_full_id_prefix_that_is_not_a_handle_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("cang-3415-111", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![]);
        let mut signaler = RecordingSignaler::default();

        let err = kill_task(&app_dir, "cang-3415-1", &inspector, &mut signaler, |_| {})
            .expect_err("full id prefix should not resolve through handle prefix matching");

        assert!(format!("{err:#}").contains("id, handle, or handle prefix"));
        assert!(signaler.signals.is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn kill_exact_handle_wins_over_longer_handle_prefix_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let exact_handle_dir = app_dir.join("workspace-a/tasks/exact-handle");
        let prefix_dir = app_dir.join("workspace-b/tasks/prefix");
        let exact_handle_path = active_record_path(&exact_handle_dir);
        let prefix_path = active_record_path(&prefix_dir);
        write_active_task_record(&record("cang-4138-111", exact_handle_dir))
            .expect("write exact-handle task record");
        write_active_task_record(&record("cang-4138-extra-222", prefix_dir))
            .expect("write prefix task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "cang-4138", &inspector, &mut signaler, |_| {})
            .expect("exact handle should win");

        assert!(output.contains("cang-4138-111"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!exact_handle_path.exists());
        assert!(prefix_path.exists());
    }

    #[test]
    fn kill_exact_full_id_wins_over_handle_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let exact_dir = app_dir.join("workspace-a/tasks/exact");
        let handle_dir = app_dir.join("workspace-b/tasks/handle");
        let exact_path = active_record_path(&exact_dir);
        let handle_path = active_record_path(&handle_dir);
        write_active_task_record(&record("cang-4138", exact_dir)).expect("write exact task record");
        write_active_task_record(&record("cang-4138-111", handle_dir))
            .expect("write handle task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();

        let output = kill_task(&app_dir, "cang-4138", &inspector, &mut signaler, |_| {})
            .expect("exact task id should win");

        assert!(output.contains("cang-4138"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!exact_path.exists());
        assert!(handle_path.exists());
    }

    #[test]
    fn kill_sends_term_only_when_task_exits_during_grace_period() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();
        let slept = Cell::new(false);

        let output = kill_task(&app_dir, "task-a", &inspector, &mut signaler, |duration| {
            assert_eq!(duration, TERM_GRACE);
            slept.set(true);
        })
        .expect("kill task");

        assert!(slept.get());
        assert!(output.contains("SIGTERM"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_escalates_to_sigkill_when_task_remains_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Running,
        ]);
        let mut signaler = RecordingSignaler::default();

        let output =
            kill_task(&app_dir, "task-a", &inspector, &mut signaler, |_| {}).expect("kill task");

        assert!(output.contains("SIGKILL"));
        assert_eq!(
            signaler.signals,
            vec![(123, libc::SIGTERM), (123, libc::SIGKILL)]
        );
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_cleans_record_when_task_exits_between_post_grace_check_and_sigkill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Stale,
        ]);
        let mut signaler = RecordingSignaler::default();

        let output =
            kill_task(&app_dir, "task-a", &inspector, &mut signaler, |_| {}).expect("kill task");

        assert!(output.contains("SIGTERM"));
        assert!(!output.contains("SIGKILL"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_cleans_record_when_sigkill_loses_exit_race() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Running,
            ActiveTaskStatus::Running,
        ]);
        let mut signaler = RecordingSignaler {
            signals: Vec::new(),
            fail_on_signal: Some(libc::SIGKILL),
        };

        let output =
            kill_task(&app_dir, "task-a", &inspector, &mut signaler, |_| {}).expect("kill task");

        assert!(output.contains("SIGKILL"));
        assert_eq!(
            signaler.signals,
            vec![(123, libc::SIGTERM), (123, libc::SIGKILL)]
        );
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_cleans_record_when_sigterm_loses_exit_race() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir)).expect("write task record");
        let inspector = StaticInspector::new(vec![ActiveTaskStatus::Running]);
        let mut signaler = RecordingSignaler {
            signals: Vec::new(),
            fail_on_signal: Some(libc::SIGTERM),
        };

        let output =
            kill_task(&app_dir, "task-a", &inspector, &mut signaler, |_| {}).expect("kill task");

        assert!(output.contains("already exited"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert!(!record_path.exists());
    }

    #[test]
    fn kill_retries_cleanup_for_stale_task_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        fs::write(task_dir.join("rootfs-marker"), "rootfs").expect("task state");
        let inspector = StaticInspector::new(vec![ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::RemoveTaskDir]);

        let output = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect("stale task should retry cleanup");

        assert!(output.contains("already exited"));
        assert!(signaler.signals.is_empty());
        assert_eq!(cleaner.calls(), vec![task_dir.clone()]);
        assert!(!task_dir.exists());
    }

    #[test]
    fn kill_failure_after_process_termination_leaves_record_for_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::Fail("cleanup denied")]);

        let err = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect_err("cleanup failure should make kill fail closed");
        let message = format!("{err:#}");

        assert!(message.contains("cleanup denied"));
        assert!(message.contains("active task record remains for retry"));
        assert!(message.contains("rerun `cang kill task-a`"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert_eq!(cleaner.calls(), vec![task_dir]);
        assert!(record_path.exists());
    }

    #[test]
    fn kill_restores_record_when_partial_cleanup_removed_visibility() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let original = record("task-a", task_dir.clone());
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&original).expect("write task record");
        let inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::RemoveActiveRecordThenFail(
            "tree cleanup failed",
        )]);

        let err = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect_err("partial cleanup failure should fail closed");
        let message = format!("{err:#}");

        assert!(message.contains("tree cleanup failed"));
        assert!(message.contains("active task record remains for retry"));
        assert_eq!(signaler.signals, vec![(123, libc::SIGTERM)]);
        assert_eq!(
            read_active_task_record(&record_path).expect("restored record"),
            original
        );
    }

    #[test]
    fn stale_retry_succeeds_after_previous_cleanup_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        fs::write(task_dir.join("rootfs-marker"), "rootfs").expect("task state");

        let first_inspector =
            StaticInspector::new(vec![ActiveTaskStatus::Running, ActiveTaskStatus::Stale]);
        let mut first_signaler = RecordingSignaler::default();
        let first_cleaner = RecordingCleaner::new(vec![CleanerOutcome::Fail("busy")]);
        kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &first_inspector,
            &mut first_signaler,
            &first_cleaner,
            |_| {},
        )
        .expect_err("first cleanup should fail");
        assert!(record_path.exists());

        let retry_inspector = StaticInspector::new(vec![ActiveTaskStatus::Stale]);
        let mut retry_signaler = RecordingSignaler::default();
        let retry_cleaner = RecordingCleaner::new(vec![CleanerOutcome::RemoveTaskDir]);

        let output = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &retry_inspector,
            &mut retry_signaler,
            &retry_cleaner,
            |_| {},
        )
        .expect("stale retry should clean");

        assert!(output.contains("already exited"));
        assert!(retry_signaler.signals.is_empty());
        assert_eq!(retry_cleaner.calls(), vec![task_dir.clone()]);
        assert!(!task_dir.exists());
    }

    #[test]
    fn kill_pid_reused_task_cleans_state_without_signaling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        fs::write(task_dir.join("rootfs-marker"), "rootfs").expect("task state");
        let inspector = StaticInspector::new(vec![ActiveTaskStatus::PidReused]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::RemoveTaskDir]);

        let output = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect("pid-reused task should clean state only");

        assert!(output.contains("process id was reused"));
        assert!(output.contains("skipped signal"));
        assert!(signaler.signals.is_empty());
        assert_eq!(cleaner.calls(), vec![task_dir.clone()]);
        assert!(!task_dir.exists());
    }

    #[test]
    fn kill_pid_reused_cleanup_failure_leaves_record_for_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        let inspector = StaticInspector::new(vec![ActiveTaskStatus::PidReused]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::Fail("cleanup denied")]);

        let err = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect_err("pid-reused cleanup failure should leave retry record");
        let message = format!("{err:#}");

        assert!(message.contains("cleanup denied"));
        assert!(message.contains("active task record remains for retry"));
        assert!(message.contains("rerun `cang kill task-a`"));
        assert!(signaler.signals.is_empty());
        assert_eq!(cleaner.calls(), vec![task_dir]);
        assert!(record_path.exists());
    }

    #[test]
    fn kill_refuses_unreadable_task_without_signaling_or_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let task_dir = app_dir.join("workspace-a/tasks/task-a");
        let record_path = active_record_path(&task_dir);
        write_active_task_record(&record("task-a", task_dir.clone())).expect("write task record");
        let inspector = StaticInspector::new(vec![ActiveTaskStatus::Unreadable]);
        let mut signaler = RecordingSignaler::default();
        let cleaner = RecordingCleaner::new(vec![CleanerOutcome::RemoveTaskDir]);

        let err = kill_task_with_cleaner(
            &app_dir,
            "task-a",
            &inspector,
            &mut signaler,
            &cleaner,
            |_| {},
        )
        .expect_err("unreadable identity should refuse kill");

        assert!(format!("{err:#}").contains("process identity is unreadable"));
        assert!(signaler.signals.is_empty());
        assert!(cleaner.calls().is_empty());
        assert!(record_path.exists());
    }

    #[test]
    fn workspace_task_gate_scans_only_current_workspace_and_allows_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let current = dir.path().join("cang/workspace-a");
        let unrelated = dir.path().join("cang/workspace-b");
        write_active_task_record(&record("stale-a", current.join("tasks/stale-a")))
            .expect("write current stale");
        write_active_task_record(&record("running-b", unrelated.join("tasks/running-b")))
            .expect("write unrelated running");

        let report = ensure_workspace_has_no_running_tasks(
            &current,
            &StaticInspector::new(vec![ActiveTaskStatus::Stale]),
        )
        .expect("stale-only current workspace should not block");

        assert_eq!(report.stale_task_ids, ["stale-a"]);
    }

    #[test]
    fn workspace_task_gate_blocks_running_reused_and_unreadable_records() {
        for status in [
            ActiveTaskStatus::Running,
            ActiveTaskStatus::PidReused,
            ActiveTaskStatus::Unreadable,
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let workspace = dir.path().join("cang/workspace-a");
            write_active_task_record(&record("task-a", workspace.join("tasks/task-a")))
                .expect("write task");

            let err = ensure_workspace_has_no_running_tasks(
                &workspace,
                &StaticInspector::new(vec![status]),
            )
            .expect_err("unsafe task status should block");

            assert!(format!("{err:#}").contains(status.as_str()));
        }
    }

    #[test]
    fn workspace_task_gate_treats_record_read_failure_as_blocker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("cang/workspace-a");
        let task_dir = workspace.join("tasks/task-a");
        fs::create_dir_all(&task_dir).expect("task dir");
        fs::write(active_record_path(&task_dir), "not an active-task record").expect("bad record");

        let err = ensure_workspace_has_no_running_tasks(&workspace, &StaticInspector::new(vec![]))
            .expect_err("record read failure should block");

        assert!(format!("{err:#}").contains("failed to parse"));
    }

    #[test]
    fn launch_gate_refuses_a_workspace_with_a_running_task() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let workspace = PathBuf::from("/src/workspace-a");
        let mut running = record(
            "workspace-a-1234-1",
            app_dir.join("workspace-a/tasks/task-a"),
        );
        running.workspace_dir = workspace.clone();
        write_active_task_record(&running).expect("write task record");

        let err = ensure_workspace_has_no_running_task(
            &app_dir,
            &workspace,
            &StaticInspector::new(vec![ActiveTaskStatus::Running]),
        )
        .expect_err("a running task must block a second launch");
        let message = format!("{err:#}");

        assert!(message.contains("already has a running cang task"));
        assert!(message.contains("workspace-a-1234"));
        assert!(message.contains("handle workspace-a"));
        assert!(message.contains("cang kill"));
    }

    #[test]
    fn launch_gate_ignores_stale_records_and_other_workspaces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let workspace = PathBuf::from("/src/workspace-a");
        let mut stale = record(
            "workspace-a-1234-1",
            app_dir.join("workspace-a/tasks/task-a"),
        );
        stale.workspace_dir = workspace.clone();
        write_active_task_record(&stale).expect("write stale record");
        let mut other = record(
            "workspace-b-1234-1",
            app_dir.join("workspace-b/tasks/task-b"),
        );
        other.workspace_dir = PathBuf::from("/src/workspace-b");
        write_active_task_record(&other).expect("write other record");

        ensure_workspace_has_no_running_task(
            &app_dir,
            &workspace,
            &StaticInspector::new(vec![ActiveTaskStatus::Stale, ActiveTaskStatus::Running]),
        )
        .expect("stale records and other workspaces must not block a launch");
    }

    #[test]
    fn launch_gate_reports_the_task_id_when_no_handle_can_be_derived() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app_dir = dir.path().join("cang");
        let workspace = PathBuf::from("/src/workspace-a");
        let mut running = record("task-a", app_dir.join("workspace-a/tasks/task-a"));
        running.workspace_dir = workspace.clone();
        write_active_task_record(&running).expect("write task record");

        let err = ensure_workspace_has_no_running_task(
            &app_dir,
            &workspace,
            &StaticInspector::new(vec![ActiveTaskStatus::Running]),
        )
        .expect_err("a running task must block a second launch");

        assert!(format!("{err:#}").contains("handle task-a"));
    }

    #[test]
    fn workspace_task_gate_treats_scan_failure_as_blocker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("cang/workspace-a");
        fs::create_dir_all(&workspace).expect("workspace dir");
        fs::write(workspace.join("tasks"), "not a directory").expect("tasks file");

        let err = ensure_workspace_has_no_running_tasks(&workspace, &StaticInspector::new(vec![]))
            .expect_err("scan failure should block");

        assert!(format!("{err:#}").contains("not a directory"));
    }

    #[test]
    fn proc_stat_parser_handles_process_names_with_spaces() {
        let stat = "123 (name with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";

        assert_eq!(parse_proc_stat_start_time(stat).unwrap(), 98765);
    }
}
