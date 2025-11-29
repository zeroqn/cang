use super::bindings::*;
use drm_sys::{DRM_COMMAND_BASE, DRM_IOCTL_BASE};
use rustix::ioctl::{Opcode, Updater, ioctl, opcode};
use std::{io, os::fd::BorrowedFd};

macro_rules! drm_ioctl {
    ($name:ident, $nr:expr, $ty:ty) => {
        pub unsafe fn $name(fd: BorrowedFd, data: &mut $ty) -> io::Result<()> {
            const OPCODE: Opcode =
                opcode::read_write::<$ty>(DRM_IOCTL_BASE, (DRM_COMMAND_BASE + $nr) as u8);
            Ok(unsafe { ioctl(fd, Updater::<OPCODE, $ty>::new(data))? })
        }
    };
}

drm_ioctl!(virtgpu_map, DRM_VIRTGPU_MAP, drm_virtgpu_map);
drm_ioctl!(
    virtgpu_execbuffer,
    DRM_VIRTGPU_EXECBUFFER,
    drm_virtgpu_execbuffer
);
drm_ioctl!(virtgpu_wait, DRM_VIRTGPU_WAIT, drm_virtgpu_3d_wait);
drm_ioctl!(virtgpu_getparam, DRM_VIRTGPU_GETPARAM, drm_virtgpu_getparam);
drm_ioctl!(virtgpu_get_caps, DRM_VIRTGPU_GET_CAPS, drm_virtgpu_get_caps);
drm_ioctl!(
    virtgpu_resource_create,
    DRM_VIRTGPU_RESOURCE_CREATE,
    drm_virtgpu_resource_create
);
drm_ioctl!(
    virtgpu_resource_info,
    DRM_VIRTGPU_RESOURCE_INFO,
    drm_virtgpu_resource_info
);
drm_ioctl!(
    virtgpu_transfer_from_host,
    DRM_VIRTGPU_TRANSFER_FROM_HOST,
    drm_virtgpu_3d_transfer_from_host
);
drm_ioctl!(
    virtgpu_transfer_to_host,
    DRM_VIRTGPU_TRANSFER_TO_HOST,
    drm_virtgpu_3d_transfer_to_host
);
drm_ioctl!(
    virtgpu_resource_create_blob,
    DRM_VIRTGPU_RESOURCE_CREATE_BLOB,
    drm_virtgpu_resource_create_blob
);
drm_ioctl!(
    virtgpu_context_init,
    DRM_VIRTGPU_CONTEXT_INIT,
    drm_virtgpu_context_init
);
