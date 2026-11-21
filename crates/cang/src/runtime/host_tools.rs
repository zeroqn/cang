//! Host runtime helper resolution for cang.
//!
//! Nix installs cang's runtime helpers under `$out/libexec/cang-helpers` so
//! `$out/bin/cang` can stay a raw ELF instead of a shell wrapper. Development
//! builds still fall back to ordinary `PATH` lookups.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const HELPER_BINARY_DIR_ENV: &str = "CANG_HELPER_BINARY_DIR";
const HELPER_BINARY_DIR: &str = "libexec/cang-helpers";
const DEFAULT_SECCOMP_POLICY: &str = "share/cang/seccomp/default.json";
const RENDER_SERVER_SECCOMP_POLICY: &str = "share/cang/seccomp/render-server.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeTool {
    Buildah,
    Btrfs,
    MkfsBtrfs,
    Blkid,
    Pasta,
    Passt,
    Strace,
    VirglRenderServer,
    RelayPump,
}

impl RuntimeTool {
    pub(crate) fn basename(self) -> &'static str {
        match self {
            Self::Buildah => "buildah",
            Self::Btrfs => "btrfs",
            Self::MkfsBtrfs => "mkfs.btrfs",
            Self::Blkid => "blkid",
            Self::Pasta => "pasta",
            Self::Passt => "passt",
            Self::Strace => "strace",
            Self::VirglRenderServer => "virgl_render_server",
            Self::RelayPump => "relay-pump",
        }
    }

    fn override_env(self) -> &'static str {
        match self {
            Self::Buildah => "CANG_BUILDAH",
            Self::Btrfs => "CANG_BTRFS",
            Self::MkfsBtrfs => "CANG_MKFS_BTRFS",
            Self::Blkid => "CANG_BLKID",
            Self::Pasta => "CANG_PASTA",
            Self::Passt => "CANG_PASST",
            Self::Strace => "CANG_STRACE",
            Self::VirglRenderServer => "CANG_VIRGL_RENDER_SERVER",
            Self::RelayPump => "CANG_RELAY_PUMP",
        }
    }
}

pub(crate) fn runtime_tool_program(tool: RuntimeTool) -> OsString {
    runtime_tool_program_with(
        tool,
        std::env::var_os(tool.override_env()),
        std::env::var_os(HELPER_BINARY_DIR_ENV),
        std::env::current_exe().ok(),
    )
}

fn runtime_tool_program_with(
    tool: RuntimeTool,
    tool_override: Option<OsString>,
    helper_dir_override: Option<OsString>,
    current_exe: Option<PathBuf>,
) -> OsString {
    if let Some(program) = nonempty_os_string(tool_override) {
        return program;
    }

    if let Some(helper_dir) = nonempty_os_string(helper_dir_override) {
        return PathBuf::from(helper_dir)
            .join(tool.basename())
            .into_os_string();
    }

    if let Some(exe) = current_exe
        && let Some(program) = package_helper_path_for_exe(&exe, tool)
        && program.is_file()
    {
        return program.into_os_string();
    }

    OsString::from(tool.basename())
}

fn nonempty_os_string(value: Option<OsString>) -> Option<OsString> {
    value.filter(|value| !value.as_os_str().is_empty())
}

pub(crate) fn package_helper_path_for_exe(exe: &Path, tool: RuntimeTool) -> Option<PathBuf> {
    package_root_from_exe(exe).map(|root| root.join(HELPER_BINARY_DIR).join(tool.basename()))
}

pub(crate) fn default_seccomp_policy_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| package_default_seccomp_policy_path_for_exe(&exe))
}

pub(crate) fn package_default_seccomp_policy_path_for_exe(exe: &Path) -> Option<PathBuf> {
    package_root_from_exe(exe).map(|root| root.join(DEFAULT_SECCOMP_POLICY))
}

