use anyhow::Result;
use std::time::Duration;

use crate::guest_init::cli::{
    GuestInitCommand, InternalSubcommand, NixSubcommand, PodmanSubcommand,
};

/// The `internal` workers that must outlive guest memory pressure, named for the
/// OOM guard's diagnostics; `None` for every other command in that namespace.
///
/// The four named here each hold state the session keeps depending on after they
/// finish - the zram device, the `/nix` overlay seed, the Podman store, the Pulse
/// socket - so a kill part-way through leaves that state half-built for the life
/// of the guest. The rest are one-shot maintenance commands whose own death is
/// the answer the host reads (`internal resize`, `internal nix gc`,
/// `internal workspace free-bytes`), or a client-side wait that must stay
/// killable like any other foreground process.
fn protected_worker_label(command: &InternalSubcommand) -> Option<&'static str> {
    match command {
        InternalSubcommand::Swap(_) => Some("guest swap worker"),
        InternalSubcommand::Nix(nix) if matches!(nix.command, NixSubcommand::Prep) => {
            Some("guest /nix overlay prep worker")
        }
        InternalSubcommand::Podman(podman) if matches!(podman.command, PodmanSubcommand::Prep) => {
            Some("guest Podman prep worker")
        }
        InternalSubcommand::Pulse(_) => Some("guest Pulse bridge"),
        InternalSubcommand::Nix(_)
        | InternalSubcommand::Podman(_)
        | InternalSubcommand::Resize(_)
        | InternalSubcommand::Workspace(_) => None,
    }
}

pub(in crate::guest_init) mod as_dev;
mod attach_profile;
pub(in crate::guest_init) mod cang;
pub(in crate::guest_init) mod cpu_bound;
mod exec;
mod exit_status;
mod priority;
mod session;
pub(in crate::guest_init) mod vsock;

pub(in crate::guest_init) fn run(command: GuestInitCommand) -> Result<()> {
    match command {
        GuestInitCommand::Enter(command) => cang::enter(command.resolved_command()),
        GuestInitCommand::FdReport(command) => {
            let interval = Duration::from_secs(command.interval_secs.max(1));
            crate::guest_init::components::fdwatch::run_report(command.watch, interval)
        }
        GuestInitCommand::AsDev(command) => as_dev::run(command.resolved_command()),
        GuestInitCommand::Internal(command) => {
            // A worker is protected before it does anything else, so the score
            // holds across whatever it waits for and no spawner has to write a
            // child's `/proc` entry between `fork` and its first instruction.
            if let Some(worker) = protected_worker_label(&command.command) {
                crate::guest_init::process::protect_from_oom(worker);
            }
            match command.command {
                InternalSubcommand::Nix(nix) => match nix.command {
                    NixSubcommand::Prep => {
                        crate::guest_init::components::nix::root::run_prep_to_status()
                    }
                    NixSubcommand::Wait => {
                        crate::guest_init::components::nix::user::wait_for_prep()
                    }
                    NixSubcommand::Gc(gc) => crate::guest_init::components::nix::gc::run(
                        gc.all,
                        gc.dry_run,
                        &crate::guest_init::components::nix::gc::GcLayout {
                            mount: gc.mount,
                            generations_dir: gc.generations_dir,
                            disk_label: gc.disk_label,
                            disk_id: gc.disk_id,
                        },
                    ),
                },
                InternalSubcommand::Podman(podman) => match podman.command {
                    PodmanSubcommand::Prep => {
                        crate::guest_init::components::podman::root::run_prep_to_status()
                    }
                    PodmanSubcommand::Wait => {
                        crate::guest_init::components::podman::user::wait_for_prep()
                    }
                    PodmanSubcommand::ServiceWait => {
                        crate::guest_init::components::podman::user::wait_for_service()
                    }
                },
                InternalSubcommand::Swap(swap) => match swap.command {
                    crate::guest_init::cli::SwapSubcommand::Ensure { after_exec } => {
                        if after_exec {
                            crate::guest_init::components::swap::zram::ensure_after_exec();
                        } else {
                            crate::guest_init::components::swap::zram::ensure();
                        }
                        Ok(())
                    }
                },
                InternalSubcommand::Pulse(pulse) => {
                    crate::guest_init::components::pulse::run(pulse.port, pulse.uid, pulse.gid)
                }
                InternalSubcommand::Workspace(workspace) => match workspace.command {
                    crate::guest_init::cli::WorkspaceSubcommand::FreeBytes => {
                        crate::guest_init::components::disk::workspace::report_free_bytes()
                    }
                },
                InternalSubcommand::Resize(resize) => {
                    crate::guest_init::components::disk::resize::run(resize.target)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::guest_init::cli::GuestInitCli;

    /// The label the guard would use for a command line, which is also the
    /// spelling `cang enter` passes when it re-executes this binary.
    fn label_of(args: &[&str]) -> Option<&'static str> {
        let cli = GuestInitCli::try_parse_from(args).expect("worker command line should parse");
        let GuestInitCommand::Internal(internal) = cli.command else {
            panic!("expected an internal command");
        };
        protected_worker_label(&internal.command)
    }

    /// The four workers whose death leaves the session's state half-built.
    #[test]
    fn the_workers_a_session_depends_on_are_protected() {
        assert_eq!(
            label_of(&[
                "cang-guest-init",
                "internal",
                "swap",
                "ensure",
                "--after-exec"
            ]),
            Some("guest swap worker")
        );
        assert_eq!(
            label_of(&["cang-guest-init", "internal", "nix", "prep"]),
            Some("guest /nix overlay prep worker")
        );
        assert_eq!(
            label_of(&["cang-guest-init", "internal", "podman", "prep"]),
            Some("guest Podman prep worker")
        );
        assert_eq!(
            label_of(&[
                "cang-guest-init",
                "internal",
                "pulse",
                "8080",
                "1000",
                "1000"
            ]),
            Some("guest Pulse bridge")
        );
    }

    /// Everything else in the namespace keeps score 0: a maintenance VM's own
    /// death is the answer the host reads, and a client-side wait is as killable
    /// as any foreground process.
    #[test]
    fn maintenance_and_client_commands_stay_killable() {
        let cases: &[&[&str]] = &[
            &["cang-guest-init", "internal", "nix", "wait"],
            &[
                "cang-guest-init",
                "internal",
                "nix",
                "gc",
                "--mount",
                "/run/cang/gc",
                "--generations-dir",
                "nix-overlay",
                "--disk-label",
                "CANG_WORKSPACE",
                "--disk-id",
                "cang-workspace",
            ],
            &["cang-guest-init", "internal", "podman", "wait"],
            &["cang-guest-init", "internal", "podman", "service-wait"],
            &["cang-guest-init", "internal", "resize", "nix"],
            &["cang-guest-init", "internal", "workspace", "free-bytes"],
        ];
        for args in cases {
            assert_eq!(label_of(args), None, "{args:?} must stay killable");
        }
    }
}
