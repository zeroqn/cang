//! The workload's CPU bound: the cores the guest reserves so a build cannot
//! take every vCPU.
//!
//! A guest is given its host's vCPUs and cargo reads every one of them, so a
//! build storm leaves no core for anything else. The rule is to keep two of them
//! out of the workload's reach - one when the guest has only two, none when it
//! has one - by narrowing the workload's affinity mask rather than by a cgroup
//! quota: a mask actually leaves the cores *idle* (measured 2.0 % and 4.1 % busy
//! against the control's least-busy 87.8 %), while `cpu.max` merely spreads the
//! cap over every CPU (least-busy 73 %) and a whole-guest quota cost 2.15x the
//! build's wall time. Affinity also inherits across `fork` and `exec`, so every
//! `rustc` the build starts stays inside it, and cargo's own job count follows
//! the mask, which lowers peak anonymous memory by 13 %. Evidence:
//! `docs/wayfinder/guest-responsiveness/notes/04-cpu-bound.md`.
//!
//! The bound applies where a guest-init process *becomes* the workload - the
//! managed session's PTY child, an exec'd command, and the `as-dev` exec - and
//! never to the session supervisor or the listener threads, which are the nice
//! `-20` interactive path this reserve exists to protect.

use std::io;
use std::path::Path;

/// How many cores the workload may not use, for a guest with `cpus` vCPUs.
///
/// Two cores are worth reserving from three upwards; a two-core guest can only
/// spare one, and a one-core guest has nothing to spare.
pub(in crate::guest_init) fn reserved_cores(cpus: usize) -> usize {
    match cpus {
        0 | 1 => 0,
        2 => 1,
        _ => 2,
    }
}

/// The half-open range of CPUs the workload is confined to, or `None` when the
/// guest is too small to reserve anything.
pub(in crate::guest_init) fn workload_cpu_range(cpus: usize) -> Option<(usize, usize)> {
    let reserved = reserved_cores(cpus);
    (reserved > 0 && cpus > reserved).then(|| (0, cpus - reserved))
}

/// The default build job count: one compiler per CPU the workload was left.
///
/// `None` when no cores are reserved, because then the guest's own `nproc` is
/// already the right answer and forcing a value would only overwrite a user's.
pub(in crate::guest_init) fn workload_job_count(cpus: usize) -> Option<usize> {
    workload_cpu_range(cpus).map(|(start, end)| end - start)
}

/// The guest's usable vCPU count, as the affinity mask sees it.
/// The CPUs the guest kernel has online.
pub(in crate::guest_init) const ONLINE_CPUS_PATH: &str = "/sys/devices/system/cpu/online";

/// The guest's usable vCPU count - the CPUs the kernel has, **not** the ones this
/// process is allowed to run on.
///
/// `sysconf(_SC_NPROCESSORS_ONLN)` is the affinity count on musl, so a process that
/// is already confined reports its confined count, and deriving a mask from that
/// reserves another two cores every time it happens. That is not hypothetical: a
/// live arm caught the default launch reading `0-21` - 0-23 from the session child,
/// then 22 vCPUs -> `0..22` from the re-derivation inside it
/// ([note 14](../../../../../docs/wayfinder/guest-responsiveness/notes/14-nix-build-bound.md)).
/// Read the kernel's own online list instead, and fall back to `sysconf` only if
/// that file is unreadable.
///
/// May allocate (it reads a file), so it belongs in a parent, before the `fork`.
pub(in crate::guest_init) fn guest_cpu_count() -> usize {
    online_cpu_count_from(Path::new(ONLINE_CPUS_PATH))
        .or_else(|| {
            let count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
            usize::try_from(count).ok()
        })
        .unwrap_or(0)
}

/// Parse a kernel CPU list (`"0-25"`, `"0"`, `"0-3,6-7"`) into a count.
fn online_cpu_count_from(path: &Path) -> Option<usize> {
    parse_cpu_list(std::fs::read_to_string(path).ok()?.trim())
}

/// Count the CPUs a kernel list names. A malformed list is `None` rather than a
/// guess, so the caller can fall back to `sysconf`.
fn parse_cpu_list(list: &str) -> Option<usize> {
    let mut count = 0usize;
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return None;
        }
        match item.split_once('-') {
            Some((first, last)) => {
                let first: usize = first.parse().ok()?;
                let last: usize = last.parse().ok()?;
                count = count.checked_add(last.checked_sub(first)?.checked_add(1)?)?;
            }
            None => {
                item.parse::<usize>().ok()?;
                count = count.checked_add(1)?;
            }
        }
    }
    (count > 0).then_some(count)
}

