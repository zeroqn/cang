//! `cang build-disk`: the workspace's raw build disk (`cang-build.raw`), which
//! carries cargo's `target/` and `CARGO_HOME` across tasks.
//!
//! Only `resize` exists: the image is created by the first launch in a workspace,
//! so growing it is the one maintenance operation it needs.

use anyhow::Result;

use crate::cli::{BuildDiskCommand, DiskMaintenanceOptions};

use super::disk_resize::{self, DiskKind, HostMaintenanceVmRunner};

pub(crate) fn run(command: BuildDiskCommand, options: DiskMaintenanceOptions) -> Result<String> {
    match command {
        BuildDiskCommand::Resize { size } => disk_resize::resize(
            DiskKind::BuildDisk,
            &size,
            &options,
            &HostMaintenanceVmRunner,
        ),
    }
}
