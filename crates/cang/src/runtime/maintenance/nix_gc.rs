//! `cang nix gc`: reclaim the workspace's `/nix` upper on the build disk.
//!
//! The writable upper lives inside the build disk's btrfs image, which the host
//! cannot mount rootless, so collecting it needs a guest: one narrow maintenance VM
//! with that disk attached, running `cang-guest-init internal nix gc`. The selection
//! rule is the guest's - keep the generation this workspace used last, collect the
//! rest - and `--all` collects that one too.

use anyhow::{Context, Result};

use crate::cli::{DiskMaintenanceOptions, NixCommand, NixGcOptions};
use crate::runtime::launch::components::persistent_disks::{
    build_disk_attachment, build_disk_env_pairs, existing_build_disk,
};

use super::disk_resize::{self, HostMaintenanceVmRunner, MaintenanceVmRunner};

const TASK_PREFIX: &str = "nix-upper-gc";
const HOSTNAME: &str = "cang-nix-upper-gc";

pub(crate) fn run(command: NixCommand, options: DiskMaintenanceOptions) -> Result<String> {
    match command {
        NixCommand::Gc { all, dry_run } => collect(
            NixGcOptions { all, dry_run },
            &options,
            &HostMaintenanceVmRunner,
        ),
    }
}

fn collect(
    options: NixGcOptions,
    launch: &DiskMaintenanceOptions,
    runner: &impl MaintenanceVmRunner,
) -> Result<String> {
    let layout = disk_resize::current_state_layout()?;
    let task_report = crate::runtime::session::task_control::ensure_workspace_has_no_running_tasks(
        layout.root_dir(),
        &crate::runtime::session::task_control::ProcfsInspector,
    )?;
    let Some(disk) = existing_build_disk(layout.root_dir())? else {
        anyhow::bail!(
            "this workspace has no build disk, so it has no /nix upper to collect - launch cang here once first"
        );
    };

    disk_resize::run_maintenance_guest(
        TASK_PREFIX,
        HOSTNAME,
        argv(&options),
        vec![build_disk_attachment(&disk)],
        build_disk_env_pairs(&disk).to_vec(),
        &layout,
        launch,
        runner,
    )
    .with_context(|| {
        format!(
            "cang failed to collect the /nix upper on '{}'; rerun `{}`",
            disk.path.display(),
            rerun_command(&options)
        )
    })?;

    Ok(render_report(&options, &disk.path, &task_report))
}

/// The guest command a `cang nix gc` run executes.
fn argv(options: &NixGcOptions) -> Vec<String> {
    let mut argv = vec!["internal".to_owned(), "nix".to_owned(), "gc".to_owned()];
    if options.all {
        argv.push("--all".to_owned());
    }
    if options.dry_run {
        argv.push("--dry-run".to_owned());
    }
    argv
}

fn rerun_command(options: &NixGcOptions) -> String {
    let mut command = "cang nix gc".to_owned();
    if options.all {
        command.push_str(" --all");
    }
    if options.dry_run {
        command.push_str(" --dry-run");
    }
    command
}

fn render_report(
    options: &NixGcOptions,
    path: &std::path::Path,
    task_report: &crate::runtime::session::task_control::WorkspaceTaskGateReport,
) -> String {
    let mut output = String::new();
    if !task_report.stale_task_ids.is_empty() {
        output.push_str(&format!(
            "stale-tasks-ignored\t{}\n",
            task_report.stale_task_ids.join(",")
        ));
    }
    let action = if options.dry_run {
        "nix-upper-would-collect"
    } else if options.all {
        "nix-upper-collected-including-cache"
    } else {
        "nix-upper-collected"
    };
    output.push_str(&format!("{action}\t{}\n", path.display()));
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guest_argv_names_the_flags_that_were_asked_for() {
        let base = NixGcOptions {
            all: false,
            dry_run: false,
        };
        assert_eq!(argv(&base), ["internal", "nix", "gc"]);
        assert_eq!(
            argv(&NixGcOptions { all: true, ..base }),
            ["internal", "nix", "gc", "--all"]
        );
        assert_eq!(
            argv(&NixGcOptions {
                dry_run: true,
                ..base
            }),
            ["internal", "nix", "gc", "--dry-run"]
        );
        assert_eq!(
            argv(&NixGcOptions {
                all: true,
                dry_run: true
            }),
            ["internal", "nix", "gc", "--all", "--dry-run"]
        );
    }

    /// The rerun line a failed run prints is a command the user can paste back.
    #[test]
    fn the_rerun_command_keeps_the_flags() {
        assert_eq!(
            rerun_command(&NixGcOptions {
                all: false,
                dry_run: false
            }),
            "cang nix gc"
        );
        assert_eq!(
            rerun_command(&NixGcOptions {
                all: true,
                dry_run: true
            }),
            "cang nix gc --all --dry-run"
        );
    }

    #[test]
    fn the_report_names_what_was_done_and_where() {
        let report = render_report(
            &NixGcOptions {
                all: false,
                dry_run: true,
            },
            std::path::Path::new("/state/workspace/cang-build.raw"),
            &crate::runtime::session::task_control::WorkspaceTaskGateReport {
                stale_task_ids: Vec::new(),
            },
        );
        assert_eq!(
            report,
            "nix-upper-would-collect\t/state/workspace/cang-build.raw\n"
        );
    }
}
