//! The guest's responsiveness policy: the switch the levers share, and the record
//! of what it decided.
//!
//! A guest is RAM-bound and has no disk swap, so `cang-guest-init` trades some
//! build throughput for latency and survival: it keeps the workload off the two
//! cores it reserves (`runtime::cpu_bound`), gives the shell a default compiler
//! count to match, and - as later tickets land - tunes the guest's memory policy.
//! This module is the one place that says whether those *levers* apply and writes
//! down what was applied.
//!
//! The OOM guard is deliberately not here. `process::protect_from_oom` keeps the
//! processes the guest cannot lose out of the OOM killer's reach; without it the
//! guest loses its only victim and panics for want of one
//! (`mm/oom_kill.c:1159-1166`). A lever trades throughput for something an operator
//! may not want; the guard only prevents a crash, so `CANG_GUEST_RESPONSIVE=0` does
//! not reach it.
//!
//! Default on, `CANG_GUEST_RESPONSIVE=0` (or `false`/`no`/`off`) turns the levers
//! off, and an unrecognised value keeps the default - the same rule the host's own
//! `parse_env_flag` and `CANG_MEMORY_RECLAIM` follow, so a typo cannot silently
//! switch a guest's safety net either way. Evidence for the levers themselves:
//! `docs/wayfinder/guest-responsiveness/`.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::guest_init::runtime::cpu_bound;

/// Opt-out switch for the responsiveness levers, resolved per process.
pub(in crate::guest_init) const ENABLE_ENV: &str = "CANG_GUEST_RESPONSIVE";

/// Where the guest records what the policy decided.
pub(in crate::guest_init) const STATUS_PATH: &str = "/run/cang/responsive.status";

/// Whether the levers apply, for a value already read from the environment.
///
/// Absent or empty means on; the four off-spellings mean off; anything else is a
/// value nobody meant, so the default stands.
pub(in crate::guest_init) fn enabled_from(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") => true,
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

/// Whether the levers apply in this process.
pub(in crate::guest_init) fn enabled() -> bool {
    enabled_from(std::env::var(ENABLE_ENV).ok().as_deref())
}

/// The status file's body: what the policy decided, in the `key=value` shape
/// `/run/cang/swap.status` and the `/nix` prep status already use.
///
/// It is written once at boot so an arm can tell why a guest's `nproc` is what it
/// is, without inferring it from the mask.
pub(in crate::guest_init) fn status_body(enabled: bool, cpus: usize) -> String {
    let reserved = if enabled {
        cpu_bound::reserved_cores(cpus)
    } else {
        0
    };
    let mask = match cpu_bound::workload_cpu_range(cpus) {
        Some((start, end)) if enabled => format!("{start}-{}", end.saturating_sub(1)),
        _ => "all".to_owned(),
    };
    let jobs = if enabled {
        cpu_bound::workload_job_count(cpus)
    } else {
        None
    };
    let mut body = String::new();
    body.push_str(&format!("state={}\n", if enabled { "on" } else { "off" }));
    body.push_str(&format!("cpus={cpus}\n"));
    body.push_str(&format!("reserved={reserved}\n"));
    body.push_str(&format!("mask={mask}\n"));
    match jobs {
        Some(jobs) => body.push_str(&format!("cargo_build_jobs={jobs}\n")),
        None => body.push_str("cargo_build_jobs=\n"),
    }
    body
}

/// Write the status file, best effort: a guest that cannot record the policy still
/// applies it.
pub(in crate::guest_init) fn write_status() {
    if let Err(err) = write_status_at(
        Path::new(STATUS_PATH),
        enabled(),
        cpu_bound::guest_cpu_count(),
    ) {
        eprintln!("cang-guest-init: responsiveness policy not recorded: {err:#}");
    }
}

fn write_status_at(path: &Path, enabled: bool, cpus: usize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| anyhow::anyhow!("failed to create {}: {err}", parent.display()))?;
    }
    std::fs::write(path, status_body(enabled, cpus))
        .map_err(|err| anyhow::anyhow!("failed to write {}: {err}", path.display()))
}

/// Where the early responder's own output goes.
pub(in crate::guest_init) const EARLYOOM_LOG_PATH: &str = "/run/cang/earlyoom.log";

