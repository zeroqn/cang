use anyhow::{Context, Result, bail};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use crate::guest_init::fs;

const TUN_MAJOR: u64 = 10;
const TUN_MINOR: u64 = 200;
const TUN_MODE: u32 = 0o666;
const TUNSETIFF: libc::Ioctl = 0x400454ca as libc::Ioctl;
const IFF_TUN: i16 = 0x0001;
const IFF_NO_PI: i16 = 0x1000;
const IFREQ_SIZE: usize = 40;

pub(in crate::guest_init) fn prepare() -> Result<()> {
    enable_user_namespaces()?;
    prepare_tun_device()?;
    prepare_kvm_device()
}

/// Raise the kernel knobs rootless user namespaces need.
///
/// The linux-hardened guest kernel boots with
/// `kernel.unprivileged_userns_clone=0`, so an unprivileged
/// `clone(CLONE_NEWUSER)` fails with `EPERM` until this runs. Anything the task
/// starts that creates its own user namespace - `bwrap`, which is what the
/// guest's `rustc`/`rust-analyzer` compatibility wrappers exec, and rootless
/// Podman - needs it raised *before* the task command runs, so `enter()` calls
/// this synchronously; the rootless Podman prep worker calls it too, and
/// [`maybe_raise_sysctl`] makes the second call a no-op.
pub(in crate::guest_init) fn enable_user_namespaces() -> Result<()> {
    maybe_raise_sysctl(Path::new("/proc/sys/user/max_user_namespaces"), 28_633)?;
    if Path::new("/proc/sys/kernel/unprivileged_userns_clone").exists() {
        maybe_raise_sysctl(Path::new("/proc/sys/kernel/unprivileged_userns_clone"), 1)?;
    }
    Ok(())
}

fn maybe_raise_sysctl(path: &Path, target: u32) -> Result<()> {
    if !path.exists() {
        bail!(
            "kernel does not expose {}; rootless container runtimes need user namespace support",
            path.display()
        );
    }
    let current = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if current < target {
        std::fs::write(path, format!("{target}\n")).with_context(|| {
            format!(
                "failed to set {}={target} for rootless container runtimes",
                path.display()
            )
        })?;
    }
    Ok(())
}

pub(in crate::guest_init) fn prepare_tun_device() -> Result<()> {
    prepare_tun_device_at(Path::new("/dev/net/tun"), verify_tun_ioctl)
}

fn prepare_tun_device_at(tun: &Path, verify: fn(&Path) -> Result<()>) -> Result<()> {
    if !tun.exists() {
        bail!(
            "rootless container runtime TUN device is missing at {}; ensure host /dev/net/tun is passed into the internal guest",
            tun.display()
        );
    }
    validate_tun_metadata(tun)?;
    fs::chmod(tun, TUN_MODE)
        .context("failed to make /dev/net/tun accessible to rootless container runtimes")?;
    verify(tun).with_context(|| {
        format!(
            "failed to verify {} with TUNSETIFF for rootless container runtimes",
            tun.display()
        )
    })
}

fn validate_tun_metadata(tun: &Path) -> Result<()> {
    let metadata = std::fs::metadata(tun)
        .with_context(|| format!("failed to stat rootless TUN device {}", tun.display()))?;
    if !metadata.file_type().is_char_device() {
        bail!("{} exists but is not a character device", tun.display());
    }
    let rdev = metadata.rdev();
    let major = linux_major(rdev);
    let minor = linux_minor(rdev);
    if major != TUN_MAJOR || minor != TUN_MINOR {
        bail!(
            "{} is character device {major}:{minor}, expected {TUN_MAJOR}:{TUN_MINOR}",
            tun.display()
        );
    }
    Ok(())
}

fn verify_tun_ioctl(tun: &Path) -> Result<()> {
    let file = File::options()
        .read(true)
        .write(true)
        .open(tun)
        .with_context(|| format!("failed to open {}", tun.display()))?;
    let mut ifreq = [0_u8; IFREQ_SIZE];
    let name = b"cang%d\0";
    ifreq[..name.len()].copy_from_slice(name);
    ifreq[libc::IFNAMSIZ..libc::IFNAMSIZ + 2].copy_from_slice(&(IFF_TUN | IFF_NO_PI).to_ne_bytes());
    // SAFETY: ioctl receives a valid fd for /dev/net/tun and a writable ifreq-shaped buffer.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), TUNSETIFF, ifreq.as_mut_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("TUNSETIFF ioctl failed")
    }
}

fn linux_major(dev: u64) -> u64 {
    ((dev >> 8) & 0x0fff) | ((dev >> 32) & 0xffff_f000)
}

fn linux_minor(dev: u64) -> u64 {
    (dev & 0x00ff) | ((dev >> 12) & 0xffff_ff00)
}

fn prepare_kvm_device() -> Result<()> {
    prepare_kvm_device_at(Path::new("/dev/kvm"))
}

fn prepare_kvm_device_at(kvm: &Path) -> Result<()> {
    if !kvm.exists() {
        return Ok(());
    }
    fs::chmod(kvm, 0o666).with_context(|| {
        format!(
            "failed to make {} accessible to non-root nested KVM tasks",
            kvm.display()
        )
    })
}

#[cfg(test)]
#[path = "kernel_tests.rs"]
mod tests;
