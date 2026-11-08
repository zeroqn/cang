//! `cang container-store`: the workspace's raw container-store disk.
//!
//! `resize` is the shared grow-and-resize flow in [`super::disk_resize`]; `reset`
//! is the destructive recreate, which only this disk has.

use anyhow::{Result, bail};

use crate::cli::{ContainerStoreCommand, DiskMaintenanceOptions};
use crate::runtime::launch::components::persistent_disks::{
    container_store_default_size_bytes, recreate_container_store,
};
use crate::runtime::session::task_control::{
    ProcfsInspector, ensure_workspace_has_no_running_tasks,
};

use super::disk_resize::{self, DiskKind, HostMaintenanceVmRunner};

pub(crate) fn run(
    command: ContainerStoreCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    match command {
        ContainerStoreCommand::Resize { size } => disk_resize::resize(
            DiskKind::ContainerStore,
            &size,
            &options,
            &HostMaintenanceVmRunner,
        ),
        ContainerStoreCommand::Reset { force } => reset(force),
    }
}

fn reset(force: bool) -> Result<String> {
    if !force {
        bail!(
            "cang container-store reset is destructive; rerun with --force to delete and recreate the raw container-store disk"
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