/// `MemAvailable` percentage at which the early responder acts.
///
/// The measured ladder ([note 01](../../../../../docs/wayfinder/guest-responsiveness/notes/01-memory-failure-ladder.md)
/// §3) put the wall two seconds after `MemAvailable` fell through ~96 MB of 4 GiB
/// (2.4%) with the page cache already gone, and a plain build storm on a 2 GiB
/// guest bottoms out around 25% - so the trigger has to sit well below the level a
/// healthy build works at and above the point where reclaim has nothing left.
/// 10% is earlyoom's own default and the one the arms' numbers support.
const EARLYOOM_MEM_PERCENT: &str = "10";

/// The early responder's arguments.
///
/// `-s 100` ignores swap entirely: this guest's swap is zram, whose `SwapFree`
/// drains as pages park in it, so "free swap" is not a pressure signal here - the
/// kernel's problem is the compressed budget, not the device's address space
/// (measured: 3738 MB of 3932 MB of slots in use for 983 MB of stored pages).
///
/// earlyoom's own `-p` is deliberately **not** passed: it would set the
/// responder's `oom_score_adj` to -100, undoing the -1000 its `pre_exec` hook
/// writes, because the program applies its flags after it starts. The niceness
/// that `-p` would buy is set in the same hook, where it costs one system call
/// and cannot overwrite the score.
const EARLYOOM_ARGS: [&str; 4] = ["-m", EARLYOOM_MEM_PERCENT, "-s", "100"];

/// The responder's niceness, the priority earlyoom's `-p` would have set.
const RESPONDER_NICE: libc::c_int = -20;

/// The responder's binary name; the image provides it on `PATH`.
const EARLYOOM_BINARY: &str = "earlyoom";

/// Start the guest's early out-of-memory responder, if the levers are on and the
/// image has it.
///
/// The kernel's own killer runs after reclaim has already thrashed, and on this
/// device that means millions of failed page scans and store refusals first
/// ([note 01](../../../../../docs/wayfinder/guest-responsiveness/notes/01-memory-failure-ladder.md)
/// §3-§4). earlyoom kills the largest consumer while `MemAvailable` is still
/// healthy, so the guest never reaches the wall. It skips any process at
/// `oom_score_adj` -1000, which is exactly the set `process::protect_from_oom`
/// maintains, so the guard and the responder share one definition of "the guest
/// cannot lose this".
///
/// Best effort: a guest without the binary, or without permission to raise the
/// score, still runs - it just falls back to the kernel's own killer.
pub(in crate::guest_init) fn start_early_responder() {
    if !enabled() {
        return;
    }
    if let Err(err) = spawn_early_oom(
        EARLYOOM_BINARY,
        &EARLYOOM_ARGS,
        Path::new(EARLYOOM_LOG_PATH),
    ) {
        eprintln!("cang-guest-init: early OOM responder not started: {err:#}");
    }
}

