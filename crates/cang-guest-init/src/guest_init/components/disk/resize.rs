use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

use crate::guest_init::cli::ResizeTarget;
use crate::guest_init::command;
use crate::guest_init::components::env::CangEnv;

pub(in crate::guest_init) fn run(target: ResizeTarget) -> Result<()> {
    let env = CangEnv::from_process_env()?;
    run_with_ops(target, &env, &SystemResizeOps)
}

fn run_with_ops(target: ResizeTarget, env: &CangEnv, ops: &impl ResizeOps) -> Result<()> {
    let mount = target.mount_point();
    let disk = ops.find_disk(target, env)?;
    ops.create_dir_all(mount)?;
    let was_mounted = ops.is_mounted(mount)?;
    if !was_mounted {
        ops.mount_btrfs(&disk, mount)
            .with_context(|| format!("failed to mount internal {} btrfs disk", target.name()))?;
    }
    ops.resize_max(mount).with_context(|| {
        format!(
            "failed to resize internal {} btrfs filesystem",
            target.name()
        )
    })?;
    if !was_mounted {
        ops.unmount(mount).with_context(|| {
            format!("failed to unmount internal {} resize mount", target.name())
        })?;
    }
    Ok(())
}

trait ResizeOps {
    fn find_disk(&self, target: ResizeTarget, env: &CangEnv) -> Result<PathBuf>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn is_mounted(&self, path: &Path) -> Result<bool>;
    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()>;
    fn resize_max(&self, mount: &Path) -> Result<()>;
    fn unmount(&self, mount: &Path) -> Result<()>;
}

#[derive(Debug, Clone, Copy)]
struct SystemResizeOps;

impl ResizeOps for SystemResizeOps {
    fn find_disk(&self, target: ResizeTarget, env: &CangEnv) -> Result<PathBuf> {
        match target {
            ResizeTarget::Nix => crate::guest_init::components::disk::nix::find_disk(
                &env.nix_disk_label,
                &env.nix_disk_id,
            ),
            ResizeTarget::Containers => crate::guest_init::components::disk::containers::find_disk(
                &env.containers_disk_label,
                &env.containers_disk_id,
            ),
        }
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        crate::guest_init::fs::create_dir_all(path)
    }

    fn is_mounted(&self, path: &Path) -> Result<bool> {
        command::status_ok("findmnt", &["-rn", path_str(path)?])
    }

    fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
        command::run("mount", &["-t", "btrfs", path_str(disk)?, path_str(mount)?])
    }

    fn resize_max(&self, mount: &Path) -> Result<()> {
        command::run("btrfs", &["filesystem", "resize", "max", path_str(mount)?])
    }

    fn unmount(&self, mount: &Path) -> Result<()> {
        command::run("umount", &[path_str(mount)?])
    }
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

impl ResizeTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Nix => "/nix",
            Self::Containers => "container storage",
        }
    }

    fn mount_point(self) -> &'static Path {
        match self {
            Self::Nix => Path::new("/run/cang/resize-nix"),
            Self::Containers => Path::new("/run/cang/resize-containers"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_init::components::env::{
        ContainerStoreBackend, RAW_BUILD_DISK_ID, RAW_BUILD_DISK_LABEL, RAW_CONTAINER_DISK_ID,
        RAW_CONTAINER_DISK_LABEL, RAW_NIX_DISK_ID, RAW_NIX_DISK_LABEL,
    };
    use std::cell::RefCell;

    #[test]
    fn resize_mounts_resizes_and_unmounts_selected_nix_disk() {
        let ops = FakeResizeOps::new(false);
        run_with_ops(ResizeTarget::Nix, &test_env(), &ops).unwrap();

        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "find:nix",
                "mkdir:/run/cang/resize-nix",
                "findmnt:/run/cang/resize-nix",
                "mount:/dev/vda:/run/cang/resize-nix",
                "resize:/run/cang/resize-nix",
                "umount:/run/cang/resize-nix",
            ]
        );
    }

    #[test]
    fn resize_reuses_pre_mounted_private_path_without_unmounting() {
        let ops = FakeResizeOps::new(true);
        run_with_ops(ResizeTarget::Containers, &test_env(), &ops).unwrap();

        assert_eq!(
            ops.operations.borrow().as_slice(),
            [
                "find:containers",
                "mkdir:/run/cang/resize-containers",
                "findmnt:/run/cang/resize-containers",
                "resize:/run/cang/resize-containers",
            ]
        );
    }

    struct FakeResizeOps {
        mounted: bool,
        operations: RefCell<Vec<String>>,
    }

    impl FakeResizeOps {
        fn new(mounted: bool) -> Self {
            Self {
                mounted,
                operations: RefCell::new(Vec::new()),
            }
        }

        fn record(&self, operation: impl Into<String>) {
            self.operations.borrow_mut().push(operation.into());
        }
    }

    impl ResizeOps for FakeResizeOps {
        fn find_disk(&self, target: ResizeTarget, _env: &CangEnv) -> Result<PathBuf> {
            match target {
                ResizeTarget::Nix => {
                    self.record("find:nix");
                    Ok(PathBuf::from("/dev/vda"))
                }
                ResizeTarget::Containers => {
                    self.record("find:containers");
                    Ok(PathBuf::from("/dev/vdb"))
                }
            }
        }

        fn create_dir_all(&self, path: &Path) -> Result<()> {
            self.record(format!("mkdir:{}", path.display()));
            Ok(())
        }

        fn is_mounted(&self, path: &Path) -> Result<bool> {
            self.record(format!("findmnt:{}", path.display()));
            Ok(self.mounted)
        }

        fn mount_btrfs(&self, disk: &Path, mount: &Path) -> Result<()> {
            self.record(format!("mount:{}:{}", disk.display(), mount.display()));
            Ok(())
        }

        fn resize_max(&self, mount: &Path) -> Result<()> {
            self.record(format!("resize:{}", mount.display()));
            Ok(())
        }

        fn unmount(&self, mount: &Path) -> Result<()> {
            self.record(format!("umount:{}", mount.display()));
            Ok(())
        }
    }

    fn test_env() -> CangEnv {
        CangEnv {
            nix_overlay: false,
            nix_host_overlay: false,
            containers_storage: false,
            container_store_backend: ContainerStoreBackend::RawDisk,
            use_passt: false,
            gpu_drm: false,
            wayland: false,
            permissions: Default::default(),
            enter_as_root: false,
            host_uid: None,
            host_gid: None,
            nix_disk_id: RAW_NIX_DISK_ID.to_owned(),
            nix_disk_label: RAW_NIX_DISK_LABEL.to_owned(),
            containers_disk_id: RAW_CONTAINER_DISK_ID.to_owned(),
            containers_disk_label: RAW_CONTAINER_DISK_LABEL.to_owned(),
            build_disk_id: RAW_BUILD_DISK_ID.to_owned(),
            build_disk_label: RAW_BUILD_DISK_LABEL.to_owned(),
        }
    }
}
