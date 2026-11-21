use anyhow::Result;
use std::time::Duration;

use crate::guest_init::cli::{
    GuestInitCommand, InternalSubcommand, NixSubcommand, PodmanSubcommand,
};

pub(in crate::guest_init) mod as_dev;
mod attach_profile;
pub(in crate::guest_init) mod cang;
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
        GuestInitCommand::Internal(command) => match command.command {
            InternalSubcommand::Nix(nix) => match nix.command {
                NixSubcommand::Prep => {
                    crate::guest_init::components::nix::root::run_prep_to_status()
                }
                NixSubcommand::Wait => crate::guest_init::components::nix::user::wait_for_prep(),
                NixSubcommand::Gc(gc) => {
                    let layout = match (gc.mount, gc.generations_dir, gc.disk_label, gc.disk_id) {
                        (None, None, None, None) => None,
                        (Some(mount), Some(generations_dir), Some(disk_label), Some(disk_id)) => {
                            Some(crate::guest_init::components::nix::gc::GcLayout {
                                mount,
                                generations_dir,
                                disk_label,
                                disk_id,
                            })
                        }
                        _ => {
                            return Err(anyhow::anyhow!(
                                "--mount, --generations-dir, --disk-label and --disk-id name the gc layout together; pass all four or none (none means the build disk)"
                            ));
                        }
                    };
                    crate::guest_init::components::nix::gc::run(gc.all, gc.dry_run, layout.as_ref())
                }
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
        },
    }
}
