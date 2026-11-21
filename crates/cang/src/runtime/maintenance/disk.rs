//! `cang disk`: the workspace's raw disks, which share one grow-and-resize flow.
//!
//! Two disks can be grown, and they are named differently because only one of them
//! is cang state:
//!
//! - `--containers`: the container-store disk, which also has the destructive
//!   recreate only it needs.
//! - `--workspace-disk PATH`: the image a launch named, with its new size as a
//!   positional `SIZE`. The image is the user's, so the command takes the path
//!   rather than deriving one, and it only grows an image that already exists.
//!
//! `--build` is retired (wayfinder ticket 27): `cang-build.raw` is no longer created
//! or attached, and `DiskCommand::validate` refuses it by name, pointing here.
//!
//! Each disk's grow is the same [`disk_resize::resize`] flow: extend the sparse
//! image on the host, then let one narrow maintenance VM grow the btrfs inside it.

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
        DiskCommand::Resize {
            build,
            containers,
            workspace_disk,
            size,
        } => {
            // `validate` already refused `--build`; this is the belt to that braces,
            // for a caller that constructs the command instead of parsing it.
            if build.is_some() {
                bail!(
                    "cang disk resize --build is retired: cang no longer creates or attaches a build disk - grow the workspace image with `cang disk resize --workspace-disk PATH SIZE`"
                );
            }
            let mut reports: Vec<String> = Vec::new();
            if let Some(path) = workspace_disk {
                let size = size.as_deref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "cang disk resize --workspace-disk PATH needs a size as the positional SIZE"
                    )
                })?;
                reports.push(disk_resize::resize_workspace_image(
                    &path,
                    size,
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