fn spawn_early_oom(binary: &str, args: &[&str], log_path: &Path) -> anyhow::Result<()> {
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
        .map_err(|err| anyhow::anyhow!("failed to clone the responder's log handle: {err}"))?;
    // SAFETY: `pre_exec` runs between `fork` and `exec` in the child, where only
    // async-signal-safe work is available: `setsid`, then `open`/`write`/`close` of
    // a static string into the child's own `/proc/self/oom_score_adj`. Writing it
    // here rather than from the parent is what keeps the `-1000` off the parent's
    // own score - protecting *that* would hand the inherited score to the shell it
    // execs, which is the defect ticket 12 fixed.
    let mut child = unsafe {
        Command::new(binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .pre_exec(|| {
                libc::setsid();
                restrict_self_from_oom()
            })
            .spawn()
    }
    .map_err(|err| anyhow::anyhow!("failed to spawn {binary}: {err}"))?;
    match child.try_wait() {
        Ok(None) => Ok(()),
        Ok(Some(status)) => Err(anyhow::anyhow!(
            "{binary} exited immediately with {status} (see {})",
            log_path.display()
        )),
        Err(err) => Err(anyhow::anyhow!("failed to poll {binary}: {err}")),
    }
}

/// Take the calling process out of the OOM killer's reach, with no allocation.
///
/// This is the `pre_exec` half of [`start_early_responder`]: the score has to be
/// the responder's own, set in its own address space before it becomes
/// `earlyoom`, because a parent writing `/proc/<child>/oom_score_adj` would race
/// the child's lifetime and - if it protected itself instead - hand the inherited
/// `-1000` to whatever guest-init later `exec`s.
///
/// The score the responder gives itself: `OOM_SCORE_ADJ_MIN`, out of reach of the
/// killer it races and of the kernel's own.
const RESPONDER_OOM_SCORE: &[u8] = b"-1000\n";
/// The responder's own score file, NUL-terminated for `open(2)`.
const RESPONDER_SCORE_PATH: &[u8] = b"/proc/self/oom_score_adj\0";

/// # Safety
///
/// Called between `fork` and `exec`: [`write_oom_score`] uses `open`, `write` and
/// `close` only, so it is async-signal-safe.
unsafe fn restrict_self_from_oom() -> std::io::Result<()> {
    // SAFETY: `setpriority` on pid 0 takes this child's own priority and is
    // async-signal-safe; the caller guarantees the post-fork context.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, RESPONDER_NICE) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the caller guarantees the post-fork context, and both arguments are
    // static buffers.
    unsafe { write_oom_score(RESPONDER_SCORE_PATH, RESPONDER_OOM_SCORE) }
}

/// Write `score` into `path`, with no allocation: the syscall sequence only.
///
/// Split out from [`restrict_self_from_oom`] so the plumbing - a NUL-terminated
/// path, the bytes written, the descriptor closed - is testable outside a `fork`.
/// The *value* is not: `-1000` is refused with `EACCES` for a process that does
/// not hold `CAP_SYS_RESOURCE` (`fs/proc/base.c:1130-1136`), which is why the
/// responder is spawned by the root guest-init and the live arm is the claim for
/// the protected case.
///
/// # Safety
///
/// `path` must be NUL-terminated; both arguments must outlive the call. Safe to use
/// between `fork` and `exec`.
unsafe fn write_oom_score(path: &[u8], score: &[u8]) -> std::io::Result<()> {
    // SAFETY: the path is NUL-terminated and `open`/`write`/`close` are
    // async-signal-safe.
    unsafe {
        let fd = libc::open(path.as_ptr() as *const libc::c_char, libc::O_WRONLY);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let written = libc::write(fd, score.as_ptr() as *const libc::c_void, score.len());
        libc::close(fd);
        if written < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The switch's whole matrix: absent and empty mean on, the four off-spellings
    /// mean off, and a value nobody meant keeps the default rather than guessing.
    #[test]
    fn only_an_explicit_off_disables_the_levers() {
        assert!(enabled_from(None));
        assert!(enabled_from(Some("")));
        assert!(enabled_from(Some("  ")));
        for off in ["0", "false", "FALSE", "No", " off ", "OFF"] {
            assert!(!enabled_from(Some(off)), "{off:?} must disable");
        }
        for on in ["1", "true", "yes", "on", "enabled", "banana"] {
            assert!(enabled_from(Some(on)), "{on:?} must keep the default");
        }
    }

    /// The record names the cores, the mask and the job count the workload will
    /// actually get, so an arm can read the policy instead of inferring it.
    #[test]
    fn the_status_file_reports_the_policy_it_applied() {
        let on = status_body(true, 26);
        assert!(on.contains("state=on"), "{on}");
        assert!(on.contains("cpus=26"), "{on}");
        assert!(on.contains("reserved=2"), "{on}");
        assert!(on.contains("mask=0-23"), "{on}");
        assert!(on.contains("cargo_build_jobs=24"), "{on}");

        let off = status_body(false, 26);
        assert!(off.contains("state=off"), "{off}");
        assert!(off.contains("reserved=0"), "{off}");
        assert!(off.contains("mask=all"), "{off}");
        assert!(off.contains("cargo_build_jobs=\n"), "{off}");
    }

    /// The small-guest edges, where the rule reserves nothing or one core.
    #[test]
    fn the_status_file_matches_the_reserve_on_small_guests() {
        let two = status_body(true, 2);
        assert!(two.contains("reserved=1"), "{two}");
        assert!(two.contains("mask=0-0"), "{two}");
        assert!(two.contains("cargo_build_jobs=1"), "{two}");

        let one = status_body(true, 1);
        assert!(one.contains("state=on"), "{one}");
        assert!(one.contains("reserved=0"), "{one}");
        assert!(one.contains("mask=all"), "{one}");
    }

    #[test]
    fn writing_the_status_creates_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run-cang/responsive.status");

        write_status_at(&path, true, 26).expect("status should be written");

        let body = std::fs::read_to_string(&path).expect("status file");
        assert!(body.contains("mask=0-23"), "{body}");
    }

    #[test]
    fn a_status_path_that_cannot_be_written_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("blocked/responsive.status");
        std::fs::create_dir_all(dir.path().join("blocked")).expect("dir");
        // A directory where the file should go: the write fails, and the caller
        // reports rather than failing the boot.
        std::fs::create_dir_all(&path).expect("path as dir");
        let err = write_status_at(&path, true, 26).expect_err("a directory must fail");
        assert!(format!("{err:#}").contains("responsive.status"), "{err:#}");
    }

    /// The responder's arguments are the ones the ladder justifies: a percentage
    /// trigger, swap ignored (zram's `SwapFree` is not a pressure signal here), and
    /// its own priority raised so it can act inside the ~2 s window.
    #[test]
    fn the_responder_ignores_swap_and_acts_below_a_healthy_working_level() {
        assert_eq!(
            EARLYOOM_ARGS,
            ["-m", EARLYOOM_MEM_PERCENT, "-s", "100"],
            "the trigger and the flags are the measured recommendation"
        );
        assert!(
            !EARLYOOM_ARGS.contains(&"-p"),
            "`-p` would reset the responder's own score to -100 after it starts, undoing the -1000"
        );
        assert_eq!(
            RESPONDER_NICE, -20,
            "and it keeps the priority `-p` would have set"
        );
        let percent: u32 = EARLYOOM_MEM_PERCENT.parse().expect("a percentage");
        // A plain build bottoms out near 25% of guest RAM (note 01 §1) and the wall
        // arrives under 3%, so anything at or above 25 would fire on a healthy
        // build.
        assert!(percent < 25, "the trigger must sit below a healthy build");
        assert!(percent >= 5, "and above the point where nothing is left");
    }

    /// The responder protects *itself*, in its own address space, and the score it
    /// writes is `OOM_SCORE_ADJ_MIN` - the same value `process::protect_from_oom`
    /// gives the supervisor, so one definition covers both kinds of process the
    /// guest cannot lose.
    ///
    /// The plumbing is exercised against a file rather than `/proc/self`: lowering
    /// a real score below `oom_score_adj_min` is refused without
    /// `CAP_SYS_RESOURCE` (`fs/proc/base.c:1130-1136`), which an unprivileged test
    /// runner does not hold - that is exactly why the responder is spawned by the
    /// root guest-init, and the protected case is the live arm's claim.
    #[test]
    fn the_responder_writes_its_own_score_with_no_room_for_the_parent() {
        assert_eq!(
            RESPONDER_OOM_SCORE, b"-1000\n",
            "the responder is out of the killer's reach, like the supervisor"
        );
        assert_eq!(
            RESPONDER_SCORE_PATH, b"/proc/self/oom_score_adj\0",
            "and it writes its own score, never a child's or a parent's"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("oom_score_adj");
        // The real target always exists (`/proc/self/oom_score_adj`); `O_WRONLY`
        // without `O_CREAT` is deliberate, so the file has to be there first.
        std::fs::write(&target, "0\n").expect("seed the score file");
        let target_path = format!("{}\0", target.display());
        // SAFETY: the path is NUL-terminated and every argument outlives the call;
        // this is the same syscall sequence the `pre_exec` hook runs.
        unsafe { write_oom_score(target_path.as_bytes(), b"7\n").expect("the write must land") };
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "7\n");
    }

    /// A write the kernel refuses is reported, not swallowed: that is how a guest
    /// whose responder could not protect itself says so instead of running exposed.
    #[test]
    fn a_refused_score_write_is_an_error() {
        // SAFETY: the path is NUL-terminated and every argument outlives the call.
        let err = unsafe { write_oom_score(b"/nonexistent-oom/score\0", b"-1000\n") }
            .expect_err("a missing path must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
