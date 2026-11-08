use anyhow::{Context, Result, anyhow, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawDiskSpec {
    pub(crate) file_name: &'static str,
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) size_bytes: u64,
    pub(crate) size_hint: &'static str,
    pub(crate) diagnostic_name: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawBtrfsDisk {
    pub(crate) path: PathBuf,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) size_bytes: u64,
    pub(crate) status: RawBtrfsDiskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawBtrfsDiskStatus {
    Created,
    Reused,
}

pub(crate) trait RawImageCommandRunner {
    fn mkfs_btrfs(&self, path: &Path, label: &str, spec: &RawDiskSpec) -> Result<()>;
    fn probe_fs_type(&self, path: &Path, spec: &RawDiskSpec) -> Result<String>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostRawImageCommandRunner;

impl RawImageCommandRunner for HostRawImageCommandRunner {
    fn mkfs_btrfs(&self, path: &Path, label: &str, spec: &RawDiskSpec) -> Result<()> {
        let output = Command::new(runtime_tool_program(RuntimeTool::MkfsBtrfs))
            .arg("-f")
            .arg("-L")
            .arg(label)
            .arg(path)
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => anyhow!(
                    "mkfs.btrfs is required to create the {}; install btrfs-progs and retry",
                    spec.diagnostic_name
                ),
                _ => err.into(),
            })
            .with_context(|| format!("failed to run mkfs.btrfs for '{}'", path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.is_empty() {
                anyhow::bail!("mkfs.btrfs failed for '{}'", path.display());
            }
            anyhow::bail!("mkfs.btrfs failed for '{}': {stderr}", path.display());
        }

        Ok(())
    }

    fn probe_fs_type(&self, path: &Path, spec: &RawDiskSpec) -> Result<String> {
        let output = Command::new(runtime_tool_program(RuntimeTool::Blkid))
            .arg("-o")
            .arg("value")
            .arg("-s")
            .arg("TYPE")
            .arg(path)
            .output()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => anyhow!(
                    "blkid is required to validate the existing {}; install util-linux and retry",
                    spec.diagnostic_name
                ),
                _ => err.into(),
            })
            .with_context(|| format!("failed to run blkid for '{}'", path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.is_empty() {
                anyhow::bail!("failed to detect filesystem type for '{}'", path.display());
            }
            anyhow::bail!(
                "failed to detect filesystem type for '{}': {stderr}",
                path.display()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

pub(crate) fn prepare_with_runner(
    state_root: &Path,
    spec: &RawDiskSpec,
    runner: &impl RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    let path = raw_disk_path(state_root, spec);
    prepare_path_with_runner(&path, spec, runner)
}

pub(crate) fn raw_disk_path(state_root: &Path, spec: &RawDiskSpec) -> PathBuf {
    state_root.join(spec.file_name)
}

pub(crate) fn grow_existing_with_runner(
    state_root: &Path,
    spec: &RawDiskSpec,
    target_size_bytes: u64,
    runner: &impl RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    let path = raw_disk_path(state_root, spec);
    let current_len = inspect_existing_regular_file(&path, spec)?
        .ok_or_else(|| anyhow!("{} '{}' does not exist; run cang with --container-store raw-disk first or use reset --force to create it", spec.diagnostic_name, path.display()))?;
    if target_size_bytes <= current_len {
        bail!(
            "requested {} size {} bytes must be greater than current size {} bytes for '{}'",
            spec.diagnostic_name,
            target_size_bytes,
            current_len,
            path.display()
        );
    }
    validate_existing(&path, spec, runner)?;
    extend_file(&path, target_size_bytes, spec)?;

    Ok(RawBtrfsDisk {
        size_bytes: target_size_bytes,
        ..disk(path, spec, RawBtrfsDiskStatus::Reused)
    })
}

/// Grow an existing disk to at least `target_size_bytes`, accepting an image that
/// is already at the target.
///
/// The equal case is the retry path: a resize run that grew the host image and
/// then failed in the guest leaves exactly that state, and rerunning the command
/// has to reach the guest again instead of demanding a bigger size.
pub(crate) fn grow_existing_at_least_with_runner(
    state_root: &Path,
    spec: &RawDiskSpec,
    target_size_bytes: u64,
    runner: &impl RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    let path = raw_disk_path(state_root, spec);
    let current_len = inspect_existing_regular_file(&path, spec)?.ok_or_else(|| {
        anyhow!(
            "{} '{}' does not exist; launch cang once in this workspace so it is created, then retry",
            spec.diagnostic_name,
            path.display()
        )
    })?;
    if target_size_bytes < current_len {
        bail!(
            "requested {} size {} bytes must not be smaller than its current size {} bytes for '{}'",
            spec.diagnostic_name,
            target_size_bytes,
            current_len,
            path.display()
        );
    }
    validate_existing(&path, spec, runner)?;
    if target_size_bytes > current_len {
        extend_file(&path, target_size_bytes, spec)?;
    }

    Ok(RawBtrfsDisk {
        size_bytes: target_size_bytes,
        ..disk(path, spec, RawBtrfsDiskStatus::Reused)
    })
}

fn extend_file(path: &Path, target_size_bytes: u64, spec: &RawDiskSpec) -> Result<()> {
    File::options()
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open '{}' for resize", path.display()))?
        .set_len(target_size_bytes)
        .with_context(|| {
            format!(
                "failed to grow {} '{}' to {} bytes",
                spec.diagnostic_name,
                path.display(),
                target_size_bytes
            )
        })
}

pub(crate) fn recreate_with_runner(
    state_root: &Path,
    spec: &RawDiskSpec,
    runner: &impl RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    let path = raw_disk_path(state_root, spec);
    if inspect_existing_regular_file(&path, spec)?.is_some() {
        fs::remove_file(&path).with_context(|| {
            format!(
                "failed to remove existing {} '{}'",
                spec.diagnostic_name,
                path.display()
            )
        })?;
    }
    prepare_path_with_runner(&path, spec, runner)
}

pub(crate) fn inspect_existing_regular_file(
    path: &Path,
    spec: &RawDiskSpec,
) -> Result<Option<u64>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect '{}'", path.display()));
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        bail!(
            "existing {} path '{}' is a symlink; refusing to follow it",
            spec.diagnostic_name,
            path.display()
        );
    }
    if !file_type.is_file() {
        bail!(
            "existing {} path '{}' is not a regular file; refusing to overwrite it",
            spec.diagnostic_name,
            path.display()
        );
    }
    Ok(Some(metadata.len()))
}

fn prepare_path_with_runner(
    path: &Path,
    spec: &RawDiskSpec,
    runner: &impl RawImageCommandRunner,
) -> Result<RawBtrfsDisk> {
    if path.exists() {
        validate_existing(path, spec, runner)?;
        return Ok(disk(path.to_path_buf(), spec, RawBtrfsDiskStatus::Reused));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }

    let file = File::create_new(path).with_context(|| {
        format!(
            "failed to create {} '{}'",
            spec.diagnostic_name,
            path.display()
        )
    })?;
    file.set_len(spec.size_bytes).with_context(|| {
        format!(
            "failed to set {} '{}' to {} bytes",
            spec.diagnostic_name,
            path.display(),
            spec.size_bytes
        )
    })?;
    drop(file);

    if let Err(err) = runner.mkfs_btrfs(path, spec.label, spec) {
        let _ = fs::remove_file(path);
        return Err(err).with_context(|| {
            format!(
                "failed to format new {} '{}' as btrfs",
                spec.diagnostic_name,
                path.display()
            )
        });
    }

    Ok(disk(path.to_path_buf(), spec, RawBtrfsDiskStatus::Created))
}

fn validate_existing(
    path: &Path,
    spec: &RawDiskSpec,
    runner: &impl RawImageCommandRunner,
) -> Result<()> {
    let metadata_len = inspect_existing_regular_file(path, spec)?.ok_or_else(|| {
        anyhow!(
            "existing {} '{}' disappeared during validation",
            spec.diagnostic_name,
            path.display()
        )
    })?;

    if metadata_len < spec.size_bytes {
        anyhow::bail!(
            "existing {} '{}' is {} bytes, below the required {} bytes; stop the VM, extend it with 'truncate -s {} {}', then retry",
            spec.diagnostic_name,
            path.display(),
            metadata_len,
            spec.size_bytes,
            spec.size_hint,
            path.display()
        );
    }

    let fs_type = runner.probe_fs_type(path, spec)?;
    if fs_type != "btrfs" {
        anyhow::bail!(
            "existing {} '{}' has filesystem type '{}', expected btrfs; refusing to reformat automatically",
            spec.diagnostic_name,
            path.display(),
            if fs_type.is_empty() {
                "unknown"
            } else {
                &fs_type
            }
        );
    }

    Ok(())
}

fn disk(path: PathBuf, spec: &RawDiskSpec, status: RawBtrfsDiskStatus) -> RawBtrfsDisk {
    RawBtrfsDisk {
        path,
        id: spec.id.to_owned(),
        label: spec.label.to_owned(),
        size_bytes: spec.size_bytes,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const TEST_SPEC: RawDiskSpec = RawDiskSpec {
        file_name: "disk.raw",
        id: "disk",
        label: "DISK",
        size_bytes: 8,
        size_hint: "8B",
        diagnostic_name: "test raw disk",
    };

    #[test]
    fn grow_existing_rejects_equal_smaller_and_missing_sizes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runner = test_support::FakeRunner::with_probe("btrfs");
        let path = temp.path().join(TEST_SPEC.file_name);

        let missing = grow_existing_with_runner(temp.path(), &TEST_SPEC, 16, &runner)
            .expect_err("missing disk should fail");
        assert!(format!("{missing:#}").contains("does not exist"));

        fs::write(&path, b"12345678").expect("disk file");
        let equal = grow_existing_with_runner(temp.path(), &TEST_SPEC, 8, &runner)
            .expect_err("equal size should fail");
        assert!(format!("{equal:#}").contains("greater than current size"));

        let smaller = grow_existing_with_runner(temp.path(), &TEST_SPEC, 7, &runner)
            .expect_err("smaller size should fail");
        assert!(format!("{smaller:#}").contains("greater than current size"));
    }

    #[test]
    fn grow_existing_rejects_symlink_before_probe_or_growth() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        fs::write(&target, b"12345678").expect("target");
        symlink(&target, temp.path().join(TEST_SPEC.file_name)).expect("symlink");
        let runner = test_support::FakeRunner::with_probe("btrfs");

        let err = grow_existing_with_runner(temp.path(), &TEST_SPEC, 16, &runner)
            .expect_err("symlink should fail");

        assert!(format!("{err:#}").contains("symlink"));
        assert_eq!(runner.mkfs_call_count(), 0);
        assert_eq!(fs::metadata(&target).expect("target metadata").len(), 8);
    }

    #[test]
    fn recreate_rejects_symlink_before_delete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        fs::write(&target, b"keep").expect("target");
        symlink(&target, temp.path().join(TEST_SPEC.file_name)).expect("symlink");
        let runner = test_support::FakeRunner::default();

        let err = recreate_with_runner(temp.path(), &TEST_SPEC, &runner)
            .expect_err("symlink should fail");

        assert!(format!("{err:#}").contains("symlink"));
        assert_eq!(fs::read(&target).expect("target"), b"keep");
        assert_eq!(runner.mkfs_call_count(), 0);
    }

    #[test]
    fn grow_existing_extends_file_after_validation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join(TEST_SPEC.file_name);
        fs::write(&path, b"12345678").expect("disk file");
        let runner = test_support::FakeRunner::with_probe("btrfs");

        let disk = grow_existing_with_runner(temp.path(), &TEST_SPEC, 16, &runner)
            .expect("grow should work");

        assert_eq!(disk.path, path);
        assert_eq!(disk.size_bytes, 16);
        assert_eq!(fs::metadata(&disk.path).expect("metadata").len(), 16);
        assert_eq!(runner.mkfs_call_count(), 0);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{RawDiskSpec, RawImageCommandRunner};
    use anyhow::{Result, anyhow};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    pub(crate) struct FakeRunner {
        pub(crate) mkfs_calls: RefCell<Vec<(PathBuf, String, &'static str)>>,
        probe_results: RefCell<VecDeque<Result<String, String>>>,
    }

    impl FakeRunner {
        /// One queued probe answer per attached disk, in prepare order.
        pub(crate) fn with_probes(fs_types: &[&str]) -> Self {
            let runner = Self::default();
            for fs_type in fs_types {
                runner
                    .probe_results
                    .borrow_mut()
                    .push_back(Ok((*fs_type).to_owned()));
            }
            runner
        }

        pub(crate) fn with_probe(fs_type: &str) -> Self {
            let runner = Self::default();
            runner
                .probe_results
                .borrow_mut()
                .push_back(Ok(fs_type.to_owned()));
            runner
        }

        pub(crate) fn mkfs_call_count(&self) -> usize {
            self.mkfs_calls.borrow().len()
        }

        pub(crate) fn push_probe_error(&self, message: &str) {
            self.probe_results
                .borrow_mut()
                .push_back(Err(message.to_owned()));
        }
    }

    impl RawImageCommandRunner for FakeRunner {
        fn mkfs_btrfs(&self, path: &Path, label: &str, spec: &RawDiskSpec) -> Result<()> {
            self.mkfs_calls.borrow_mut().push((
                path.to_path_buf(),
                label.to_owned(),
                spec.diagnostic_name,
            ));
            Ok(())
        }

        fn probe_fs_type(&self, _path: &Path, _spec: &RawDiskSpec) -> Result<String> {
            match self.probe_results.borrow_mut().pop_front() {
                Some(Ok(value)) => Ok(value),
                Some(Err(message)) => Err(anyhow!(message)),
                None => Err(anyhow!("unexpected probe")),
            }
        }
    }
}
