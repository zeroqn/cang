//! `cang disk`: the workspace's raw disks, which share one grow-and-resize flow.
//!
//! Two disks exist, and a third consumer arrived when the writable `/nix` upper
//! moved into the build disk - so one command names them by flag instead of one
//! command tree per disk:
//!
//! - `--build`: `cang-build.raw`, carrying cargo's `target/`, `CARGO_HOME` and
//!   now the `/nix` overlay's upper; grow-only, because the image is created by
//!   the first launch in a workspace.
//! - `--containers`: the container-store disk, which also has the destructive
//!   recreate only it needs.
//!
//! A single invocation may name both. Each disk's grow is the same
//! [`disk_resize::resize`] flow: extend the sparse image on the host, then let
//! one narrow maintenance VM grow the btrfs inside it.

use anyhow::{Result, bail};

use crate::cli::{DiskCommand, DiskMaintenanceOptions};
use crate::runtime::launch::components::persistent_disks::{
    container_store_default_size_bytes, recreate_container_store,
};
use crate::runtime::session::task_control::{
    ProcfsInspector, ensure_workspace_has_no_running_tasks,
};

use super::disk_resize::{self, DiskKind, HostMaintenanceVmRunner};

pub(crate) fn run(command: DiskCommand, options: DiskMaintenanceOptions) -> Result<String> {
    match command {
        DiskCommand::Resize { build, containers } => {
            if build.is_none() && containers.is_none() {
                bail!("cang disk resize needs --build SIZE, --containers SIZE, or both");
            }
            let mut reports: Vec<String> = Vec::new();
            if let Some(size) = build {
                reports.push(disk_resize::resize(
                    DiskKind::BuildDisk,
                    &size,
                    &options,
                    &HostMaintenanceVmRunner,
                )?);
            }
            if let Some(size) = containers {
                reports.push(disk_resize::resize(
                    DiskKind::ContainerStore,
                    &size,
                    &options,
                    &HostMaintenanceVmRunner,
                )?);
            }
            Ok(reports.concat())
        }
        DiskCommand::Reset { containers, force } => {
            if !containers {
                bail!(
                    "cang disk reset needs --containers: it is the only disk with a recreate path"
                );
            }
            reset_container_store(force)
        }
    }
}

fn reset_container_store(force: bool) -> Result<String> {
    if !force {
        bail!(
            "cang disk reset --containers is destructive; rerun with --force to delete and recreate the raw container-store disk"
        );
    }
    let layout = disk_resize::current_state_layout()?;
    let task_report = ensure_workspace_has_no_running_tasks(layout.root_dir(), &ProcfsInspector)?;
    let disk = recreate_container_store(layout.root_dir())?;

    Ok(disk_resize::render_report(
        "reset-container-store",
        &disk.path,
        container_store_default_size_bytes(),
        &task_report,
    ))
}
