//! `cang -- <command>` launcher signalling for the guest's managed PTY.
//!
//! A command launch from a script has no terminal behind it, so the payload's
//! PTY must not behave like one: the line discipline's `OPOST` would rewrite
//! every newline into CR LF on the caller's stdout, and `ECHO` would copy a
//! piped stdin back into the output before the payload ever reads it.

pub(in crate::runtime::session) const LAUNCHER_TERMINFO_ENV: &str = "CANG_LAUNCHER_TERMINFO";

/// The guest env pair that asks guest-init to run the payload's PTY as a data
/// pipe, or `None` when the launch is an interactive session (`cang` with no
/// command, or `cang -- <command>` from a terminal, which needs a real terminal).
pub(in crate::runtime::session) fn guest_env_pair(
    guest_command: &[String],
    stdin_is_terminal: bool,
) -> Option<(String, String)> {
    (!guest_command.is_empty() && !stdin_is_terminal)
        .then(|| (LAUNCHER_TERMINFO_ENV.to_owned(), "1".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), "echo hi".to_owned()]
    }

    #[test]
    fn launcher_terminfo_is_requested_for_a_piped_command_launch() {
        assert_eq!(
            guest_env_pair(&command(), false),
            Some((LAUNCHER_TERMINFO_ENV.to_owned(), "1".to_owned()))
        );
    }

    #[test]
    fn launcher_terminfo_is_not_requested_for_an_interactive_session() {
        assert_eq!(guest_env_pair(&[], false), None);
    }

    #[test]
    fn launcher_terminfo_is_not_requested_when_a_terminal_backs_the_launch() {
        assert_eq!(guest_env_pair(&command(), true), None);
    }
}