pub(crate) fn package_render_server_seccomp_policy_path_for_exe(exe: &Path) -> Option<PathBuf> {
    package_root_from_exe(exe).map(|root| root.join(RENDER_SERVER_SECCOMP_POLICY))
}

pub(crate) fn package_root_from_exe(exe: &Path) -> Option<PathBuf> {
    let executable_dir = exe.parent()?;
    if matches!(
        executable_dir.file_name().and_then(OsStr::to_str),
        Some("bin" | "libexec")
    ) {
        return executable_dir.parent().map(Path::to_path_buf);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_helper_paths_are_derived_from_bin_cang_without_store_hashes() {
        assert_eq!(
            package_root_from_exe(Path::new("/nix/store/hash-cang/bin/cang")),
            Some(PathBuf::from("/nix/store/hash-cang"))
        );
        assert_eq!(
            package_helper_path_for_exe(
                Path::new("/nix/store/hash-cang/bin/cang"),
                RuntimeTool::Buildah
            ),
            Some(PathBuf::from(
                "/nix/store/hash-cang/libexec/cang-helpers/buildah"
            ))
        );
    }

    #[test]
    fn the_relay_pump_is_a_package_helper_like_the_others() {
        assert_eq!(RuntimeTool::RelayPump.basename(), "relay-pump");
        assert_eq!(RuntimeTool::RelayPump.override_env(), "CANG_RELAY_PUMP");
        assert_eq!(
            package_helper_path_for_exe(
                Path::new("/nix/store/hash-cang/bin/cang"),
                RuntimeTool::RelayPump
            ),
            Some(PathBuf::from(
                "/nix/store/hash-cang/libexec/cang-helpers/relay-pump"
            ))
        );
    }

    #[test]
    fn legacy_libexec_executable_shape_still_resolves_package_root() {
        assert_eq!(
            package_helper_path_for_exe(
                Path::new("/nix/store/hash-cang/libexec/cang"),
                RuntimeTool::Passt
            ),
            Some(PathBuf::from(
                "/nix/store/hash-cang/libexec/cang-helpers/passt"
            ))
        );
    }

    #[test]
    fn default_seccomp_policy_path_is_package_relative() {
        assert_eq!(
            package_default_seccomp_policy_path_for_exe(Path::new("/nix/store/hash-cang/bin/cang")),
            Some(PathBuf::from(
                "/nix/store/hash-cang/share/cang/seccomp/default.json"
            ))
        );
        assert_eq!(
            package_default_seccomp_policy_path_for_exe(Path::new(
                "/nix/store/hash-cang/libexec/cang"
            )),
            Some(PathBuf::from(
                "/nix/store/hash-cang/share/cang/seccomp/default.json"
            ))
        );
    }

    #[test]
    fn explicit_tool_override_wins_before_helper_dir_and_package_paths() {
        assert_eq!(
            runtime_tool_program_with(
                RuntimeTool::Btrfs,
                Some(OsString::from("/custom/btrfs")),
                Some(OsString::from("/helpers")),
                Some(PathBuf::from("/nix/store/hash-cang/bin/cang")),
            ),
            OsString::from("/custom/btrfs")
        );
    }

    #[test]
    fn explicit_helper_dir_wins_before_package_relative_paths() {
        assert_eq!(
            runtime_tool_program_with(
                RuntimeTool::MkfsBtrfs,
                None,
                Some(OsString::from("/helpers")),
                Some(PathBuf::from("/nix/store/hash-cang/bin/cang")),
            ),
            OsString::from("/helpers/mkfs.btrfs")
        );
    }

    #[test]
    fn missing_package_helper_falls_back_to_bare_program_for_development() {
        assert_eq!(
            runtime_tool_program_with(
                RuntimeTool::Blkid,
                None,
                None,
                Some(PathBuf::from("/nix/store/hash-cang/bin/cang")),
            ),
            OsString::from("blkid")
        );
    }
}
