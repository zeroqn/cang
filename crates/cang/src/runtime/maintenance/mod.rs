pub(crate) mod disk;
pub(crate) mod disk_resize;
pub(crate) mod namespace;
pub(crate) mod nix_gc;

use anyhow::Result;
use std::path::Path;

use crate::cli::{DiskCommand, DiskMaintenanceOptions, NixCommand};

/// The internal subcommand a maintenance VM runs, once, inside its own user
/// namespace: `<cang> internal maintenance-vm <launch.conf>`.
pub(crate) const MAINTENANCE_VM_ARG: &str = "maintenance-vm";

pub(crate) fn run_disk_command(
    command: DiskCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    disk::run(command, options)
}

pub(crate) fn run_nix_command(
    command: NixCommand,
    options: DiskMaintenanceOptions,
) -> Result<String> {
    nix_gc::run(command, options)
}

/// One maintenance VM, from a launch config the parent wrote.
///
/// Its own process because preparing the root, mounting the disk and starting
/// `libkrun` need the keep-id user namespace - the same one a task VM's worker gets,
/// with this user's subordinate ID ranges mapped - while the rootfs materialization
/// in front of it must stay outside one: a rootless Buildah snapshot sets up a
/// namespace of its own and cannot run inside cang's.
pub(crate) fn run_maintenance_vm(config_path: &Path) -> Result<()> {
    disk_resize::run_maintenance_vm(config_path)
}
