//! The workspace image's auto-grow: the host side of "the tree grows itself".
//!
//! A guest mount and a mid-run grow are mutually exclusive - virtio-blk fixes the
//! device size when the device is probed - so the decision is taken *between* runs.
//! The guest records what its tree had free right after it mounts the disk
//! (`/workspace/.cang/free-bytes`, written by `cang-guest-init`); the host cannot
//! read a btrfs image it has no view of, so the number comes back through one narrow
//! maintenance VM, shaped exactly like `cang nix gc`'s and `cang disk resize`'s.
//!
//! Then the rule (wayfinder tickets 24/27, bob's answer 2026-11-20):
//!
//! - free under a tenth of the image -> the image wants to grow;
//! - the step is twice what is in use plus 16 GiB, so the next grow is not one
//!   compile away;
//! - the host must keep twice the step's headroom, or the image is left alone and
//!   the refusal is printed with both numbers;
//! - `--workspace-size` is a *creation* size, so it is not a cap and the refusal
//!   names `cang disk resize --workspace-disk` instead;
//! - only the sparse file grows here. The guest expands the btrfs filesystem to the
//!   new device size when it mounts the disk, the same hand-over `cang disk resize`
//!   makes.

use anyhow::{Context, Result, anyhow, bail};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::cli::DiskMaintenanceOptions;
use crate::runtime::launch::components::persistent_disks::{
    raw_btrfs::HostRawImageCommandRunner,
    workspace::{self, AutoGrowDecision, WorkspaceDisk},
};
use crate::runtime::launch::config::LaunchConfig;

use super::disk_resize::{
    self, MaintenanceVmRunner, WORKSPACE_DISK_TASK_PREFIX, with_maintenance_guest,
};

const HOSTNAME: &str = "cang-workspace-grow-report";
/// The line the guest prints on its console, which is the host's stdin/stdout/stderr
/// for the maintenance VM: `cang-workspace-free-bytes<TAB><free><TAB><total>}`.
const REPORT_PREFIX: &str = "cang-workspace-free-bytes";

/// What auto-grow did, so the caller prints one line and moves on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoGrowOutcome {
    /// Nothing to do: enough free, no marker yet, an image that just grew, or a
    /// disk another process holds.
    Left,
    /// The image grew, from `from_bytes` to `to_bytes`, because it had `free_bytes`.
    Grew {
        from_bytes: u64,
        to_bytes: u64,
        free_bytes: u64,
    },
    /// Wanted, but the host cannot pay for it.
    Refused {
        from_bytes: u64,
        to_bytes: u64,
        free_bytes: u64,
        needed_bytes: u64,
        host_free_bytes: u64,
    },
}

/// Grow the workspace image if its tree is running out of room.
///
/// `options` carries the image and guest-init the maintenance VM must boot, so an
/// auto-grow uses the same image a launch would; a caller with less context passes
/// `None` and the image selection falls back to what `cang disk resize` uses.
pub(crate) fn auto_grow(
    path: &Path,
    size_bytes: u64,
    options: &DiskMaintenanceOptions,
    runner: &impl MaintenanceVmRunner,
) -> Result<AutoGrowOutcome> {
    let report = read_free_bytes(path, options, runner)?;
    decide_and_grow(
        size_bytes,
        report,
        host_free_bytes(path)?,
        |target_size_bytes| {
            workspace::grow_existing_at_least_with_runner(
                path,
                target_size_bytes,
                &HostRawImageCommandRunner,
            )
            .map(|disk| disk.size_bytes)
        },
    )
}

/// The policy half of the auto-grow, with the two host-touching steps injected.
///
/// Split out because the reading half (`read_free_bytes`) has to boot a maintenance
/// VM and materialise a task rootfs, which no unit test can do honestly - the tests
/// below drive this function with a report and a grow closure instead, and the live
/// arms are what prove the VM path.
pub(crate) fn decide_and_grow(
    size_bytes: u64,
    report: Option<(u64, u64)>,
    host_free_bytes: u64,
    grow: impl FnOnce(u64) -> Result<u64>,
) -> Result<AutoGrowOutcome> {
    let (free_bytes, _total_bytes) = match report {
        Some(report) => report,
        // No marker: the image has not been mounted by a guest that writes one yet,
        // or the guest predates it. Growing blind is not a guess worth making.
        None => return Ok(AutoGrowOutcome::Left),
    };
    match workspace::plan_auto_grow(size_bytes, free_bytes, host_free_bytes) {
        AutoGrowDecision::Leave => Ok(AutoGrowOutcome::Left),
        AutoGrowDecision::Grow { target_size_bytes } => {
            let to_bytes = grow(target_size_bytes)?;
            Ok(AutoGrowOutcome::Grew {
                from_bytes: size_bytes,
                to_bytes,
                free_bytes,
            })
        }
        AutoGrowDecision::Refuse {
            target_size_bytes,
            needed_bytes,
            host_free_bytes,
        } => Ok(AutoGrowOutcome::Refused {
            from_bytes: size_bytes,
            to_bytes: target_size_bytes,
            free_bytes,
            needed_bytes,
            host_free_bytes,
        }),
    }
}

