pub(crate) mod disk;
pub(crate) mod disk_resize;

use anyhow::Result;

use crate::cli::{DiskCommand, DiskMaintenanceOptions};

pub(crate) fn run_disk_command(
    command: DiskCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    disk::run(command, options)
}
