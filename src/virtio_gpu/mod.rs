use std::{
    io::{self, Result},
    num::NonZeroU32,
    os::fd::AsFd,
};

use bitflags::bitflags;
use drm::{buffer::Handle as BufferHandle, control::RawResourceHandle};
use zerocopy::{Immutable, IntoBytes};

use crate::{
    cross_domain::CrossDomainCapabilities,
    source::channel::Ring,
    virtio_gpu::bindings::{
        VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE, VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        VIRTGPU_BLOB_FLAG_USE_SHAREABLE, VIRTGPU_BLOB_MEM_GUEST, VIRTGPU_BLOB_MEM_HOST3D,
        VIRTGPU_BLOB_MEM_HOST3D_GUEST, VIRTGPU_DRM_CAPSET_CROSS_DOMAIN, VIRTGPU_PARAM_3D_FEATURES,
        VIRTGPU_PARAM_BLOB_ALIGNMENT, VIRTGPU_PARAM_CAPSET_QUERY_FIX, VIRTGPU_PARAM_CONTEXT_INIT,
        VIRTGPU_PARAM_CROSS_DEVICE, VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME, VIRTGPU_PARAM_HOST_VISIBLE,
        VIRTGPU_PARAM_RESOURCE_BLOB, VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs, drm_virtgpu_context_init,
        drm_virtgpu_context_set_param, drm_virtgpu_map,
    },
};

pub mod bindings;
pub mod ioctl;

pub struct ResourceInfo {
    pub res_handle: RawResourceHandle,
    pub size: u32,
    pub blob_mem: u32,
}

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Param {
    /// "do we have 3D features in the hw" (NOTE: not *actually* 3D specific)
    _3dFeatures = VIRTGPU_PARAM_3D_FEATURES as _,
    /// do we have the capset fix
    CapsetQueryFix = VIRTGPU_PARAM_CAPSET_QUERY_FIX as _,
    /// DRM_VIRTGPU_RESOURCE_CREATE_BLOB
    ResourceBlob = VIRTGPU_PARAM_RESOURCE_BLOB as _,
    /// Host blob resources are mappable
    HostVisible = VIRTGPU_PARAM_HOST_VISIBLE as _,
    /// Cross virtio-device resource sharing (UUID)
    CrossDevice = VIRTGPU_PARAM_CROSS_DEVICE as _,
    /// DRM_VIRTGPU_CONTEXT_INIT
    ContextInit = VIRTGPU_PARAM_CONTEXT_INIT as _,
    /// Bitmask of supported capability set ids
    SupportedCapsetIds = VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs as _,
    /// Ability to set debug name from userspace
    ExplicitDebugName = VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME as _,
    /// Device alignment requirements for blobs
    BlobAlignment = VIRTGPU_PARAM_BLOB_ALIGNMENT as _,
}

pub unsafe trait Capset: Default + Sized {
    const ID: u32;
    const VERSION: u32 = 0;
}

unsafe impl Capset for CrossDomainCapabilities {
    const ID: u32 = VIRTGPU_DRM_CAPSET_CROSS_DOMAIN;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BlobMem {
    Guest = VIRTGPU_BLOB_MEM_GUEST,
    Host3d = VIRTGPU_BLOB_MEM_HOST3D,
    Host3dGuest = VIRTGPU_BLOB_MEM_HOST3D_GUEST,
}

bitflags! {
    pub struct BlobFlags: u32 {
        const USE_MAPPABLE = VIRTGPU_BLOB_FLAG_USE_MAPPABLE;
        const USE_SHARABLE = VIRTGPU_BLOB_FLAG_USE_SHAREABLE;
        const USE_CROSS_DEVICE = VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE;
    }
}

pub struct Blob {
    pub bo_handle: drm::buffer::Handle,
    pub res_handle: RawResourceHandle,
}

pub trait VirtioDevice: drm::control::Device {
    fn ensure_feature(&self, param: Param) -> Result<()> {
        if self.get_param(param)? == 0 {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        } else {
            Ok(())
        }
    }

    fn get_param(&self, param: Param) -> Result<u64> {
        let mut value = 0u64;

        let mut info = bindings::drm_virtgpu_getparam {
            param: param as u64,
            value: &mut value as *mut _ as u64,
        };

        unsafe {
            ioctl::virtgpu_getparam(self.as_fd(), &mut info)?;
        }

        Ok(value)
    }