/// Read the number the guest recorded in the tree, through one maintenance VM.
///
/// `Ok(None)` means the guest reported no marker: a workspace image can always be
/// mounted, but only a guest that ran this version writes the file.
pub(crate) fn read_free_bytes(
    path: &Path,
    options: &DiskMaintenanceOptions,
    runner: &impl MaintenanceVmRunner,
) -> Result<Option<(u64, u64)>> {
    let disk: WorkspaceDisk = validate_for_report(path)?;
    let layout = disk_resize::current_state_layout()?;
    // The guest mounts the image read-only and reads the marker; nothing about this
    // VM writes to the image, which is why it can run beside a lease it does not
    // take.
    let console = with_maintenance_guest(
        WORKSPACE_DISK_TASK_PREFIX,
        HOSTNAME,
        vec![
            "internal".to_owned(),
            "workspace".to_owned(),
            "free-bytes".to_owned(),
        ],
        vec![workspace::attachment(&disk)],
        workspace::env_pairs(&disk).to_vec(),
        &layout,
        options,
        |config: &LaunchConfig, task_state_dir: &Path| runner.run_capturing(config, task_state_dir),
    )
    .context("failed to read the workspace image's free-space marker")?;

    Ok(parse_report(&console))
}

fn validate_for_report(path: &Path) -> Result<WorkspaceDisk> {
    let Some(len) = fs::symlink_metadata(path)
        .map(|metadata| metadata.len())
        .map(Some)
        .or_else(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(err)
            }
        })?
    else {
        bail!(
            "no cang workspace disk at '{}': a launch creates the image, so an auto-grow has nothing to read",
            path.display()
        );
    };
    Ok(WorkspaceDisk {
        path: path.to_path_buf(),
        size_bytes: len,
        status: workspace::WorkspaceDiskStatus::Reused,
    })
}

/// The guest's report line, as `(free, total)`.
fn parse_report(console: &[u8]) -> Option<(u64, u64)> {
    let text = String::from_utf8_lossy(console);
    text.lines()
        .find_map(|line| line.trim().strip_prefix(REPORT_PREFIX))
        .and_then(|rest| {
            let mut fields = rest.split_whitespace();
            let free = fields.next()?.parse().ok()?;
            let total = fields.next()?.parse().ok()?;
            Some((free, total))
        })
}

