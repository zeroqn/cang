use anyhow::{Result, anyhow, bail};
use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;

const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;
const CAP_BPF: u32 = 39;
const CAP_SYS_ADMIN: u32 = 21;
const ALLOWED_CAPABILITIES: [u32; 4] = [CAP_NET_ADMIN, CAP_NET_RAW, CAP_BPF, CAP_SYS_ADMIN];
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const PR_CAP_AMBIENT: libc::c_int = 47;
const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;

#[repr(C)]
struct CapUserHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapUserData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    let command: Vec<OsString> = args.into_iter().collect();
    if command.is_empty() {
        bail!("usage: cang-granted COMMAND [ARG ...]");
    }

    let permitted = current_permitted_capabilities()?;
    if permitted == [0, 0] {
        bail!("this cang task has no capability-bearing --new-perms grants");
    }
    reject_unexpected_capabilities(permitted)?;
    set_inheritable_and_ambient(permitted)?;
    exec(&command)
}

fn current_permitted_capabilities() -> Result<[u32; 2]> {
    let mut header = CapUserHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapUserData::default(); 2];
    if unsafe { libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok([data[0].permitted, data[1].permitted])
}

fn reject_unexpected_capabilities(permitted: [u32; 2]) -> Result<()> {
    let allowed = capability_mask(&ALLOWED_CAPABILITIES);
    let unexpected = [permitted[0] & !allowed[0], permitted[1] & !allowed[1]];
    if unexpected != [0, 0] {
        bail!(
            "refusing unexpected permitted capabilities {:08x}{:08x}",
            unexpected[1],
            unexpected[0]
        );
    }
    Ok(())
}

fn set_inheritable_and_ambient(permitted: [u32; 2]) -> Result<()> {
    let mut header = CapUserHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [
        CapUserData {
            effective: permitted[0],
            permitted: permitted[0],
            inheritable: permitted[0],
        },
        CapUserData {
            effective: permitted[1],
            permitted: permitted[1],
            inheritable: permitted[1],
        },
    ];
    if unsafe { libc::syscall(libc::SYS_capset, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    for capability in ALLOWED_CAPABILITIES {
        if permitted[(capability / 32) as usize] & (1 << (capability % 32)) == 0 {
            continue;
        }
        if unsafe {
            libc::prctl(
                PR_CAP_AMBIENT,
                PR_CAP_AMBIENT_RAISE,
                libc::c_ulong::from(capability),
                0,
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn capability_mask(capabilities: &[u32]) -> [u32; 2] {
    let mut mask = [0, 0];
    for capability in capabilities {
        mask[(*capability / 32) as usize] |= 1 << (*capability % 32);
    }
    mask
}

fn exec(command: &[OsString]) -> Result<()> {
    let command = command
        .iter()
        .map(|arg| {
            CString::new(arg.as_os_str().as_bytes())
                .map_err(|_| anyhow!("command contains a NUL byte"))
        })
        .collect::<Result<Vec<_>>>()?;
    let argv = command
        .iter()
        .map(|arg| arg.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect::<Vec<_>>();
    unsafe { libc::execvp(command[0].as_ptr(), argv.as_ptr()) };
    Err(std::io::Error::last_os_error().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_binaries_build_mimalloc_without_allocation_profiling() {
        // The bootstrap and `cang-granted` are the musl-static binaries the
        // image runs; they share the workspace allocator features (`no_profile`,
        // `secure`), so a guest never pays sampling on the allocation fast path.
        assert!(
            !rustfs_mimalloc::MiMalloc::profiling_enabled(),
            "cang-guest-init builds mimalloc with no_profile"
        );
    }

    #[test]
    fn allowlist_accepts_supported_capabilities() {
        assert!(reject_unexpected_capabilities(capability_mask(&ALLOWED_CAPABILITIES)).is_ok());
    }

    #[test]
    fn allowlist_rejects_other_capabilities() {
        let err = reject_unexpected_capabilities(capability_mask(&[22]))
            .expect_err("CAP_SYS_BOOT must be rejected");
        assert!(
            err.to_string()
                .contains("unexpected permitted capabilities")
        );
    }
}
