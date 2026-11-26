use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

use crate::naming::derive_workspace_slug;

const APP_DIR_NAME: &str = "cang";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateLayout {
    app_dir: PathBuf,
    root_dir: PathBuf,
}

impl StateLayout {
    pub(crate) fn app_dir(&self) -> &Path {
        &self.app_dir
    }

    pub(crate) fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub(crate) fn image_cache_dir(&self) -> PathBuf {
        self.app_dir.join("microvm").join("images")
    }

    fn new(app_dir: PathBuf, root_dir: PathBuf) -> Self {
        Self { app_dir, root_dir }
    }
}

pub(crate) fn resolve_state_layout_from_parts(
    cwd: &Path,
    xdg_state_home: Option<&Path>,
    home_dir: Option<&Path>,
    state_location_override: Option<&Path>,
) -> Result<StateLayout> {
    let default_location_root = default_state_location_root(xdg_state_home, home_dir)?;
    let location_root = state_location_override.unwrap_or(default_location_root.as_path());

    let app_dir = location_root.join(APP_DIR_NAME);
    Ok(StateLayout::new(
        app_dir.clone(),
        app_dir.join(derive_workspace_slug(cwd)),
    ))
}

fn default_state_location_root(
    xdg_state_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(path) = xdg_state_home {
        return Ok(path.to_path_buf());
    }

    let home_dir =
        home_dir.ok_or_else(|| anyhow!("HOME is not set and XDG_STATE_HOME is not available"))?;
    Ok(home_dir.join(".local").join("state"))
}

#[cfg(test)]
mod tests {
    use crate::state::{default_state_location_root, resolve_state_layout_from_parts};
    use std::path::Path;

    #[test]
    fn default_state_location_root_prefers_xdg_state_home() {
        let path = default_state_location_root(
            Some(Path::new("/tmp/xdg-state")),
            Some(Path::new("/tmp/home")),
        )
        .expect("xdg state home should resolve");

        assert_eq!(path, Path::new("/tmp/xdg-state"));
    }

    #[test]
    fn default_state_location_root_falls_back_to_home_local_state() {
        let path = default_state_location_root(None, Some(Path::new("/tmp/home")))
            .expect("fallback should work");

        assert_eq!(path, Path::new("/tmp/home/.local/state"));
    }

    #[test]
    fn resolve_state_layout_uses_default_xdg_state_root() {
        let layout = resolve_state_layout_from_parts(
            Path::new("/tmp/project"),
            Some(Path::new("/tmp/xdg-state")),
            Some(Path::new("/tmp/home")),
            None,
        )
        .expect("layout should resolve");

        assert_eq!(layout.root_dir(), Path::new("/tmp/xdg-state/cang/project"));
        assert_eq!(
            layout.image_cache_dir(),
            Path::new("/tmp/xdg-state/cang/microvm/images")
        );
        assert!(!layout.image_cache_dir().starts_with(layout.root_dir()));
    }

    #[test]
    fn resolve_state_layout_honors_config_override_and_appends_cang() {
        let layout = resolve_state_layout_from_parts(
            Path::new("/tmp/project"),
            Some(Path::new("/tmp/xdg-state")),
            Some(Path::new("/tmp/home")),
            Some(Path::new("/tmp/custom-root/")),
        )
        .expect("layout should resolve");

        assert_eq!(
            layout.root_dir(),
            Path::new("/tmp/custom-root/cang/project")
        );
    }

    #[test]
    fn resolve_state_layout_exposes_app_root_for_cross_workspace_management() {
        let layout = resolve_state_layout_from_parts(
            Path::new("/tmp/project"),
            Some(Path::new("/tmp/xdg-state")),
            Some(Path::new("/tmp/home")),
            None,
        )
        .expect("layout should resolve");

        assert_eq!(layout.app_dir(), Path::new("/tmp/xdg-state/cang"));
    }
}
