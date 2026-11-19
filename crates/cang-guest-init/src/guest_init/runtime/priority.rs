//! Scheduling priority for the threads that carry interactive work.
//!
//! A busy guest runs dozens of compiler threads, and the two interactive paths - the
//! session PTY and the exec listener - are cheap but latency-critical: when every vCPU is
//! oversubscribed they wait their turn behind the build. Raising their nice level costs
//! the build nothing measurable and keeps typing and command start-up responsive
//! (measured: PTY p99 24 ms -> 7.5 ms, max 1 353 ms -> 59 ms on a guest running 13
//! concurrent `rustc`s; see docs/wayfinder/nix-upper-on-block/notes/36).

/// Nice level for the threads that carry a keystroke or an exec handshake.
pub(in crate::guest_init::runtime) const INTERACTIVE_NICE: libc::c_int = -20;

/// Raise the current thread's priority. Best effort: a guest that cannot raise its own
/// priority still works, only more slowly under load.
pub(in crate::guest_init::runtime) fn prioritise_current_thread(
    thread_name: &str,
    nice: libc::c_int,
) {
    let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        debug(&format!(
            "could not raise priority of {thread_name}: {err}; it will compete for CPU normally"
        ));
    } else {
        debug(&format!("{thread_name} prioritised to nice {nice}"));
    }
}

fn debug(message: &str) {
    if std::env::var("CANG_GUEST_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("cang-guest-init: debug: {message}");
    }
}
