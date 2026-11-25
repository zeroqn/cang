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

use anyhow::{Context, Result};

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
pub(in crate::guest_init) fn guest_cpu_count() -> usize {
    let count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    usize::try_from(count).unwrap_or(0)
}

/// Confine the calling process - and every child it later forks or execs - to the
/// cores the rule leaves it.
///
/// Best effort by design: a guest whose workload keeps every CPU still works, it
/// only competes with the build, so a failure is reported and ignored. This runs
/// in the child that is about to `exec`, so it must not allocate on the paths
/// that come from `pre_exec`; the syscall itself is async-signal-safe.
pub(in crate::guest_init) fn confine_workload() {
    if !crate::guest_init::components::responsive::enabled() {
        return;
    }
    if let Err(err) = confine_workload_at(guest_cpu_count()) {
        eprintln!("cang-guest-init: workload CPU bound not applied: {err:#}");
    }
}

fn confine_workload_at(cpus: usize) -> Result<bool> {
    let Some((start, end)) = workload_cpu_range(cpus) else {
        return Ok(false);
    };
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
        return Err(std::io::Error::last_os_error()).context(format!(
            "failed to confine the workload to CPUs {start}-{}",
            end.saturating_sub(1)
        ));
    }
    Ok(true)
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
        if workload_cpu_range(cpus).is_none() {
            // A single-CPU host (a CI runner pinned to one core) has nothing to
            // reserve; the rule is a no-op there by design.
            assert!(!confine_workload_at(cpus).unwrap());
            return;
        }

        let mut before: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let mut after: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let get = |set: &mut libc::cpu_set_t| unsafe {
            assert_eq!(libc::sched_getaffinity(0, size, set), 0, "read affinity");
        };
        get(&mut before);

        assert!(confine_workload_at(cpus).unwrap(), "the bound applies");
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
}
