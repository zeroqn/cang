use std::{
    collections::{HashMap, hash_map::Entry},
    io,
    num::NonZero,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        raw::c_void,
    },
    ptr,
};

use drm::{CLOEXEC, RDWR, buffer::Handle, control::RawResourceHandle, node::DrmNode};
use drm_fourcc::DrmFourcc;
use libc::{MAP_SHARED, PROT_READ, PROT_WRITE};
use log::{debug, error, warn};
use smallvec::smallvec;
use wayland_server::backend::protocol::{Argument, Message};

use crate::{
    cross_domain::{CROSS_DOMAIN_ID_TYPE_READ_PIPE, CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB},
    sigbus::SIGBUS_WATCHER,
    source::channel::{ImageDesc, Ring, query_image},
    virtio_gpu::{BlobFlags, BlobMem, VirtioDevice},
    wl_proto::{Data, Object, TryClone},
};

// lets not pull gbm for two values
const LINEAR: u32 = 16;
const SCANOUT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ImageReqs {
    width: u32,
    height: u32,
    drm_format: u32,
}

pub struct ProtocolState {
    image_cache: HashMap<ImageReqs, ImageDesc>,
    pipe_id: u32,

    wl_shm: Option<u32>,
    shm_pools: HashMap<u32, WlShmPool>,
    shm_buffers: HashMap<u32, WlShmBuffer>,
    surfaces: HashMap<u32, WlSurface>,

    destructions_to_skip: HashMap<u32, usize>,
}

pub struct WlShmPool {
    size: usize,

    fd: OwnedFd,
    guest_mmap: *mut c_void,

    res_handle: RawResourceHandle,
    blob_fd: OwnedFd,
    host_mmap: *mut c_void,

    destroyed: bool,
}

impl Drop for WlShmPool {
    fn drop(&mut self) {
        // SAFETY: we do not create WlShmPool with any pointers other than successful mmap results
        unsafe {
            libc::munmap(self.guest_mmap, self.size);
            libc::munmap(self.host_mmap, self.size);
        }
    }
}

pub struct WlShmBuffer {
    pool: u32,
    offset: usize,
    width: usize,
    height: usize,
    stride: usize,
    format: u32,
}

pub struct WlSurface {
    pending_buffer: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct Identifier {
    pub type_: u32,
    pub size: u32,
    pub identifier: NonZero<u32>,
    pub bo: Option<Handle>,
}

impl TryClone for Identifier {
    fn try_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

enum MessageIter {
    None,
    One(std::iter::Once<Message<u32, Identifier>>),
    Many(std::vec::IntoIter<Message<u32, Identifier>>),
}

impl Iterator for MessageIter {
    type Item = Message<u32, Identifier>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::None => None,
            Self::One(iter) => iter.next(),
            Self::Many(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::None => (0, Some(0)),
            Self::One(iter) => iter.size_hint(),
            Self::Many(iter) => iter.size_hint(),
        }
    }
}

impl From<Message<u32, Identifier>> for MessageIter {
    fn from(value: Message<u32, Identifier>) -> Self {
        MessageIter::One(std::iter::once(value))
    }
}

impl From<Vec<Message<u32, Identifier>>> for MessageIter {
    fn from(value: Vec<Message<u32, Identifier>>) -> Self {
        MessageIter::Many(value.into_iter())
    }
}

#[derive(Debug)]
pub enum TransformError {
    Io(io::Error),
    Protocol {
        object_id: u32,
        code: u32,
        message: &'static std::ffi::CStr,
    },
}

impl From<io::Error> for TransformError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(io) => io.fmt(f),
            Self::Protocol { message, .. } => write!(f, "protocol error: {message:?}"),
        }
    }
}

impl std::error::Error for TransformError {}

