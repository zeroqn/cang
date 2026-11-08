pub(crate) mod build_disk;
pub(crate) mod container_store;
pub(crate) mod disk_resize;

use anyhow::Result;

use crate::cli::{BuildDiskCommand, ContainerStoreCommand, DiskMaintenanceOptions};

pub(crate) fn run_container_store_command(
    command: ContainerStoreCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    container_store::run(command, options)
}

pub(crate) fn run_build_disk_command(
    command: BuildDiskCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    build_disk::run(command, options)
}
