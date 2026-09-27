use rustix::ioctl::{Ioctl, IoctlOutput, Opcode, ioctl, opcode};
use std::{
    fs::OpenOptions,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        raw::c_void,
    },
};

const UDMABUF_IOCTL_BASE: u8 = 0x75; // 'u'

#[repr(C)]
#[derive(Default)]
struct UdmabufCreate {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

unsafe impl Ioctl for UdmabufCreate {
    type Output = OwnedFd;

    const IS_MUTATING: bool = false;

    fn opcode(&self) -> Opcode {
        opcode::write::<UdmabufCreate>(UDMABUF_IOCTL_BASE, 0x42)
    }

    fn as_ptr(&mut self) -> *mut c_void {
        std::ptr::addr_of_mut!(*self).cast::<c_void>()
    }

    unsafe fn output_from_ptr(
        num: IoctlOutput,
        _: *mut c_void,
    ) -> rustix::io::Result<Self::Output> {
        // A negative return would've been caught by rustix already
        Ok(unsafe { OwnedFd::from_raw_fd(num) })
    }
}

pub struct Udmabuf {
    fd: OwnedFd,
}

impl Udmabuf {
    pub fn open() -> io::Result<Self> {
        const UDMABUF_PATH: &str = "/dev/udmabuf";
        let fd = OpenOptions::new().read(true).open(UDMABUF_PATH)?.into();
        Ok(Udmabuf { fd })
    }

    pub fn create(&self, memfd: &impl AsRawFd, offset: u64, size: u64) -> io::Result<OwnedFd> {
        let req = UdmabufCreate {
            memfd: memfd.as_raw_fd() as u32,
            flags: 0,
            offset,
            size,
        };
        Ok(unsafe { ioctl(&self.fd, req)? })
    }
}
