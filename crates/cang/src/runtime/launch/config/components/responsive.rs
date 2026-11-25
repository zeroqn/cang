//! The host's contribution to the guest's responsiveness policy.
//!
//! The guest turns its responsiveness levers on by default - the reserved CPU
//! cores and the derived build job count - so the only value that has to cross
//! into the guest environment is the one that turns them off. Reading it from the
//! host environment (rather than a CLI flag) is the same shape as
//! `CANG_MEMORY_RECLAIM`, and keeps the default launch's guest environment
//! byte-identical to what it was before the policy existed.
//!
//! The guest side is `crates/cang-guest-init/src/guest_init/components/responsive.rs`;
//! the rule both sides share: an unrecognised value changes nothing, so a typo
//! neither enables nor disables anything.

/// Opt-out for the guest's responsiveness levers.
pub(crate) const ENABLE_ENV: &str = "CANG_GUEST_RESPONSIVE";

/// The guest environment pair this launch needs, given the host's value.
///
/// `None` when the lever stays on - unset, empty, or a value nobody meant. The
/// guest defaults to on, so there is nothing to send.
pub(crate) fn guest_env_pair(host_value: Option<&str>) -> Option<(&'static str, &'static str)> {
    let value = host_value.map(str::trim)?;
    let disabled = matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    );
    disabled.then_some((ENABLE_ENV, "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only an explicit off crosses into the guest; everything else leaves the
    /// guest's default alone, including values nobody meant.
    #[test]
    fn only_an_explicit_off_reaches_the_guest() {
        assert_eq!(guest_env_pair(None), None);
        assert_eq!(guest_env_pair(Some("")), None);
        assert_eq!(guest_env_pair(Some("1")), None);
        assert_eq!(guest_env_pair(Some("true")), None);
        assert_eq!(guest_env_pair(Some("banana")), None);
        assert_eq!(guest_env_pair(Some("0")), Some((ENABLE_ENV, "0")));
        assert_eq!(guest_env_pair(Some(" OFF ")), Some((ENABLE_ENV, "0")));
        assert_eq!(guest_env_pair(Some("False")), Some((ENABLE_ENV, "0")));
    }
}
