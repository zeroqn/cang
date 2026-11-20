//! `cang nix gc`: reclaim the `/nix` upper generations a workspace no longer needs.
//!
//! The writable upper lives inside a btrfs image the host cannot mount rootless, so
//! collecting it needs a guest: one narrow maintenance VM with that disk attached,
//! running `cang-guest-init internal nix gc`. Which disk follows the launch surface -
//! the build disk for a host-directory launch, or the image named by
//! `--workspace-disk`, whose tree holds the upper at `/workspace/.cang/nix/` - and
//! the selection rule is the guest's: keep the generation this workspace used last,
//! collect the rest, with `--all` collecting that one too.

use anyhow::{Context, Result};

use crate::cli::{DiskMaintenanceOptions, NixCommand, NixGcOptions};
use crate::runtime::launch::components::persistent_disks::{
    build_disk_attachment, build_disk_env_pairs, existing_build_disk,
    raw_btrfs::HostRawImageCommandRunner,
    workspace::{self, WorkspaceDisk},
};

use super::disk_resize::{self, HostMaintenanceVmRunner, MaintenanceVmRunner};

const TASK_PREFIX: &str = "nix-upper-gc";
const HOSTNAME: &str = "cang-nix-upper-gc";

pub(crate) fn run(command: NixCommand, options: DiskMaintenanceOptions) -> Result<String> {
    match command {
        NixCommand::Gc {
            all,
            dry_run,
            workspace_disk,
        } => collect(
            NixGcOptions {
                all,
                dry_run,
                workspace_disk,
            },
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
    let target = collect_target(&options, &layout)?;

    disk_resize::run_maintenance_guest(
        TASK_PREFIX,
        HOSTNAME,
        argv(&options, &target),
        vec![target.attachment()],
        target.env_pairs().to_vec(),
        &layout,
        launch,
        runner,
    )
    .with_context(|| {
        format!(
            "cang failed to collect the /nix upper on '{}'; rerun `{}`",
            target.path.display(),
            rerun_command(&options)
        )
    })?;

    Ok(render_report(&options, &target.path, &task_report))
}

/// Which disk this run collects, and the guest-side layout that says where its
/// generations are.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CollectTarget {
    path: std::path::PathBuf,
    attachment: crate::runtime::launch::config::DiskAttachment,
    env_pairs: Vec<(String, String)>,
    /// The maintenance VM's mount point for the disk.
    guest_mount: String,
    /// Where the generations live inside it.
    guest_generations_dir: String,
    /// The btrfs label and disk id the guest looks the disk up by.
    disk_label: String,
    disk_id: String,
}

impl CollectTarget {
    fn attachment(&self) -> crate::runtime::launch::config::DiskAttachment {
        self.attachment.clone()
    }

    fn env_pairs(&self) -> &[(String, String)] {
        &self.env_pairs
    }
}

/// The build disk, or the image `--workspace-disk` names.
///
/// A workspace image is *validated*, never created or grown: `gc` on a path with no
/// image is a typo, and mkfs-ing it would be the worst possible response.
fn collect_target(
    options: &NixGcOptions,
    layout: &crate::state::StateLayout,
) -> Result<CollectTarget> {
    if let Some(path) = options.workspace_disk.as_deref() {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .context("failed to resolve the current directory for --workspace-disk")?
                .join(path)
        };
        let disk: WorkspaceDisk =
            workspace::validate_existing_with_runner(&path, &HostRawImageCommandRunner)?;
        return Ok(CollectTarget {
            path: disk.path.clone(),
            attachment: workspace::attachment(&disk),
            env_pairs: workspace::env_pairs(&disk).to_vec(),
            guest_mount: "/workspace".to_owned(),
            guest_generations_dir: "/workspace/.cang/nix".to_owned(),
            disk_label: workspace::LABEL.to_owned(),
            disk_id: workspace::ID.to_owned(),
        });
    }

    let Some(disk) = existing_build_disk(layout.root_dir())? else {
        anyhow::bail!(
            "this workspace has no build disk, so it has no /nix upper to collect - launch cang here once first, or pass --workspace-disk for an image whose tree holds it"
        );
    };
    Ok(CollectTarget {
        path: disk.path.clone(),
        attachment: build_disk_attachment(&disk),
        env_pairs: build_disk_env_pairs(&disk).to_vec(),
        guest_mount: "/home/dev/.cache/cang-build".to_owned(),
        guest_generations_dir: "/home/dev/.cache/cang-build/nix-overlay".to_owned(),
        disk_label: disk.label.clone(),
        disk_id: disk.id.clone(),
    })
}