/// Whether this process may already run on every CPU inside `start..end` and on no
/// other: the idempotence check, one `sched_getaffinity`.
fn already_within(start: usize, end: usize) -> io::Result<bool> {
    let mut current: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: pid 0 is this process and the set is correctly sized.
    let rc =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut current) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    for cpu in 0..libc::CPU_SETSIZE as usize {
        // SAFETY: `CPU_ISSET` only reads the set.
        let set = unsafe { libc::CPU_ISSET(cpu, &current) };
        let inside = cpu >= start && cpu < end;
        if set != inside {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Confine the calling process, reporting a failure instead of failing the caller./// The confinement a child should apply, resolved in the parent.
///
/// Between `fork` and `exec` only async-signal-safe work is legal, and neither
/// reading the environment (`responsive::enabled`) nor calling `sysconf` is, so the
/// parent decides once and the child applies it with [`apply_workload_range`],
/// which allocates nothing and makes one system call.
pub(in crate::guest_init) fn planned_workload_range() -> Option<(usize, usize)> {
    if !crate::guest_init::components::responsive::enabled() {
        return None;
    }
    workload_cpu_range(guest_cpu_count())
}

/// Confine this process - and every child it later forks or execs - to `range`.
///
/// **Idempotent**: a process already inside the range is left alone. The bound is
/// applied at layered seams (the session's forked child, then whatever that child
/// execs through), so applying it twice must not reserve cores twice - and a live
/// arm found exactly that when a re-derivation read its own confined count
/// ([note 14](../../../../../docs/wayfinder/guest-responsiveness/notes/14-nix-build-bound.md)).
///
/// Allocates nothing, so it is legal in a `pre_exec` hook. `None` is the no-op
/// case: `CANG_GUEST_RESPONSIVE=0`, or a guest with too few vCPUs to reserve one.
pub(in crate::guest_init) fn apply_workload_range(range: Option<(usize, usize)>) -> io::Result<()> {
    let Some((start, end)) = range else {
        return Ok(());
    };
    if already_within(start, end)? {
        return Ok(());
    }
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for cpu in start..end {
        // SAFETY: `cpu` is inside the mask's capacity by construction (bounded by
        // the vCPU count the kernel reported) and `mask` is a valid local.
        unsafe { libc::CPU_SET(cpu, &mut mask) };
    }
    // SAFETY: pid 0 is the calling process and the mask is a live, correctly sized
    // `cpu_set_t`.
    let rc = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mask) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule bob set: two cores from three upwards, one at two, none at one.
    #[test]
    fn the_reserve_follows_the_guest_size() {
        for (cpus, reserved) in [(1, 0), (2, 1), (3, 2), (26, 2), (64, 2)] {
            assert_eq!(reserved_cores(cpus), reserved, "{cpus} vCPUs");
        }
    }

    #[test]
    fn a_coreless_or_single_core_guest_gets_no_bound() {
        assert_eq!(workload_cpu_range(0), None);
        assert_eq!(workload_cpu_range(1), None);
        assert_eq!(workload_job_count(1), None);
    }

    /// The mask is the *low* cores and the reserve is the high ones, so the cores
    /// freed are the ones a guest leaves idle rather than the ones it starts on.
    #[test]
    fn the_range_and_the_job_count_agree() {
        assert_eq!(workload_cpu_range(2), Some((0, 1)));
        assert_eq!(workload_job_count(2), Some(1));
        assert_eq!(workload_cpu_range(26), Some((0, 24)));
        assert_eq!(workload_job_count(26), Some(24));
    }

    /// Applied for real to this process: the mask read back is the range the rule
    /// computed, which is what every child of the workload will inherit.
    #[test]
    fn confining_this_process_changes_its_affinity_mask() {
        let cpus = guest_cpu_count();
        let range = workload_cpu_range(cpus);
        if range.is_none() {
            // A single-CPU host (a CI runner pinned to one core) has nothing to
            // reserve; the rule is a no-op there by design, and applying it must
            // change nothing.
            assert!(apply_workload_range(None).is_ok());
            return;
        }

        let mut before: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let mut after: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let get = |set: &mut libc::cpu_set_t| unsafe {
            assert_eq!(libc::sched_getaffinity(0, size, set), 0, "read affinity");
        };
        get(&mut before);

        apply_workload_range(range).expect("the bound applies");
        get(&mut after);

        let (start, end) = workload_cpu_range(cpus).unwrap();
        for cpu in 0..libc::CPU_SETSIZE as usize {
            let in_mask = unsafe { libc::CPU_ISSET(cpu, &after) };
            let in_range = cpu >= start && cpu < end;
            assert_eq!(in_mask, in_range, "cpu {cpu} in the confined mask");
        }

        // Restore so the rest of the suite is not confined by this test.
        assert_eq!(
            unsafe { libc::sched_setaffinity(0, size, &before) },
            0,
            "restore affinity"
        );
    }

    /// The split the `pre_exec` seams rely on: the plan is resolved in the parent,
    /// and applying it in the child allocates nothing.
    #[test]
    fn the_planned_range_is_what_the_child_applies() {
        let planned = planned_workload_range();
        // Without the environment read here, the property under test is the shape:
        // a plan is either a real range or the disabled/no-op answer.
        if let Some((start, end)) = planned {
            assert!(start < end, "a planned range is never empty");
            assert_eq!(start, 0, "the workload keeps the low cores");
            assert_eq!(workload_cpu_range(guest_cpu_count()), planned);
        }
        assert!(apply_workload_range(planned).is_ok(), "the plan applies");
        // Restore: the suite stays on every CPU it started with.
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let mut full: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for cpu in 0..guest_cpu_count() {
            // SAFETY: bounded by the reported vCPU count.
            unsafe { libc::CPU_SET(cpu, &mut full) };
        }
        // SAFETY: pid 0 is this process and the set is correctly sized.
        unsafe { assert_eq!(libc::sched_setaffinity(0, size, &full), 0) };
    }

    #[test]
    fn a_refused_syscall_is_reported_rather_than_swallowed() {
        // A mask for a CPU that cannot exist is refused by the kernel, which is
        // the failure shape a guest without the capability would hit.
        let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe { libc::CPU_SET(libc::CPU_SETSIZE as usize - 1, &mut mask) };
        let rc =
            unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mask) };
        assert_ne!(rc, 0, "an out-of-range mask must be refused");
    }

    /// The kernel's CPU-list spellings, as `/sys/devices/system/cpu/online` writes
    /// them.
    #[test]
    fn the_online_cpu_list_is_counted_not_guessed() {
        assert_eq!(parse_cpu_list("0-25"), Some(26));
        assert_eq!(parse_cpu_list("0"), Some(1));
        assert_eq!(parse_cpu_list("0-3,6-7"), Some(6));
        assert_eq!(parse_cpu_list("0,2,4"), Some(3));
        // A malformed list is `None` so the caller falls back instead of acting on
        // a made-up count.
        assert_eq!(parse_cpu_list(""), None);
        assert_eq!(parse_cpu_list("0-"), None);
        assert_eq!(parse_cpu_list("3-1"), None);
        assert_eq!(parse_cpu_list("x"), None);
    }

    /// Where the count comes from, and the property arm 14 found broken: it is the
    /// kernel's CPU *list*, never this process's affinity.
    ///
    /// A fixture rather than the real `ONLINE_CPUS_PATH`, because a `nix build`
    /// check phase runs without `/sys` mounted - a test that asserts the real path
    /// exists cannot pass in the sandbox CI uses.
    #[test]
    fn the_cpu_count_comes_from_the_kernel_list_not_this_process_affinity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let online = dir.path().join("online");
        std::fs::write(&online, "0-25\n").expect("fixture");

        assert_eq!(online_cpu_count_from(&online), Some(26));

        // Confine this thread to one CPU: the list still answers 26, which is the
        // whole point (musl's `sysconf` would have answered 1).
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let mut before: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: pid 0 is this thread and the set is correctly sized.
        unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut before), 0) };
        apply_workload_range(Some((0, 1))).expect("confine to one cpu");
        assert_eq!(online_cpu_count_from(&online), Some(26));
        // SAFETY: restore this thread's mask.
        unsafe { assert_eq!(libc::sched_setaffinity(0, size, &before), 0) };
    }

    /// Applying the bound twice reserves nothing extra: the second call is a no-op
    /// once the mask is already inside the range.
    #[test]
    fn applying_the_bound_twice_does_not_reserve_twice() {
        let cpus = guest_cpu_count();
        let Some(range) = workload_cpu_range(cpus) else {
            return;
        };
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let mut before: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: pid 0 is this process.
        unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut before), 0) };

        apply_workload_range(Some(range)).expect("first application");
        let mut first: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut first), 0) };
        apply_workload_range(Some(range)).expect("second application");
        let mut second: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut second), 0) };

        for cpu in 0..libc::CPU_SETSIZE as usize {
            // SAFETY: `CPU_ISSET` only reads the sets.
            assert_eq!(
                unsafe { libc::CPU_ISSET(cpu, &first) },
                unsafe { libc::CPU_ISSET(cpu, &second) },
                "cpu {cpu} after the second application"
            );
        }
        // And the range itself is what the rule said, not two cores narrower.
        assert_eq!(range, (0, cpus - reserved_cores(cpus)));

        // SAFETY: restore.
        unsafe { assert_eq!(libc::sched_setaffinity(0, size, &before), 0) };
    }
}