impl ProtocolState {
    pub fn new() -> Self {
        ProtocolState {
            image_cache: HashMap::new(),
            pipe_id: 0x80000001,
            wl_shm: None,
            shm_pools: HashMap::new(),
            shm_buffers: HashMap::new(),
            surfaces: HashMap::new(),
            destructions_to_skip: HashMap::new(),
        }
    }

    fn handle_shm_create_pool(
        &mut self,
        drm: &impl VirtioDevice,
        query_ring: &Ring,
        mut message: Message<u32, OwnedFd>,
    ) -> io::Result<Message<u32, Identifier>> {
        self.wl_shm = Some(message.sender_id);

        let [
            Argument::NewId(new_pool),
            Argument::Fd(orig_fd),
            Argument::Int(size),
        ] = ({
            let mut args = message.args.drain(0..3);
            [
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
            ]
        })
        else {
            error!("Invalid args for wl_shm::create_pool");
            return Err(io::ErrorKind::InvalidData.into());
        };

        let reqs = ImageReqs {
            width: size as u32,
            height: 1,
            drm_format: DrmFourcc::R8 as u32,
        };

        let desc = match self.image_cache.entry(reqs) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let desc = query_image(
                    drm,
                    query_ring,
                    size as u32,
                    1,
                    DrmFourcc::R8 as u32,
                    SCANOUT | LINEAR,
                )
                .inspect_err(|err| error!("Unable to query host blob parameters: {}", err))?;
                *e.insert(desc)
            }
        };

        let blob = drm
            .resource_create_blob(
                desc.host_size as usize,
                BlobMem::Host3d,
                BlobFlags::USE_MAPPABLE | BlobFlags::USE_SHARABLE,
                Some(desc.blob_id as u64),
            )
            .inspect_err(|err| error!("Unable to create host blob: {}", err))?;

        let identifier = Identifier {
            type_: CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB,
            size: size as u32,
            identifier: blob.res_handle,
            bo: Some(blob.bo_handle),
        };

        let fd = drm
            .buffer_to_prime_fd(blob.bo_handle, CLOEXEC | RDWR)
            .inspect_err(|err| error!("Unable to get fd from bo handle: {}", err))?;

        let guest_mmap = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                orig_fd.as_raw_fd(),
                0,
            )
        };
        if guest_mmap.is_null() {
            error!("Failed to mmap guest shm pool.");
            return Err(io::ErrorKind::InvalidData.into());
        }
        let host_mmap = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if host_mmap.is_null() {
            error!("Failed to mmap host shm pool.");
            return Err(io::ErrorKind::InvalidData.into());
        }

        if let Some(_old) = self.shm_pools.insert(
            new_pool,
            WlShmPool {
                size: size as usize,

                fd: orig_fd,
                guest_mmap,

                res_handle: blob.res_handle,
                blob_fd: fd,
                host_mmap,

                destroyed: false,
            },
        ) {
            warn!("shm_pool id reused while still in usage.");
        }

        Ok(Message {
            sender_id: message.sender_id,
            opcode: message.opcode,
            args: smallvec![
                Argument::NewId(new_pool),
                Argument::Fd(identifier),
                Argument::Int(size)
            ],
        })
    }

    fn handle_shm_resize(
        &mut self,
        drm: &impl VirtioDevice,
        query_ring: &Ring,
        mut message: Message<u32, OwnedFd>,
    ) -> io::Result<Vec<Message<u32, Identifier>>> {
        let pool_id = message.sender_id;
        let Some(pool) = self
            .shm_pools
            .get_mut(&pool_id)
            .filter(|pool| !pool.destroyed)
        else {
            error!("wl_shm_pool::resize on destroyed pool");
            return Err(io::ErrorKind::InvalidData.into());
        };

        let [Argument::Int(size)] = ({
            let mut args = message.args.drain(0..1);
            [args.next().ok_or(io::ErrorKind::InvalidData)?]
        }) else {
            error!("Invalid args for wl_shm_pool::resize");
            return Err(io::ErrorKind::InvalidData.into());
        };

        if (size as usize) < pool.size {
            // not allowed
            error!("Invalid resize, shrinking wl_shm_pool");
            return Err(io::ErrorKind::InvalidData.into());
        }

        if size as usize == pool.size {
            return Ok(vec![message.map_fd(|_| unreachable!())]);
        }

        // so this is somewhat horrible, because we need to not only create a new host_buffer
        // and copy the contents of the old to the new one...
        //
        // ... but we also need to make the host aware of the new identifier, which means
        // sending a `destroy` for the pool, create a pool with the new identifier and then thus
        // recreate all wl_buffers, which also means potentially re-attaching the new buffers.
        //
        // So this is the point, where we actually need to return multiple messages.

        // first lets worry about creating the new buffer and copying.

        let reqs = ImageReqs {
            width: size as u32,
            height: 1,
            drm_format: DrmFourcc::R8 as u32,
        };

        let desc = match self.image_cache.entry(reqs) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let desc = query_image(
                    drm,
                    query_ring,
                    size as u32,
                    1,
                    DrmFourcc::R8 as u32,
                    SCANOUT | LINEAR,
                )
                .inspect_err(|err| error!("Unable to query host blob parameters: {}", err))?;
                *e.insert(desc)
            }
        };

        let blob = drm
            .resource_create_blob(
                desc.host_size as usize,
                BlobMem::Host3d,
                BlobFlags::USE_MAPPABLE | BlobFlags::USE_SHARABLE,
                Some(desc.blob_id as u64),
            )
            .inspect_err(|err| error!("Unable to create host blob: {}", err))?;

        let identifier = Identifier {
            type_: CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB,
            size: size as u32,
            identifier: blob.res_handle,
            bo: Some(blob.bo_handle),
        };

        let fd = drm
            .buffer_to_prime_fd(blob.bo_handle, CLOEXEC | RDWR)
            .inspect_err(|err| error!("Unable to get fd from bo handle: {}", err))?;

        let guest_mmap = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                pool.fd.as_raw_fd(),
                0,
            )
        };
        if guest_mmap.is_null() {
            error!("Failed to mmap guest shm pool.");
            return Err(io::ErrorKind::InvalidData.into());
        }

        let host_mmap = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if host_mmap.is_null() {
            error!("Failed to mmap host shm pool.");
            return Err(io::ErrorKind::InvalidData.into());
        }

        unsafe {
            ptr::copy_nonoverlapping(pool.host_mmap, host_mmap, pool.size);
        }

        // now lets construct all the messages

        let mut messages = Vec::new();

        // wl_shm_pool::destroy
        messages.push(Message {
            sender_id: pool_id,
            opcode: 1,
            args: smallvec![],
        });
        *self.destructions_to_skip.entry(pool_id).or_default() += 1;

        // wl_shm::create_pool
        let Some(global_id) = self.wl_shm.as_ref() else {
            unreachable!()
        };
        messages.push(Message {
            sender_id: *global_id,
            opcode: 0,
            args: smallvec![
                Argument::NewId(pool_id),
                Argument::Fd(identifier),
                Argument::Int(size),
            ],
        });

        for (buffer_id, buffer) in self
            .shm_buffers
            .iter()
            .filter(|(_, buffer)| buffer.pool == pool_id)
        {
            // wl_buffer::destroy
            messages.push(Message {
                sender_id: *buffer_id,
                opcode: 0,
                args: smallvec![],
            });
            // wl_shm_pool::create_buffer
            messages.push(Message {
                sender_id: pool_id,
                opcode: 0,
                args: smallvec![
                    Argument::NewId(*buffer_id),
                    Argument::Int(buffer.offset as i32),
                    Argument::Int(buffer.width as i32),
                    Argument::Int(buffer.height as i32),
                    Argument::Int(buffer.stride as i32),
                    Argument::Uint(buffer.format),
                ],
            });
            for (surface_id, _) in self
                .surfaces
                .iter()
                .filter(|(_, surface)| surface.pending_buffer.is_some_and(|id| id == *buffer_id))
            {
                //wl_surface::attach
                messages.push(Message {
                    sender_id: *surface_id,
                    opcode: 1,
                    args: smallvec![
                        Argument::Object(*buffer_id),
                        Argument::Int(0),
                        Argument::Int(0),
                    ],
                });
            }
        }

        // lastly update our pool data

        unsafe {
            libc::munmap(pool.guest_mmap, pool.size);
            libc::munmap(pool.host_mmap, pool.size);
        }
        pool.res_handle = blob.res_handle;
        pool.blob_fd = fd;
        pool.guest_mmap = guest_mmap;
        pool.host_mmap = host_mmap;
        pool.size = size as usize;

        Ok(messages)
    }

    fn handle_shm_pool_create_buffer(
        &mut self,
        message: Message<u32, OwnedFd>,
    ) -> io::Result<Message<u32, Identifier>> {
        let &[
            Argument::NewId(buffer_id),
            Argument::Int(offset),
            Argument::Int(width),
            Argument::Int(height),
            Argument::Int(stride),
            Argument::Uint(format),
        ] = &*message.args
        else {
            error!("Invalid args for wl_shm_pool::create_buffer");
            return Err(io::ErrorKind::InvalidData.into());
        };

        self.shm_buffers.insert(
            buffer_id,
            WlShmBuffer {
                pool: message.sender_id,
                offset: offset as usize,
                width: width as usize,
                height: height as usize,
                stride: stride as usize,
                format,
            },
        );

        Ok(message.map_fd(|_| unreachable!("wl_shm_pool::create_buffer with fd?")))
    }

    fn handle_dmabuf_params_add(
        &mut self,
        drm: &impl VirtioDevice,
        mut message: Message<u32, OwnedFd>,
    ) -> io::Result<Message<u32, Identifier>> {
        let [
            Argument::Fd(dmabuf),
            Argument::Uint(plane_idx),
            Argument::Uint(offset),
            Argument::Uint(stride),
            Argument::Uint(modifier_high),
            Argument::Uint(modifier_low),
        ] = ({
            let mut args = message.args.drain(0..6);
            [
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
            ]
        })
        else {
            return Err(io::ErrorKind::InvalidData.into());
        };

        match drm
            .prime_fd_to_buffer(dmabuf.as_fd())
            .and_then(|bo| Ok((bo, drm.get_resource_info(bo)?)))
        {
            Ok((bo, info)) => {
                let dmabuf = Identifier {
                    type_: CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB,
                    size: info.size,
                    identifier: info.res_handle,
                    bo: Some(bo),
                };

                Ok(Message {
                    sender_id: message.sender_id,
                    opcode: message.opcode,
                    args: smallvec![
                        Argument::Fd(dmabuf),
                        Argument::Uint(plane_idx),
                        Argument::Uint(offset),
                        Argument::Uint(stride),
                        Argument::Uint(modifier_high),
                        Argument::Uint(modifier_low)
                    ],
                })
            }
            Err(err) => {
                unimplemented!("Not doing non virtio-gpu yet. {}", err);
            }
        }
    }

    fn handle_surface_commit(
        &mut self,
        message: Message<u32, OwnedFd>,
    ) -> Result<Message<u32, Identifier>, TransformError> {
        if let Some(surface) = self.surfaces.get_mut(&message.sender_id) {
            if let Some(buffer_id) = surface.pending_buffer.take() {
                if let Some(shm_buffer) = self.shm_buffers.get(&buffer_id) {
                    if let Some(pool) = self.shm_pools.get(&shm_buffer.pool) {
                        // TODO (optimization): Use (buffer)damage

                        let guard = SIGBUS_WATCHER.watch(pool.guest_mmap, pool.size);

                        unsafe {
                            ptr::copy_nonoverlapping(
                                pool.guest_mmap.byte_add(shm_buffer.offset),
                                pool.host_mmap.byte_add(shm_buffer.offset),
                                shm_buffer.stride * shm_buffer.height,
                            );
                        }

                        if guard.has_faulted() {
                            warn!("faulted when accessing guest shm buffer");
                            return Err(TransformError::Protocol {
                                object_id: buffer_id,
                                code: 2,
                                message: c"error accessing SHM buffer",
                            });
                        }

                        // TODO (optimization): We can send release to the client now and hide the host one later
                    } else {
                        panic!("Commit of shm_buffer without pool?");
                    }
                }
            }
        }

        Ok(message.map_fd(|_| unreachable!("wl_surface::commit with fd?")))
    }

    fn handle_receive(
        &mut self,
        guest_pipes: &mut HashMap<u32, OwnedFd>,
        mut message: Message<u32, OwnedFd>,
    ) -> io::Result<Message<u32, Identifier>> {
        let [Argument::Str(mime), Argument::Fd(pipe)] = ({
            let mut args = message.args.drain(0..2);
            [
                args.next().ok_or(io::ErrorKind::InvalidData)?,
                args.next().ok_or(io::ErrorKind::InvalidData)?,
            ]
        }) else {
            return Err(io::ErrorKind::InvalidData.into());
        };

        let id = self.pipe_id;
        let (next, overflow) = self.pipe_id.overflowing_add(1);
        if overflow {
            self.pipe_id = 0x80000001;
        } else {
            self.pipe_id = next;
        }
        guest_pipes.insert(id, pipe);

        Ok(Message {
            sender_id: message.sender_id,
            opcode: message.opcode,
            args: smallvec![
                Argument::Str(mime),
                Argument::Fd(Identifier {
                    type_: CROSS_DOMAIN_ID_TYPE_READ_PIPE,
                    size: 0,
                    identifier: unsafe { NonZero::new_unchecked(id) },
                    bo: None,
                })
            ],
        })
    }

    pub fn transform_guest_message(
        &mut self,
        drm: &impl VirtioDevice,
        query_ring: &Ring,
        guest_pipes: &mut HashMap<u32, OwnedFd>,
        message: Message<u32, OwnedFd>,
        obj: &Object<Data>,
    ) -> Result<impl Iterator<Item = Message<u32, Identifier>>, TransformError> {
        match (
            obj.interface.name,
            obj.interface.requests[message.opcode as usize].name,
        ) {
            ("wl_shm", "create_pool") => self
                .handle_shm_create_pool(drm, query_ring, message)
                .map(MessageIter::from)
                .map_err(TransformError::from),
            ("wl_shm", "release") => Ok(if self.wl_shm.is_some_and(|id| id == message.sender_id) {
                MessageIter::None
            } else {
                message.map_fd(|_| unreachable!()).into()
            }),
            ("wl_shm_pool", "resize") => self
                .handle_shm_resize(drm, query_ring, message)
                .map(Into::into)
                .map_err(TransformError::from),
            ("wl_shm_pool", "destroy") => {
                if let Some(pool) = self.shm_pools.get_mut(&message.sender_id) {
                    pool.destroyed = true;
                    if self
                        .shm_buffers
                        .values()
                        .all(|buffer| buffer.pool != message.sender_id)
                    {
                        self.shm_pools.remove(&message.sender_id);
                    }
                }

                Ok(message.map_fd(|_| unreachable!()).into())
            }
            ("wl_shm_pool", "create_buffer") => self
                .handle_shm_pool_create_buffer(message)
                .map(Into::into)
                .map_err(TransformError::from),
            ("zwp_linux_buffer_params_v1", "add") => self
                .handle_dmabuf_params_add(drm, message)
                .map(Into::into)
                .map_err(TransformError::from),
            ("wl_buffer", "destroy") => {
                if let Some(buffer) = self.shm_buffers.remove(&message.sender_id) {
                    let id = buffer.pool;
                    if let Some(pool) = self.shm_pools.get(&id) {
                        if pool.destroyed {
                            if self.shm_buffers.values().all(|buffer| buffer.pool != id) {
                                self.shm_pools.remove(&id);
                            }
                        }
                    }
                }

                Ok(message.map_fd(|_| unreachable!()).into())
            }
            ("wl_surface", "attach") => {
                let surface = self
                    .surfaces
                    .entry(message.sender_id)
                    .or_insert_with(|| WlSurface {
                        pending_buffer: None,
                    });
                let &Argument::Object(buffer) = &message.args[0] else {
                    return Err(TransformError::Io(io::ErrorKind::InvalidData.into()));
                };
                surface.pending_buffer = self.shm_buffers.contains_key(&buffer).then_some(buffer);

                Ok(message.map_fd(|_| unreachable!()).into())
            }
            ("wl_surface", "commit") => self.handle_surface_commit(message).map(Into::into),
            ("wl_surface", "destroy") => {
                self.surfaces.remove(&message.sender_id);

                Ok(message.map_fd(|_| unreachable!()).into())
            }
            ("wl_data_offer", "receive")
            | ("ext_data_control_offer_v1", "receive")
            | ("zwp_primary_selection_offer_v1", "receive") => self
                .handle_receive(guest_pipes, message)
                .map(Into::into)
                .map_err(TransformError::from),
            ("wp_image_description_creator_icc_v1", "set_icc_file") => unimplemented!(),
            (interface, name) => Ok(message
                .map_fd(|_| panic!("Unhandled file descriptor: {}.{}", interface, name))
                .into()),
        }
    }

    pub fn filter_host_message(
        &mut self,
        drm: &DrmNode,
        message: Message<u32, OwnedFd>,
        obj: &Object<Data>,
    ) -> Option<Message<u32, OwnedFd>> {
        match (
            obj.interface.name,
            obj.interface.events[message.opcode as usize].name,
        ) {
            // The client is not supposed to reuse IDs without receiving delete_id as a confirmation
            // that it is safe to do so. However if handle_shm_resize is called a few times in a row,
            // what happens is exactly that, so we need to deal with the consequences here.
            ("wl_display", "delete_id") => {
                let &[Argument::Uint(ref id)] = message.args.as_slice() else {
                    unreachable!()
                };
                if let Some(cnt) = self.destructions_to_skip.get_mut(id)
                    && *cnt > 0
                {
                    *cnt -= 1;
                    debug!("Skipping delete_id({id}), skips left: {cnt}");
                    return None;
                }
                Some(message)
            }
            ("wl_buffer", "release") => {
                // TODO (optimization): send early and then hide this message
                Some(message)
            }
            // TODO (potential issue): Adjust dmabuf formats?
            //      - Potentially the host advertises formats, that virt-gpu doesn't support.
            //      - But it is unclear if that is an actual issue, if the client cannot get a buffer of that format anyway.
            //      - If there is no intersection, we possibly want to hide the protocol though,
            //        which is a little too late at this point. :/
            ("zwp_linux_dmabuf_feedback_v1", "main_device")
            | ("zwp_linux_dmabuf_feedback_v1", "tranche_target_device")
            | ("ext_image_copy_capture_session_v1", "dmabuf_device") => {
                let dev = drm.dev_id().to_ne_bytes().to_vec();

                Some(Message {
                    sender_id: message.sender_id,
                    opcode: message.opcode,
                    args: smallvec![Argument::Array(Box::new(dev))],
                })
            }
            ("ext_image_copy_capture_frame_v1", "ready") => {
                // TODO(missing, but not critical): copy from host
                // `ext_image_copy_capture` needs a reverse copy from the host buffer
                // into shm, if the wl_buffer is shm-based. Otherwise the capture doesn't update.
                Some(message)
            }
            (_, _) => Some(message),
        }
    }
}
