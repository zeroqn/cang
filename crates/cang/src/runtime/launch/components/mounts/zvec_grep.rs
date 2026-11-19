//! `zvec-grep` home bind mount contribution.
//!
//! This file owns only the `.zvec-grep` mount contribution; it does not define
//! new mount policy or validation behavior. `zg` keeps its runtime/daemon state
//! and its embedding-model cache under `ZVEC_GREP_HOME` (default `~/.zvec-grep`),
//! so the task user needs a home it owns rather than a root-owned directory the
//! guest bootstrap created.

use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::runtime::launch::components::mounts::resolve_dir;

use crate::runtime::launch::config::{BindMount, ZVEC_GREP_TAG, ZVEC_GREP_TARGET};

pub(crate) fn prepare(home_dir: &Path) -> Result<BindMount> {
    let zvec_grep_dir = home_dir.join(".zvec-grep");
    fs::create_dir_all(&zvec_grep_dir)
        .map_err(|err| anyhow::anyhow!("failed to create '{}': {err}", zvec_grep_dir.display()))?;
    let zvec_grep_dir = resolve_dir(&zvec_grep_dir)?;
    Ok(super::bind_mount(
        &zvec_grep_dir,
        ZVEC_GREP_TAG,
        ZVEC_GREP_TARGET,
    ))
}
