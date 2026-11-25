//! The host's contribution to the guest's sched_ext scheduler.
//!
//! The guest starts `scx_bpfland` by default, so the only value that has to cross
//! into the guest environment is the one that turns it off: `--scx=off`. Reading
//! nothing on a default launch keeps that launch's guest environment identical to
//! what it was before the scheduler existed.
//!
//! The guest side is `crates/cang-guest-init/src/guest_init/components/scx.rs`;
//! the rule both sides share is that the guest decides, and the host only says
//! `off`.

pub(in crate::runtime::session) const SCX_ENV: &str = "CANG_GUEST_SCX";

/// The guest environment pair this launch needs, given `--scx`.
///
/// `None` when the scheduler stays on: the guest defaults to on, so there is
/// nothing to send.
pub(in crate::runtime::session) fn guest_env_pair(enabled: bool) -> Option<(String, String)> {
    (!enabled).then(|| (SCX_ENV.to_owned(), "0".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_off_value_reaches_the_guest() {
        assert_eq!(guest_env_pair(true), None);
        assert_eq!(
            guest_env_pair(false),
            Some((SCX_ENV.to_owned(), "0".to_owned()))
        );
    }
}