/// The guest command a `cang nix gc` run executes.
fn argv(options: &NixGcOptions, target: &CollectTarget) -> Vec<String> {
    let mut argv = vec![
        "internal".to_owned(),
        "nix".to_owned(),
        "gc".to_owned(),
        "--mount".to_owned(),
        target.guest_mount.clone(),
        "--generations-dir".to_owned(),
        target.guest_generations_dir.clone(),
        "--disk-label".to_owned(),
        target.disk_label.clone(),
        "--disk-id".to_owned(),
        target.disk_id.clone(),
    ];
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
    if let Some(path) = options.workspace_disk.as_deref() {
        command.push_str(&format!(" --workspace-disk {}", path.display()));
    }
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

    fn options(all: bool, dry_run: bool) -> NixGcOptions {
        NixGcOptions {
            all,
            dry_run,
            workspace_disk: None,
        }
    }

    fn target(mount: &str, generations: &str) -> CollectTarget {
        CollectTarget {
            path: std::path::PathBuf::from("/state/workspace/cang-build.raw"),
            attachment: crate::runtime::launch::config::DiskAttachment {
                id: "cang-build-disk".to_owned(),
                path: std::path::PathBuf::from("/state/workspace/cang-build.raw"),
                read_only: false,
            },
            env_pairs: Vec::new(),
            guest_mount: mount.to_owned(),
            guest_generations_dir: generations.to_owned(),
            disk_label: "cang-build".to_owned(),
            disk_id: "cang-build-disk".to_owned(),
        }
    }

    /// The guest is told the layout every run, so it never has to guess which disk
    /// holds the generations: the build disk for a host-directory launch, the
    /// workspace tree for a `--workspace-disk` one.
    #[test]
    fn the_guest_argv_names_the_layout_and_the_flags_that_were_asked_for() {
        let build = target(
            "/home/dev/.cache/cang-build",
            "/home/dev/.cache/cang-build/nix-overlay",
        );
        assert_eq!(
            argv(&options(false, false), &build),
            [
                "internal",
                "nix",
                "gc",
                "--mount",
                "/home/dev/.cache/cang-build",
                "--generations-dir",
                "/home/dev/.cache/cang-build/nix-overlay",
                "--disk-label",
                "cang-build",
                "--disk-id",
                "cang-build-disk",
            ]
        );
        assert_eq!(
            argv(&options(true, true), &build)
                .last()
                .map(String::as_str),
            Some("--dry-run")
        );
        let workspace = CollectTarget {
            disk_label: "CANG_WORKSPACE".to_owned(),
            disk_id: "cang-workspace".to_owned(),
            ..target("/workspace", "/workspace/.cang/nix")
        };
        assert_eq!(
            argv(&options(true, false), &workspace),
            [
                "internal",
                "nix",
                "gc",
                "--mount",
                "/workspace",
                "--generations-dir",
                "/workspace/.cang/nix",
                "--disk-label",
                "CANG_WORKSPACE",
                "--disk-id",
                "cang-workspace",
                "--all",
            ]
        );
    }

    /// The rerun line a failed run prints is a command the user can paste back, and
    /// it carries the disk the run was about.
    #[test]
    fn the_rerun_command_keeps_the_flags_and_the_disk() {
        assert_eq!(rerun_command(&options(false, false)), "cang nix gc");
        assert_eq!(
            rerun_command(&options(true, true)),
            "cang nix gc --all --dry-run"
        );
        let with_disk = NixGcOptions {
            workspace_disk: Some(std::path::PathBuf::from("/data/work.raw")),
            ..options(true, false)
        };
        assert_eq!(
            rerun_command(&with_disk),
            "cang nix gc --workspace-disk /data/work.raw --all"
        );
    }

    /// A `--workspace-disk` image is validated, never created: gc on a path with no
    /// image is a typo, and formatting it would be the worst possible answer.
    #[test]
    fn a_missing_workspace_image_is_refused_without_creating_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("work.raw");
        let options = NixGcOptions {
            workspace_disk: Some(path.clone()),
            ..options(false, true)
        };

        let layout = disk_resize::current_state_layout().expect("state layout");
        let err = collect_target(&options, &layout).expect_err("a missing image must be refused");

        assert!(
            format!("{err:#}").contains("cang nix gc does not create one"),
            "{err:#}"
        );
        assert!(!path.exists(), "gc must not create the image");
    }

    #[test]
    fn the_report_names_what_was_done_and_where() {
        let report = render_report(
            &NixGcOptions {
                all: false,
                dry_run: true,
                workspace_disk: None,
            },
            std::path::Path::new("/state/workspace/cang-build.raw"),
            &crate::runtime::session::task_control::WorkspaceTaskGateReport {
                stale_task_ids: Vec::new(),
            },
        );
        assert_eq!(
            report,
            "nix-upper-would-collect	/state/workspace/cang-build.raw\n"
        );
    }
}
