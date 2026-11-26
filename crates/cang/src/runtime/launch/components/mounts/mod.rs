//! Development bind mount contributors.
//!
//! Each owner file contributes one built-in mount family. The aggregate
//! preserves order and validation; owner files do not introduce new policy.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::launch::config::{BindMount, validate_mounts};
use crate::state::StateLayout;

mod cargo;
mod codex;
mod cortexkit;
mod dirge;
mod omp;
mod pi;
mod workspace;
mod zvec_grep;

/// `include_workspace_mounts` is false for a launch that serves `/workspace` from
/// its own disk: the guest mounts that disk itself, and the cargo home it binds
/// comes from inside the tree, so the host contributes neither the `/workspace`
/// share nor a `~/.cargo` directory.
pub(crate) fn prepare_dev_mounts(
    workspace_dir: &Path,
    home_dir: &Path,
    state_layout: &StateLayout,
    include_workspace_mounts: bool,
) -> Result<Vec<BindMount>> {
    let mut mounts = Vec::new();
    if include_workspace_mounts {
        mounts.push(workspace::bind_mount(workspace_dir));
    }
    mounts.extend([
        codex::prepare(home_dir)?,
        omp::prepare(home_dir)?,
        pi::prepare(home_dir)?,
        zvec_grep::prepare(home_dir)?,
        cortexkit::prepare(home_dir)?,
    ]);
    mounts.extend(dirge::prepare(home_dir)?);
    // Even a workspace-disk launch keeps this mount: at boot it is the registry
    // the guest seeds the tree's cargo home from, and only then does the guest
    // bind the tree's own `cargo/` over it.
    mounts.push(cargo::prepare(state_layout)?);
    validate_mounts(&mounts)?;
    Ok(mounts)
}

fn bind_mount(source: &Path, tag: &str, target: &str) -> BindMount {
    BindMount::directory(source, tag, target)
}

pub(crate) fn resolve_dir(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path)
        .with_context(|| format!("failed to inspect mount source '{}'", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::runtime::launch::config::{
        CARGO_TAG, CARGO_TARGET, CODEX_TAG, CODEX_TARGET, CORTEXKIT_TAG, CORTEXKIT_TARGET,
        DIRGE_CONFIG_TAG, DIRGE_CONFIG_TARGET, DIRGE_DATA_TAG, DIRGE_DATA_TARGET, DIRGE_HOME_TAG,
        DIRGE_HOME_TARGET, OMP_TAG, OMP_TARGET, PI_TAG, PI_TARGET, WORKSPACE_TAG, WORKSPACE_TARGET,
        ZVEC_GREP_TAG, ZVEC_GREP_TARGET,
    };
    use crate::state;

    #[test]
    fn dev_mounts_preserve_targets_tags_and_sources() {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let workspace = dir.path().join("project");
        let home = dir.path().join("home");
        fs::create_dir_all(&workspace).expect("workspace should exist");
        let state_layout = state::resolve_state_layout_from_parts(
            &workspace,
            Some(dir.path().join("state").as_path()),
            Some(home.as_path()),
            None,
        )
        .expect("state layout should resolve");

        let mounts = prepare_dev_mounts(&workspace, &home, &state_layout, true)
            .expect("mounts should prepare");

        assert_eq!(mounts.len(), 10);
        assert_mount(&mounts[0], &workspace, WORKSPACE_TAG, WORKSPACE_TARGET);
        assert_mount(&mounts[1], &home.join(".codex"), CODEX_TAG, CODEX_TARGET);
        assert_mount(&mounts[2], &home.join(".omp"), OMP_TAG, OMP_TARGET);
        assert_mount(&mounts[3], &home.join(".pi"), PI_TAG, PI_TARGET);
        assert_mount(
            &mounts[4],
            &home.join(".zvec-grep"),
            ZVEC_GREP_TAG,
            ZVEC_GREP_TARGET,
        );
        assert_mount(
            &mounts[5],
            &home.join(".local/share/cortexkit"),
            CORTEXKIT_TAG,
            CORTEXKIT_TARGET,
        );
        assert_mount(
            &mounts[6],
            &home.join(".config/dirge"),
            DIRGE_CONFIG_TAG,
            DIRGE_CONFIG_TARGET,
        );
        assert_mount(
            &mounts[7],
            &home.join(".local/share/dirge"),
            DIRGE_DATA_TAG,
            DIRGE_DATA_TARGET,
        );
        assert_mount(
            &mounts[8],
            &home.join(".dirge"),
            DIRGE_HOME_TAG,
            DIRGE_HOME_TARGET,
        );
        assert_mount(
            &mounts[9],
            &state_layout.root_dir().join("cargo"),
            CARGO_TAG,
            CARGO_TARGET,
        );
        assert!(home.join(".codex").is_dir());
        assert!(home.join(".omp").is_dir());
        assert!(home.join(".pi").is_dir());
        assert!(home.join(".zvec-grep").is_dir());
        assert!(home.join(".local/share/cortexkit").is_dir());
        assert!(home.join(".config/dirge").is_dir());
        assert!(home.join(".local/share/dirge").is_dir());
        assert!(home.join(".dirge").is_dir());
        assert!(state_layout.root_dir().join("cargo").is_dir());
    }

    fn assert_mount(mount: &BindMount, source: &Path, tag: &str, target: &str) {
        assert_eq!(mount.source, source);
        assert_eq!(mount.tag, tag);
        assert_eq!(mount.target, target);
    }
}
