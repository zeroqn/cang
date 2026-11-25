//! The guest's sched_ext scheduler: the switch, the start, and the record.
//!
//! The guest kernel is built with `CONFIG_SCHED_CLASS_EXT`, and the image
//! carries `scx_bpfland`, which cang starts as root before the workload exists so
//! the workload's wakeup latency is decided by bpfland's earlier-deadline policy
//! rather than by stock EEVDF. The scheduler is a separate process, so a guest
//! whose firmware predates `CONFIG_SCHED_CLASS_EXT`, or whose image has no
//! `scx_bpfland`, still boots: the start is best effort and the status file says
//! which of those happened.
//!
//! A scheduler that fails while it is loaded is not a wedge: sched_ext reverts to
//! the fair scheduler on its own (`Documentation/scheduler/sched-ext.rst`), so
//! this module never waits for, or tears down, the process it started.
//!
//! Default on, `CANG_GUEST_SCX=0` (or `false`/`no`/`off`) turns it off, and an
//! unrecognised value keeps the default - the rule `CANG_GUEST_RESPONSIVE` and the
//! host's own `parse_env_flag` follow, so a typo cannot silently change a guest's
//! scheduling. The host's half is `crates/cang/src/runtime/session/scx.rs`, driven
//! by `--scx`.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Opt-out switch for the guest's sched_ext scheduler, resolved per process.
pub(in crate::guest_init) const ENABLE_ENV: &str = "CANG_GUEST_SCX";

/// Where the guest records what the scheduler start decided.
pub(in crate::guest_init) const STATUS_PATH: &str = "/run/cang/scx.status";

/// Where the scheduler's own output goes.
pub(in crate::guest_init) const LOG_PATH: &str = "/run/cang/scx.log";

/// The scheduler binary name; the image provides it on `PATH`.
const SCHEDULER_BINARY: &str = "scx_bpfland";

/// sched_ext's own state, the mount point kernel exposes when the class is built.
const SCHED_EXT_DIR: &str = "/sys/kernel/sched_ext";

/// Whether the scheduler applies, for a value already read from the environment.
pub(in crate::guest_init) fn enabled_from(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") => true,
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

/// Whether the scheduler applies in this process.
pub(in crate::guest_init) fn enabled() -> bool {
    enabled_from(std::env::var(ENABLE_ENV).ok().as_deref())
}

/// What the start decided, before anything is written or spawned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The kernel was not built with `CONFIG_SCHED_CLASS_EXT`.
    NoSchedExt,
    /// Another scheduler is already driving the guest.
    AlreadyLoaded,
    /// Start the scheduler.
    Start,
}

/// Start the scheduler if this guest can and should.
///
/// Best effort by design: a guest without sched_ext in its firmware, without the
/// binary in its image, or without permission to load a BPF program still runs -
/// it just keeps the fair scheduler.
pub(in crate::guest_init) fn start() {
    if !enabled() {
        write_status("state=off\nreason=requested\n");
        return;
    }
    let decision = decide(
        Path::new(SCHED_EXT_DIR).exists(),
        current_state(Path::new(SCHED_EXT_DIR)).as_deref(),
    );
    match decision {
        Decision::NoSchedExt => {
            write_status("state=skipped\nreason=kernel-without-sched-ext\n");
        }
        Decision::AlreadyLoaded => {
            write_status("state=skipped\nreason=already-loaded\n");
        }
        Decision::Start => match spawn_scheduler(SCHEDULER_BINARY, Path::new(LOG_PATH)) {
            Ok(pid) => write_status(&status_body(pid)),
            Err(err) => {
                eprintln!("cang-guest-init: sched_ext scheduler not started: {err:#}");
                write_status(&format!("state=failed\nreason={err}\n"));
            }
        },
    }
}

/// The decision, from the two facts the guest can read before spawning.
fn decide(sched_ext_present: bool, state: Option<&str>) -> Decision {
    if !sched_ext_present {
        return Decision::NoSchedExt;
    }
    if state == Some("enabled") {
        return Decision::AlreadyLoaded;
    }
    Decision::Start
}

/// The scheduler's own `state`, when sched_ext exposes it.
fn current_state(sched_ext_dir: &Path) -> Option<String> {
    std::fs::read_to_string(sched_ext_dir.join("state"))
        .ok()
        .map(|text| text.trim().to_owned())
}

/// The status file's body after a start: the interface sched_ext switched to.
fn status_body(pid: u32) -> String {
    format!("state=started\npid={pid}\nswitch_all=1\n")
}

fn write_status(body: &str) {
    let path = Path::new(STATUS_PATH);
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "cang-guest-init: failed to create {}: {err}",
            parent.display()
        );
        return;
    }
    if let Err(err) = std::fs::write(path, body) {
        eprintln!("cang-guest-init: failed to write {}: {err}", path.display());
    }
}

/// Spawn the scheduler detached from this process's lifetime and session.
///
/// Detaching is what lets the scheduler outlive the guest-init process that
/// `exec`s into the workload, and the log is opened before the fork so a failure
/// to open it is reported rather than lost in the child.
fn spawn_scheduler(binary: &str, log_path: &Path) -> anyhow::Result<u32> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| anyhow::anyhow!("failed to create {}: {err}", parent.display()))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|err| anyhow::anyhow!("failed to open {}: {err}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|err| anyhow::anyhow!("failed to clone the scheduler's log handle: {err}"))?;
    // SAFETY: `pre_exec` runs between `fork` and `exec` in the child, where only
    // async-signal-safe work is available; `setsid` is one system call.
    let mut child = unsafe {
        Command::new(binary)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .pre_exec(|| {
                libc::setsid();
                Ok(())
            })
            .spawn()
    }
    .map_err(|err| anyhow::anyhow!("failed to spawn {binary}: {err}"))?;
    match child.try_wait() {
        Ok(None) => Ok(child.id()),
        Ok(Some(status)) => Err(anyhow::anyhow!(
            "{binary} exited immediately with {status} (see {})",
            log_path.display()
        )),
        Err(err) => Err(anyhow::anyhow!("failed to poll {binary}: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_off_spellings_turn_the_scheduler_off() {
        assert!(enabled_from(None));
        assert!(enabled_from(Some("")));
        assert!(
            enabled_from(Some("banana")),
            "a value nobody meant keeps the default on"
        );
        assert!(!enabled_from(Some("0")));
        assert!(!enabled_from(Some(" OFF ")));
        assert!(!enabled_from(Some("False")));
    }

    /// A guest whose firmware predates the pin bump must not report a failure: it
    /// has no `/sys/kernel/sched_ext`, and that is the `skipped` reason.
    #[test]
    fn a_kernel_without_sched_ext_is_skipped_rather_than_failed() {
        assert_eq!(decide(false, None), Decision::NoSchedExt);
    }

    /// A second start in the same guest - the state a caller inspecting a live
    /// guest sees - must not spawn a second scheduler.
    #[test]
    fn an_already_loaded_scheduler_is_not_started_again() {
        assert_eq!(decide(true, Some("enabled")), Decision::AlreadyLoaded);
        assert_eq!(decide(true, Some("disabled")), Decision::Start);
        assert_eq!(decide(true, None), Decision::Start);
    }

    #[test]
    fn the_status_file_records_the_interface_that_switched() {
        assert_eq!(status_body(4242), "state=started\npid=4242\nswitch_all=1\n");
    }
}
