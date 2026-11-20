//! Bind mount contribution validation and workspace compatibility ownership.

use anyhow::{Result, anyhow};
use std::collections::BTreeSet;

use super::super::model::{
    BindMount, HostNixOverlay, NIX_TAG, NIX_TARGET, WORKSPACE_TARGET, canonical_mount_target,
};

pub(crate) fn mounts_with_host_nix_overlay(
    mounts: &[BindMount],
    host_nix_overlay: Option<&HostNixOverlay>,
) -> Result<Vec<BindMount>> {
    validate_mounts(mounts)?;
    let Some(host_nix_overlay) = host_nix_overlay else {
        return Ok(mounts.to_vec());
    };
    if mounts.iter().any(|mount| mount.target == NIX_TARGET) {
        anyhow::bail!(
            "cang host /nix overlay owns {NIX_TARGET}; remove the duplicate user bind mount"
        );
    }
    let mut with_nix = mounts.to_vec();
    with_nix.push(BindMount::directory(
        host_nix_overlay.mergeddir.clone(),
        NIX_TAG,
        NIX_TARGET,
    ));
    validate_mounts(&with_nix)?;
    Ok(with_nix)
}

pub(crate) fn workspace_mount(mounts: &[BindMount]) -> Result<&BindMount> {
    mounts
        .iter()
        .find(|mount| mount.target == WORKSPACE_TARGET)
        .ok_or_else(|| anyhow!("cang launch config requires a {WORKSPACE_TARGET} mount"))
}

pub fn validate_mounts(mounts: &[BindMount]) -> Result<()> {
    if mounts.is_empty() {
        anyhow::bail!("cang launch config requires at least one bind mount");
    }
    let mut tags = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for mount in mounts {
        if mount.tag.trim().is_empty() {
            anyhow::bail!("cang bind mount tag cannot be empty");
        }
        let canonical_target = canonical_mount_target(&mount.target)?;
        if canonical_target != mount.target {
            anyhow::bail!(
                "cang bind mount target '{}' must be canonical absolute path '{}'",
                mount.target,
                canonical_target
            );
        }
        if mount.target.contains(".config/codex")
            || mount.source.to_string_lossy().contains(".config/codex")
        {
            anyhow::bail!("cang bind mounts must not include .config/codex");
        }
        if !tags.insert(mount.tag.as_str()) {
            anyhow::bail!("cang bind mount tag '{}' is duplicated", mount.tag);
        }
        if !targets.insert(mount.target.as_str()) {
            anyhow::bail!("cang bind mount target '{}' is duplicated", mount.target);
        }
    }
    Ok(())
}

/// The `/workspace` bind mount every host-directory launch contributes. A launch
/// that serves `/workspace` from its own disk has none, which is why the
/// requirement is stated where the disks are known rather than inside
/// [`validate_mounts`].
pub(crate) fn require_workspace_mount(mounts: &[BindMount]) -> Result<()> {
    workspace_mount(mounts).map(|_| ())
}