/// Free bytes on the filesystem the image lives on, which is what the headroom
/// rule spends.
fn host_free_bytes(path: &Path) -> Result<u64> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| Path::new(".").to_path_buf(), Path::to_path_buf);
    let c_path = std::ffi::CString::new(parent.as_os_str().as_bytes())
        .with_context(|| format!("'{}' is not a usable path", parent.display()))?;
    // SAFETY: `statvfs` writes into a buffer this call owns and reads only `c_path`.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) };
    if rc != 0 {
        return Err(anyhow!(
            "failed to read free space for '{}': {}",
            parent.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(stats.f_bavail as u64 * stats.f_frsize as u64)
}

/// The one line a grow prints.
pub(crate) fn grew_line(outcome: &AutoGrowOutcome) -> Option<String> {
    let AutoGrowOutcome::Grew {
        from_bytes,
        to_bytes,
        free_bytes,
    } = outcome
    else {
        return None;
    };
    Some(format!(
        "cang: grew the workspace image from {} to {} because it had {} bytes free",
        human_bytes(*from_bytes),
        human_bytes(*to_bytes),
        free_bytes
    ))
}

/// The one line a refusal prints: both numbers, and the command that does it by hand.
pub(crate) fn refused_line(path: &Path, outcome: &AutoGrowOutcome) -> Option<String> {
    let AutoGrowOutcome::Refused {
        from_bytes,
        to_bytes,
        free_bytes,
        needed_bytes,
        host_free_bytes,
    } = outcome
    else {
        return None;
    };
    Some(format!(
        "cang: did not grow the workspace image '{}' from {} to {} because it had {} bytes free \
         (under {}% of the image) but the host has only {} free, under the {}x headroom the {} step \
         needs; free some space, or grow it by hand with `cang disk resize --workspace-disk {} {}`",
        path.display(),
        human_bytes(*from_bytes),
        human_bytes(*to_bytes),
        free_bytes,
        workspace::AUTO_GROW_FREE_PERCENT,
        human_bytes(*host_free_bytes),
        workspace::AUTO_GROW_HOST_HEADROOM_FACTOR,
        human_bytes(*needed_bytes),
        path.display(),
        human_bytes(*to_bytes),
    ))
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [
        ("TiB", 1024_u64.pow(4)),
        ("GiB", 1024_u64.pow(3)),
        ("MiB", 1024_u64.pow(2)),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::LogLevel;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn options() -> DiskMaintenanceOptions {
        DiskMaintenanceOptions {
            image: None,
            pull_latest: false,
            guest_init: None,
            mem_gib: None,
            log_settings: crate::logging::LogSettings::resolve(Some(LogLevel::Info), false, None),
        }
    }

    /// An image file of `len` bytes, grown through the injected closure the way the
    /// real one is grown through `grow_existing_at_least_with_runner`.
    fn image(dir: &Path, len: u64) -> std::path::PathBuf {
        let path = dir.join("work.raw");
        std::fs::write(&path, vec![0u8; 4096]).expect("image");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("image");
        file.set_len(len).expect("image size");
        path
    }

    fn grow_to(path: &Path) -> impl FnOnce(u64) -> Result<u64> + '_ {
        move |target: u64| {
            let file = std::fs::File::options()
                .write(true)
                .open(path)
                .expect("image");
            file.set_len(target).expect("grow");
            Ok(target)
        }
    }

    /// The report is read off the guest console, past whatever the kernel printed
    /// before it.
    #[test]
    fn the_report_line_is_found_among_console_output() {
        let console = b"[    0.000000] Linux version\n\n\ncang-workspace-free-bytes\t1234\t5678\n";
        assert_eq!(parse_report(console), Some((1234, 5678)));
        assert_eq!(parse_report(b"nothing here"), None);
        assert_eq!(parse_report(b"cang-workspace-free-bytes\tgarbage\n"), None);
    }

    /// A marker under a tenth of the image grows the image by twice what is in use
    /// plus a spare, and says so on one line.
    #[test]
    fn a_low_marker_grows_the_image_and_prints_one_line() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = image(temp.path(), 8 * GIB);

        let outcome = decide_and_grow(
            8 * GIB,
            Some((512 * 1024 * 1024, 8 * GIB)),
            1024 * GIB,
            grow_to(&path),
        )
        .expect("auto-grow");

        let AutoGrowOutcome::Grew {
            from_bytes,
            to_bytes,
            free_bytes,
        } = outcome
        else {
            panic!("a gibibyte free of eight should grow, got {outcome:?}");
        };
        assert_eq!(from_bytes, 8 * GIB);
        assert!(to_bytes > from_bytes, "the host image grew");
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            to_bytes,
            "the grow closure is what resized the image"
        );
        let line = grew_line(&AutoGrowOutcome::Grew {
            from_bytes,
            to_bytes,
            free_bytes,
        })
        .expect("a grow line");
        assert!(line.contains("grew the workspace image"), "{line}");
        assert!(line.contains(&format!("{free_bytes} bytes free")), "{line}");
    }

    #[test]
    fn a_marker_with_enough_free_leaves_the_image_alone() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = image(temp.path(), 8 * GIB);

        let outcome = decide_and_grow(8 * GIB, Some((2 * GIB, 8 * GIB)), 1024 * GIB, |_| {
            panic!("a healthy image must not be grown")
        })
        .expect("auto-grow");

        assert_eq!(outcome, AutoGrowOutcome::Left);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            8 * GIB,
            "the image is untouched"
        );
        assert_eq!(grew_line(&outcome), None);
        assert_eq!(refused_line(&path, &outcome), None);
    }

    /// A guest that predates the marker reports nothing, and growing blind would be
    /// a guess: the image stays as it is.
    #[test]
    fn no_marker_means_no_growth() {
        let outcome = decide_and_grow(8 * GIB, None, 1024 * GIB, |_| {
            panic!("no marker means no growth")
        })
        .expect("auto-grow");

        assert_eq!(outcome, AutoGrowOutcome::Left);
    }

    #[test]
    fn a_refusal_names_both_numbers_and_the_manual_command() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = image(temp.path(), 8 * GIB);
        // A host with no room for the step refuses instead of growing.
        let outcome = decide_and_grow(8 * GIB, Some((0, 8 * GIB)), 0, |_| {
            panic!("a host without headroom must refuse")
        })
        .expect("auto-grow");

        let AutoGrowOutcome::Refused {
            from_bytes,
            to_bytes,
            needed_bytes,
            host_free_bytes,
            ..
        } = outcome
        else {
            panic!("no host headroom must refuse, got {outcome:?}");
        };
        assert_eq!(from_bytes, 8 * GIB);
        assert!(to_bytes > from_bytes);
        assert!(needed_bytes > host_free_bytes);
        let line = refused_line(&path, &outcome).expect("a refusal line");
        assert!(line.contains("the host has only"), "{line}");
        assert!(
            line.contains(&format!("{} free", human_bytes(host_free_bytes))),
            "{line}"
        );
        assert!(line.contains("cang disk resize --workspace-disk"), "{line}");
    }

    #[test]
    fn a_missing_image_is_refused_before_any_vm_boots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("absent.raw");

        let err = validate_for_report(&path).expect_err("no image");

        assert!(
            format!("{err:#}").contains("no cang workspace disk"),
            "{err:#}"
        );
    }

    #[test]
    fn options_carry_no_image_so_the_default_selection_applies() {
        assert!(options().image.is_none());
    }
}
