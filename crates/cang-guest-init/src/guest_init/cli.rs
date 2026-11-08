use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "cang-guest-init",
    version,
    about = "Initialize a cang direct-libkrun microvm guest"
)]
pub(in crate::guest_init) struct GuestInitCli {
    #[command(subcommand)]
    pub(in crate::guest_init) command: GuestInitCommand,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub(in crate::guest_init) enum GuestInitCommand {
    Enter(EnterCommand),
    /// Report guest file-descriptor pressure and its likely origin.
    #[command(name = "fd-report")]
    FdReport(FdReportCommand),
    #[command(name = "as-dev", hide = true)]
    AsDev(AsDevCommand),
    #[command(hide = true)]
    Internal(InternalCommand),
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct FdReportCommand {
    /// Keep printing a fresh report every interval instead of exiting.
    #[arg(long)]
    pub(in crate::guest_init) watch: bool,
    /// Seconds between samples while watching.
    #[arg(long, value_name = "SECONDS", default_value_t = 10)]
    pub(in crate::guest_init) interval_secs: u64,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct EnterCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(in crate::guest_init) command: Vec<String>,
}

impl EnterCommand {
    pub(in crate::guest_init) fn resolved_command(&self) -> Vec<String> {
        resolved_dev_command(&self.command)
    }
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct AsDevCommand {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(in crate::guest_init) command: Vec<String>,
}

impl AsDevCommand {
    pub(in crate::guest_init) fn resolved_command(&self) -> Vec<String> {
        resolved_dev_command(&self.command)
    }
}

fn resolved_dev_command(command: &[String]) -> Vec<String> {
    let command = command.strip_prefix(&["--".to_owned()]).unwrap_or(command);
    if command.is_empty() {
        vec!["fish".to_owned(), "-l".to_owned()]
    } else {
        command.to_vec()
    }
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct InternalCommand {
    #[command(subcommand)]
    pub(in crate::guest_init) command: InternalSubcommand,
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum InternalSubcommand {
    Nix(NixCommand),
    Podman(PodmanCommand),
    Pulse(PulseCommand),
    Resize(ResizeCommand),
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct PulseCommand {
    pub(in crate::guest_init) port: u32,
    pub(in crate::guest_init) uid: u32,
    pub(in crate::guest_init) gid: u32,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct NixCommand {
    #[command(subcommand)]
    pub(in crate::guest_init) command: NixSubcommand,
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum NixSubcommand {
    Prep,
    Wait,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct PodmanCommand {
    #[command(subcommand)]
    pub(in crate::guest_init) command: PodmanSubcommand,
}

#[derive(Debug, Subcommand, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum PodmanSubcommand {
    Prep,
    Wait,
    ServiceWait,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::guest_init::cli::{
        GuestInitCli, GuestInitCommand, InternalSubcommand, NixSubcommand, PodmanSubcommand,
    };

    #[test]
    fn command_help_renders() {
        GuestInitCli::command().debug_assert();
    }

    #[test]
    fn public_help_hides_internal_worker_namespace() {
        let mut help = Vec::new();
        GuestInitCli::command()
            .write_long_help(&mut help)
            .expect("help should render");
        let help = String::from_utf8(help).expect("help should be utf8");

        assert!(help.contains("enter"));
        assert!(help.contains("fd-report"));
        assert!(!help.contains("internal"));
    }

    #[test]
    fn parses_public_fd_report_command() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "fd-report"])
            .expect("fd-report should parse");
        let GuestInitCommand::FdReport(command) = cli.command else {
            panic!("expected fd-report command");
        };
        assert!(!command.watch);
        assert_eq!(command.interval_secs, 10);

        let cli = GuestInitCli::try_parse_from([
            "cang-guest-init",
            "fd-report",
            "--watch",
            "--interval-secs",
            "3",
        ])
        .expect("fd-report watch should parse");
        let GuestInitCommand::FdReport(command) = cli.command else {
            panic!("expected fd-report command");
        };
        assert!(command.watch);
        assert_eq!(command.interval_secs, 3);
    }

    #[test]
    fn parses_enter_command() {
        let cli =
            GuestInitCli::try_parse_from(["cang-guest-init", "enter", "bash", "-lc", "echo ok"])
                .expect("enter command should parse");

        let GuestInitCommand::Enter(command) = cli.command else {
            panic!("expected enter command");
        };
        assert_eq!(command.resolved_command(), ["bash", "-lc", "echo ok"]);
    }

    #[test]
    fn enter_defaults_to_login_fish() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "enter"])
            .expect("enter command should parse");

        let GuestInitCommand::Enter(command) = cli.command else {
            panic!("expected enter command");
        };
        assert_eq!(command.resolved_command(), ["fish", "-l"]);
    }

    #[test]
    fn enter_allows_explicit_delimiter_before_command() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "enter", "--", "fish", "-l"])
            .expect("enter command should parse");

        let GuestInitCommand::Enter(command) = cli.command else {
            panic!("expected enter command");
        };
        assert_eq!(command.resolved_command(), ["fish", "-l"]);
    }

    #[test]
    fn as_dev_defaults_to_login_fish() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "as-dev"])
            .expect("as-dev command should parse");

        let GuestInitCommand::AsDev(command) = cli.command else {
            panic!("expected as-dev command");
        };
        assert_eq!(command.resolved_command(), ["fish", "-l"]);
    }

    #[test]
    fn as_dev_accepts_explicit_delimiter_before_command() {
        let cli = GuestInitCli::try_parse_from([
            "cang-guest-init",
            "as-dev",
            "--",
            "bash",
            "-lc",
            "id -un",
        ])
        .expect("as-dev command should parse");

        let GuestInitCommand::AsDev(command) = cli.command else {
            panic!("expected as-dev command");
        };
        assert_eq!(command.resolved_command(), ["bash", "-lc", "id -un"]);
    }

    #[test]
    fn parses_hidden_internal_worker_commands() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "internal", "nix", "prep"])
            .expect("internal nix prep should parse");
        let GuestInitCommand::Internal(internal) = cli.command else {
            panic!("expected internal command");
        };
        let InternalSubcommand::Nix(nix) = internal.command else {
            panic!("expected nix command");
        };
        assert_eq!(nix.command, NixSubcommand::Prep);

        let cli =
            GuestInitCli::try_parse_from(["cang-guest-init", "internal", "podman", "service-wait"])
                .expect("internal podman service-wait should parse");
        let GuestInitCommand::Internal(internal) = cli.command else {
            panic!("expected internal command");
        };
        let InternalSubcommand::Podman(podman) = internal.command else {
            panic!("expected podman command");
        };
        assert_eq!(podman.command, PodmanSubcommand::ServiceWait);
    }

    #[test]
    fn parses_hidden_internal_resize_command() {
        let cli = GuestInitCli::try_parse_from(["cang-guest-init", "internal", "resize", "nix"])
            .expect("internal resize should parse");
        let GuestInitCommand::Internal(internal) = cli.command else {
            panic!("expected internal command");
        };
        let InternalSubcommand::Resize(resize) = internal.command else {
            panic!("expected resize command");
        };
        assert_eq!(resize.target, crate::guest_init::cli::ResizeTarget::Nix);
    }
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct ResizeCommand {
    #[arg(value_enum)]
    pub(in crate::guest_init) target: ResizeTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(in crate::guest_init) enum ResizeTarget {
    Nix,
    Containers,
    Build,
}