    fn get_capset<T: Capset>(&self) -> Result<T> {
        let mut value = T::default();
        let mut info = bindings::drm_virtgpu_get_caps {
            cap_set_id: T::ID,
            cap_set_ver: T::VERSION,
            addr: &mut value as *mut _ as u64,
            size: std::mem::size_of::<T>() as u32,
            pad: 0,
        };

        unsafe {
            ioctl::virtgpu_get_caps(self.as_fd(), &mut info)?;
        }

        Ok(value)
    }

    fn context_init(&self, params: &[drm_virtgpu_context_set_param]) -> Result<()> {
        let mut info = drm_virtgpu_context_init {
            num_params: params.len() as u32,
            pad: 0,
            ctx_set_params: params.as_ptr() as u64,
        };

        unsafe {
            ioctl::virtgpu_context_init(self.as_fd(), &mut info)?;
        }

        Ok(())
    }

    fn resource_create_blob(
        &self,
        size: usize,
        mem: BlobMem,
        flags: BlobFlags,
        blob_id: Option<u64>,
    ) -> Result<Blob> {
        let mut info = bindings::drm_virtgpu_resource_create_blob {
            blob_mem: mem as u32,
            blob_flags: flags.bits(),
            bo_handle: 0,
            res_handle: 0,
            size: size as u64,
            pad: 0,
            cmd_size: 0,
            cmd: 0,
            blob_id: blob_id.unwrap_or(0),
        };

        unsafe {
            ioctl::virtgpu_resource_create_blob(self.as_fd(), &mut info)?;
        }

        Ok(unsafe {
            Blob {
                bo_handle: RawResourceHandle::new_unchecked(info.bo_handle).into(),
                res_handle: RawResourceHandle::new_unchecked(info.res_handle),
            }
        })
    }

    fn map_offset_for_blob(&self, gem: BufferHandle) -> Result<u64> {
        let mut info = drm_virtgpu_map {
            offset: 0,
            handle: gem.into(),
            pad: 0,
        };

        unsafe {
            ioctl::virtgpu_map(self.as_fd(), &mut info)?;
        }

        Ok(info.offset)
    }

    fn execbuffer<T: IntoBytes + Immutable + ?Sized>(
        &self,
        cmd: &T,
        ring: Option<&Ring>,
    ) -> Result<()> {
        let cmd = cmd.as_bytes();
        let mut info = bindings::drm_virtgpu_execbuffer {
            flags: if ring.is_some() {
                bindings::VIRTGPU_EXECBUF_RING_IDX
            } else {
                0
            },
            size: cmd.len() as u32,
            command: cmd.as_ptr() as u64,
            bo_handles: if let Some(ring) = ring {
                &ring.bo_handle as *const _ as u64
            } else {
                0
            },
            num_bo_handles: if ring.is_some() { 1 } else { 0 },
            fence_fd: 0,
            ring_idx: if let Some(ring) = ring { ring.idx } else { 0 },
            syncobj_stride: 0,
            num_in_syncobjs: 0,
            num_out_syncobjs: 0,
            in_syncobjs: 0,
            out_syncobjs: 0,
        };

        unsafe {
            ioctl::virtgpu_execbuffer(self.as_fd(), &mut info)?;
        }

        Ok(())
    }

    fn wait(&self, gem: BufferHandle) -> Result<()> {
        let mut ret = Err(io::Error::from_raw_os_error(libc::EAGAIN));
        let mut info = bindings::drm_virtgpu_3d_wait {
            handle: gem.into(),
            flags: 0,
        };
        while ret.as_ref().is_err_and(|err| {
            err.raw_os_error()
                .is_some_and(|errno| errno == libc::EAGAIN)
        }) {
            unsafe {
                ret = ioctl::virtgpu_wait(self.as_fd(), &mut info);
            }
        }

        ret
    }

    fn get_resource_info(&self, gem: BufferHandle) -> Result<ResourceInfo> {
        let mut info = bindings::drm_virtgpu_resource_info {
            bo_handle: gem.into(),
            res_handle: 0,
            size: 0,
            blob_mem: 0,
        };

        unsafe {
            ioctl::virtgpu_resource_info(self.as_fd(), &mut info)?;
        }

        Ok(ResourceInfo {
            res_handle: NonZeroU32::new(info.res_handle)
                .expect("ioctl returned successful but returned no resource handle"),
            size: info.size,
            blob_mem: info.blob_mem,
        })
    }
}
