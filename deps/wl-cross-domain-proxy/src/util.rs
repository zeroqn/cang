use std::{
    io,
    os::{fd::AsRawFd, raw::c_void},
    ptr,
};

use libc::MAP_SHARED;

pub struct MmapGuard {
    ptr: *mut c_void,
    len: usize,
}

impl MmapGuard {
    pub fn map_fd(fd: &impl AsRawFd, len: usize, prot: i32) -> io::Result<Self> {
        let ptr = unsafe { libc::mmap(ptr::null_mut(), len, prot, MAP_SHARED, fd.as_raw_fd(), 0) };
        if ptr.is_null() {
            Err(io::ErrorKind::InvalidData.into())
        } else {
            Ok(MmapGuard { ptr, len })
        }
    }

    pub fn ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for MmapGuard {
    fn drop(&mut self) {
        // SAFETY: we only construct this with successful mmap results
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}
