use std::{
    collections::{HashMap, VecDeque},
    fs::OpenOptions,
    io::{self, ErrorKind},
    mem,
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    ptr,
    rc::Rc,
    slice,
    sync::OnceLock,
};

use anyhow::Context;
use calloop::{EventSource, Interest, Mode, PostAction, generic::Generic};
use drm::{
    buffer::Handle as BufferHandle,
    control::{Device as ControlDevice, RawResourceHandle},
    node::DrmNode,
};
use drm_sys::drm_event;
use log::{debug, error, warn};
use smallvec::SmallVec;
use wayland_server::backend::protocol::{Argument, Message};
use zerocopy::{FromBytes, IntoBytes};

use crate::{
    DrmDevice,
    cross_domain::{
        CROSS_DOMAIN_CHANNEL_RING, CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND,
        CROSS_DOMAIN_CMD_GET_IMAGE_REQUIREMENTS, CROSS_DOMAIN_CMD_INIT, CROSS_DOMAIN_CMD_POLL,
        CROSS_DOMAIN_CMD_READ, CROSS_DOMAIN_CMD_RECEIVE, CROSS_DOMAIN_CMD_SEND,
        CROSS_DOMAIN_CMD_WRITE, CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB, CROSS_DOMAIN_ID_TYPE_WRITE_PIPE,
        CROSS_DOMAIN_MAX_IDENTIFIERS, CROSS_DOMAIN_QUERY_RING, CROSS_DOMAIN_RING_NONE,
        CrossDomainCapabilities, CrossDomainGetImageRequirements, CrossDomainHeader,
        CrossDomainInitV0, CrossDomainPoll, CrossDomainReadWrite, CrossDomainSendReceive,
    },
    source::channel::wayland::ProtocolState,
    virtio_gpu::{BlobFlags, BlobMem, Param, VirtioDevice, bindings as ffi},
    wl_proto::{
        Buffer, ClientConnection, Data, MAX_BYTES_OUT, MAX_FDS_OUT, MessageParseError, Object,
        ObjectMap, parse_message, write_to_buffers,
    },
};

mod wayland;

#[allow(unused_variables, dead_code)]
pub struct ClientChannel {
    local: ClientConnection,
    protocol_state: ProtocolState,

    node: DrmNode,
    drm: Generic<DrmDevice, anyhow::Error>,
    remote_data: Buffer<u8>,
    remote_fds: VecDeque<OwnedFd>,

    params: HashMap<Param, u64>,
    query_ring: Ring,
    channel_ring: Ring,
    pipe_id: u32,
    pipes_write_from_host: HashMap<u32, OwnedFd>,
    pipes_read_from_guest: HashMap<u32, (Generic<OwnedFd, anyhow::Error>, bool)>,
}

const REQUIRED_PARAMS: [Param; 6] = [
    Param::_3dFeatures,
    Param::CapsetQueryFix,
    Param::ResourceBlob,
    Param::HostVisible,
    Param::ContextInit,
    Param::SupportedCapsetIds,
];

static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

impl ClientChannel {
    pub fn new(local: UnixStream, node: DrmNode, mut filters: Vec<String>) -> anyhow::Result<Self> {
        let drm = DrmDevice(Rc::new(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(
                    node.dev_path()
                        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?,
                )
                .context("Failed to open virtio-gpu device")?,
        ));

        let params = REQUIRED_PARAMS
            .iter()
            .map(|param| Ok((*param, drm.get_param(*param)?)))
            .collect::<io::Result<HashMap<Param, u64>>>()?;
        if params[&Param::SupportedCapsetIds] & (1 << ffi::VIRTGPU_DRM_CAPSET_CROSS_DOMAIN) == 0 {
            return Err(io::Error::from(ErrorKind::Unsupported))
                .context("Host does not support cross domain protocol");
        }

        let caps = drm.get_capset::<CrossDomainCapabilities>()?;

        // do *NOT* add a check for caps.supports_dmabuf here!
        // supports_dmabuf refers to dmabuf allocation *via* the cross-domain capability.
        // 3D Wayland clients allocate via the virglrenderer one, so that is *not* required.

        // we never expose the old mesa protocol and just use dmabufs.
        filters.push("wl_drm".to_owned());
        // we currently don't know how to handle syncobjs.
        filters.push("wp_linux_drm_syncobj_manager_v1".to_owned());
        // this would be interesting, but needs cooperation from the hypervisor.
        // we'd probably be better off to completely implement this protocol in the guest.
        filters.push("wp_drm_lease_device_v1".to_owned());

        if (caps.supported_channels & (1 << CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND)) == 0 {
            return Err(io::Error::from(ErrorKind::Unsupported))
                .context("Host does not support wayland channel type");
        }

        drm.context_init(&[
            ffi::drm_virtgpu_context_set_param {
                param: ffi::VIRTGPU_CONTEXT_PARAM_CAPSET_ID as u64,
                value: ffi::VIRTGPU_DRM_CAPSET_CROSS_DOMAIN as u64,
            },
            ffi::drm_virtgpu_context_set_param {
                param: ffi::VIRTGPU_CONTEXT_PARAM_NUM_RINGS as u64,
                value: 2,
            },
            ffi::drm_virtgpu_context_set_param {
                param: ffi::VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK as u64,
                value: 1 << CROSS_DOMAIN_CHANNEL_RING,
            },
        ])
        .context("Context init failed")?;

        let query_ring = Ring::new(&drm, CROSS_DOMAIN_QUERY_RING).context("Query ring failed")?;
        let channel_ring =
            Ring::new(&drm, CROSS_DOMAIN_CHANNEL_RING).context("Channel ring failed")?;

        let init = CrossDomainInitV0 {
            hdr: CrossDomainHeader {
                cmd: CROSS_DOMAIN_CMD_INIT,
                ring_idx: CROSS_DOMAIN_RING_NONE as u8,
                cmd_size: mem::size_of::<CrossDomainInitV0>() as u16,
                pad: 0,
            },
            query_ring_id: query_ring.res_id.into(),
            channel_ring_id: channel_ring.res_id.into(),
            channel_type: CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND,
        };
        drm.execbuffer(&init, None).context("Init CMD failed")?;

        poll(&drm, &channel_ring).context("Poll CMD failed")?;

        let drm = Generic::new_with_error(drm, Interest::READ, Mode::Edge);
        let local = ClientConnection::new(local, filters);
        Ok(ClientChannel {
            local,
            protocol_state: ProtocolState::new(),

            node,
            drm,
            remote_data: Buffer::new(MAX_BYTES_OUT * 2),
            remote_fds: VecDeque::new(),

            params,
            query_ring,
            channel_ring,
            pipes_read_from_guest: HashMap::new(),
            pipes_write_from_host: HashMap::new(),
            pipe_id: 0x80000000,
        })
    }
}

impl Drop for ClientChannel {
    fn drop(&mut self) {
        self.channel_ring.destroy(self.drm.get_ref());
        self.query_ring.destroy(self.drm.get_ref());
    }
}

#[allow(unused_variables, dead_code)]
pub struct Ring {
    pub idx: u32,
    pub bo_handle: BufferHandle,
    pub res_id: RawResourceHandle,
    pub map: *mut libc::c_void,
}

impl Ring {
    fn new(drm: &impl VirtioDevice, idx: u32) -> anyhow::Result<Ring> {
        let page_size =
            *PAGE_SIZE.get_or_init(|| unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize);

        let blob = drm
            .resource_create_blob(page_size, BlobMem::Guest, BlobFlags::USE_MAPPABLE, None)
            .context("Unable to create ring blob")?;
        let offset = drm
            .map_offset_for_blob(blob.bo_handle)
            .context("Failed to find map offset for ring")?;

        let res = unsafe {
            libc::mmap(
                ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                drm.as_fd().as_raw_fd(),
                offset as i64,
            )
        };

        if res == libc::MAP_FAILED {
            return Err(io::Error::last_os_error()).context("Failed to mmap ring buffer");
        }

        Ok(Ring {
            idx,
            bo_handle: blob.bo_handle,
            res_id: blob.res_handle,
            map: res,
        })
    }

    fn destroy(&mut self, drm: &impl ControlDevice) {
        unsafe {
            libc::munmap(self.map, *PAGE_SIZE.get().unwrap());
        }
        let _ = drm.close_buffer(self.bo_handle);
    }
}

#[derive(Debug, Clone, Copy, Hash)]
struct ImageDesc {
    pub strides: [u32; 4],
    pub offsets: [u32; 4],
    pub host_size: u64,
    pub blob_id: u32,
}

fn query_image(
    drm: &impl VirtioDevice,
    ring: &Ring,
    width: u32,
    height: u32,
    drm_format: u32,
    flags: u32,
) -> io::Result<ImageDesc> {
    let cmd = CrossDomainGetImageRequirements {
        hdr: CrossDomainHeader {
            cmd: CROSS_DOMAIN_CMD_GET_IMAGE_REQUIREMENTS,
            cmd_size: mem::size_of::<CrossDomainGetImageRequirements>() as u16,
            ring_idx: ring.idx as u8,
            pad: 0,
        },
        width,
        height,
        drm_format,
        flags,
    };

    drm.execbuffer(&cmd, Some(ring))?;
    drm.wait(ring.bo_handle)?;

    let addr = ring.map as *const u32;

    let desc = ImageDesc {
        strides: std::array::from_fn(|i| unsafe { *addr.offset(i as isize) }),
        offsets: std::array::from_fn(|i| unsafe { *addr.offset((4 + i) as isize) }),
        host_size: unsafe { *(addr.offset(10) as *const u64) },
        blob_id: unsafe { *addr.offset(12) },
    };

    Ok(desc)
}

fn poll(drm: &impl VirtioDevice, ring: &Ring) -> io::Result<()> {
    let cmd = CrossDomainPoll {
        hdr: CrossDomainHeader {
            cmd: CROSS_DOMAIN_CMD_POLL,
            cmd_size: mem::size_of::<CrossDomainPoll>() as u16,
            ring_idx: ring.idx as u8,
            pad: 0,
        },
        pad: 0,
    };

    drm.execbuffer(&cmd, Some(ring))
}

impl EventSource for ClientChannel {
    type Event = ();
    type Metadata = ();
    type Ret = ();
    type Error = anyhow::Error;

    fn process_events<F>(
        &mut self,
        readiness: calloop::Readiness,
        token: calloop::Token,
        _callback: F,
    ) -> Result<calloop::PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut res = PostAction::Continue;

        let drm = unsafe { self.drm.get_mut() };
        let mut err_msg: Option<Message<u32, OwnedFd>> = None;
        let local_result = self.local.process_events(readiness, token, |msg, obj| {
            log::debug!("Wayland socket event");

            // SAFETY: we don't drop the stream
            let mut buffer = [0; MAX_BYTES_OUT + mem::size_of::<CrossDomainSendReceive>()];
            let mut identifiers = Vec::with_capacity(MAX_FDS_OUT);

            for msg in self
                .protocol_state
                .transform_guest_message(
                    drm,
                    &self.query_ring,
                    &mut self.pipes_write_from_host,
                    msg,
                    obj,
                )
                .inspect_err(|err| match err {
                    wayland::TransformError::Io(err) => {
                        warn!("Failed to transform message: {}", err)
                    }
                    wayland::TransformError::Protocol {
                        object_id,
                        code,
                        message,
                    } => {
                        err_msg = Some(Message {
                            sender_id: 1,
                            opcode: 0,
                            args: smallvec::smallvec![
                                Argument::Object(*object_id),
                                Argument::Uint(*code),
                                Argument::Str(Some(Box::new((*message).into()))),
                            ],
                        })
                    }
                })?
            {
                let read_bytes = write_to_buffers(
                    &msg,
                    &mut buffer[mem::size_of::<CrossDomainSendReceive>()..],
                    &mut identifiers,
                )
                .context("Failed to write out message")?;

                let total_size = mem::size_of::<CrossDomainSendReceive>() + read_bytes;

                let mut cmd = CrossDomainSendReceive {
                    hdr: CrossDomainHeader {
                        cmd: CROSS_DOMAIN_CMD_SEND,
                        ring_idx: CROSS_DOMAIN_RING_NONE as u8,
                        cmd_size: total_size as u16,
                        pad: 0,
                    },
                    opaque_data_size: read_bytes as u32,
                    num_identifiers: identifiers.len() as u32,
                    identifiers: [0; CROSS_DOMAIN_MAX_IDENTIFIERS],
                    identifier_sizes: [0; CROSS_DOMAIN_MAX_IDENTIFIERS],
                    identifier_types: [0; CROSS_DOMAIN_MAX_IDENTIFIERS],
                };

                let mut bos = SmallVec::<[BufferHandle; CROSS_DOMAIN_MAX_IDENTIFIERS]>::new();
                for (i, ident) in identifiers.drain(..).enumerate() {
                    cmd.identifier_types[i] = ident.type_;
                    cmd.identifier_sizes[i] = ident.size as u32;
                    cmd.identifiers[i] = ident.identifier.into();
                    if let Some(bo) = ident.bo {
                        bos.push(bo);
                    }
                }

                cmd.write_to(&mut buffer[0..mem::size_of::<CrossDomainSendReceive>()])
                    .unwrap();
                drm.execbuffer(&buffer[0..total_size], None)
                    .context("send-receive execbuffer failed")?;
                for bo in bos {
                    drm.close_buffer(bo)
                        .context("Failed to close buffer handle")?;
                }
            }

            Ok(PostAction::Continue)
        })?;
        if let Some(msg) = err_msg.take() {
            self.local.write_message(&msg)?;
            self.local.flush()?;
            return Ok(PostAction::Remove);
        }
        match local_result {
            PostAction::Reregister if res == PostAction::Continue => {
                res = PostAction::Reregister;
            }
            PostAction::Disable if res != PostAction::Remove => {
                res = PostAction::Disable;
            }
            PostAction::Remove => {
                res = PostAction::Remove;
            }
            _ => {}
        }

        let _ = drm;
        match self.drm.process_events(readiness, token, |_, drm| {
            let drm = unsafe { drm.get_mut() };
            let mut event = drm_event {
                type_: 0,
                length: 0,
            };
            let mut res = PostAction::Continue;

            let len = unsafe {
                libc::read(
                    drm.as_fd().as_raw_fd(),
                    &mut event as *mut drm_event as *mut _,
                    mem::size_of::<drm_event>(),
                )
            };
            if len < 0 {
                return Err(io::Error::from_raw_os_error(-len as i32))
                    .context("drm device read failed");
            }
            if len == 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe)).context("drm device eof");
            }
            assert!(len as usize == mem::size_of::<drm_event>());
            log::debug!("Virtiogpu drm event");

            if event.type_ != ffi::VIRTGPU_EVENT_FENCE_SIGNALED {
                return Ok(PostAction::Continue);
            }

            let ring_slice = unsafe {
                slice::from_raw_parts(
                    self.channel_ring.map as *const u8,
                    *PAGE_SIZE.get().unwrap(),
                )
            };
            let hdr = CrossDomainHeader::ref_from_bytes(
                &ring_slice[0..mem::size_of::<CrossDomainHeader>()],
            )
            .unwrap();

            match hdr.cmd {
                CROSS_DOMAIN_CMD_RECEIVE => {
                    let cmd = CrossDomainSendReceive::ref_from_bytes(
                        &ring_slice[0..mem::size_of::<CrossDomainSendReceive>()],
                    )
                    .unwrap();
                    let data = &ring_slice[mem::size_of::<CrossDomainSendReceive>()
                        ..mem::size_of::<CrossDomainSendReceive>()
                            + (cmd.opaque_data_size as usize)];

                    let mut fds = SmallVec::<[OwnedFd; CROSS_DOMAIN_MAX_IDENTIFIERS]>::new_const();
                    for i in 0..(cmd.num_identifiers as usize) {
                        match cmd.identifier_types[i] {
                            CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB => {
                                let blob = drm
                                    .resource_create_blob(
                                        cmd.identifier_sizes[i] as usize,
                                        BlobMem::Host3d,
                                        BlobFlags::USE_MAPPABLE | BlobFlags::USE_SHARABLE,
                                        Some(cmd.identifiers[i] as u64),
                                    )
                                    .context("create_blob on CMD_RECEIVE failed")?;
                                let fd = drm
                                    .buffer_to_prime_fd(
                                        blob.bo_handle,
                                        drm_sys::DRM_CLOEXEC | drm_sys::DRM_RDWR,
                                    )
                                    .context("GEM to FD failed")?;
                                drm.close_buffer(blob.bo_handle)
                                    .context("Close buffer failed")?;

                                fds.push(fd);
                            }
                            CROSS_DOMAIN_ID_TYPE_WRITE_PIPE => {
                                let (read_fd, write_fd) =
                                    rustix::pipe::pipe().context("Failed to allocate pipe")?;
                                self.pipes_read_from_guest.insert(
                                    cmd.identifiers[i],
                                    (
                                        Generic::new_with_error(
                                            read_fd,
                                            Interest::READ,
                                            Mode::Level,
                                        ),
                                        false,
                                    ),
                                );
                                res = PostAction::Reregister;

                                fds.push(write_fd);
                            }
                            x => {
                                error!("Unhandled identifier type: {:?}!", x);
                            }
                        }
                    }

                    self.remote_data.move_to_front();
                    self.remote_data.get_writable_storage()[0..data.len()].copy_from_slice(data);
                    self.remote_data.advance(data.len());
                    self.remote_fds.extend(fds.into_iter());

                    while let Some((msg, obj)) = try_read_message(
                        &mut self.remote_data,
                        &mut self.remote_fds,
                        &self.local.map,
                    )
                    .inspect_err(|err| debug!("end of messages: {:?}", err))
                    .ok()
                    {
                        // NOTE: Do not and_then the filtering, the loop must continue if we skip a message!
                        if let Some(msg) = self
                            .protocol_state
                            .filter_host_message(&self.node, msg, &obj)
                        {
                            self.local
                                .write_message(&msg)
                                .context("Failed to write message")?;
                        }
                    }
                    self.local.flush().context("Failed to flush messages")?;
                }
                CROSS_DOMAIN_CMD_READ => {
                    let cmd = CrossDomainReadWrite::read_from_bytes(
                        &ring_slice[0..mem::size_of::<CrossDomainReadWrite>()],
                    )
                    .unwrap();
                    let data = &ring_slice[mem::size_of::<CrossDomainReadWrite>()
                        ..mem::size_of::<CrossDomainReadWrite>() + (cmd.opaque_data_size as usize)];

                    if let Some(write_fd) = self.pipes_write_from_host.get(&cmd.identifier) {
                        let written = rustix::io::write(&write_fd, data)
                            .context("Failed to write to pipe")?;
                        if written < (cmd.opaque_data_size as usize) {
                            warn!("Not written enough data!");
                        }

                        if cmd.hang_up != 0 {
                            self.pipes_write_from_host.remove(&cmd.identifier).unwrap();
                            res = PostAction::Reregister;
                        }
                    } else {
                        error!("Unknown pipe read: {:?}", cmd.identifier);
                    }
                }
                x => {
                    warn!("Unknown cross domain event: {:?}", x);
                }
            }

            poll(drm, &self.channel_ring).context("drm poll failed")?;

            Ok(res)
        })? {
            PostAction::Reregister if res == PostAction::Continue => {
                res = PostAction::Reregister;
            }
            PostAction::Disable if res != PostAction::Remove => {
                res = PostAction::Disable;
            }
            PostAction::Remove => {
                res = PostAction::Remove;
            }
            _ => {}
        }

        let mut to_remove = Vec::new();
        let drm = unsafe { self.drm.get_mut() };
        for (id, (pipe, _)) in self.pipes_read_from_guest.iter_mut() {
            match pipe.process_events(readiness, token, |_, pipe| {
                log::debug!("Local pipe event");
                let mut res = PostAction::Continue;
                let mut buf = [0u8; MAX_BYTES_OUT];
                let mut cmd = CrossDomainReadWrite {
                    hdr: CrossDomainHeader {
                        cmd: CROSS_DOMAIN_CMD_WRITE,
                        ring_idx: CROSS_DOMAIN_RING_NONE as u8,
                        cmd_size: mem::size_of::<CrossDomainReadWrite>() as u16,
                        pad: 0,
                    },
                    identifier: *id,
                    hang_up: 0,
                    opaque_data_size: 0,
                    pad: 0,
                };

                match rustix::io::read(
                    pipe.as_fd(),
                    &mut buf[mem::size_of::<CrossDomainReadWrite>()..],
                ) {
                    Ok(read_bytes) => {
                        if read_bytes == 0 {
                            cmd.hang_up = 1;
                        } else {
                            cmd.hdr.cmd_size += read_bytes as u16;
                            cmd.opaque_data_size = read_bytes as u32;

                            if read_bytes < MAX_BYTES_OUT - mem::size_of::<CrossDomainReadWrite>() {
                                cmd.hang_up = 1;
                            }
                        }
                    }
                    Err(_) => {
                        cmd.hang_up = 1;
                    }
                }

                if cmd.hang_up != 0 {
                    to_remove.push(*id);
                    res = PostAction::Reregister;
                }

                cmd.write_to(&mut buf[0..mem::size_of::<CrossDomainReadWrite>()])
                    .unwrap();
                drm.execbuffer(&buf[0..cmd.hdr.cmd_size as usize], None)
                    .context("execbuf pipe write failed")?;

                Ok(res)
            })? {
                PostAction::Reregister if res == PostAction::Continue => {
                    res = PostAction::Reregister;
                }
                PostAction::Disable if res != PostAction::Remove => {
                    res = PostAction::Disable;
                }
                PostAction::Remove => {
                    res = PostAction::Remove;
                }
                _ => {}
            }
        }
        for id in to_remove {
            self.pipes_read_from_guest.remove(&id);
        }

        Ok(res)
    }

    fn register(
        &mut self,
        poll: &mut calloop::Poll,
        token_factory: &mut calloop::TokenFactory,
    ) -> calloop::Result<()> {
        self.local.register(poll, token_factory)?;
        self.drm.register(poll, token_factory)?;
        for (pipe, _) in self.pipes_read_from_guest.values_mut() {
            pipe.register(poll, token_factory)?;
        }
        Ok(())
    }

    fn reregister(
        &mut self,
        poll: &mut calloop::Poll,
        token_factory: &mut calloop::TokenFactory,
    ) -> calloop::Result<()> {
        self.local.reregister(poll, token_factory)?;
        self.drm.reregister(poll, token_factory)?;
        for (pipe, is_registered) in self.pipes_read_from_guest.values_mut() {
            if *is_registered {
                pipe.reregister(poll, token_factory)?;
            } else {
                pipe.register(poll, token_factory)?;
                *is_registered = true;
            }
        }
        Ok(())
    }

    fn unregister(&mut self, poll: &mut calloop::Poll) -> calloop::Result<()> {
        self.local.unregister(poll)?;
        self.drm.unregister(poll)?;
        for (pipe, is_registered) in self.pipes_read_from_guest.values_mut() {
            if *is_registered {
                pipe.unregister(poll)?;
            }
        }
        Ok(())
    }
}

fn try_read_message(
    buffer: &mut Buffer<u8>,
    fds: &mut VecDeque<OwnedFd>,
    obj_map: &ObjectMap<Data>,
) -> Result<(Message<u32, OwnedFd>, Object<Data>), MessageParseError> {
    let (msg, read_data) = {
        let data = buffer.get_contents();
        if data.len() < 2 * 4 {
            return Err(MessageParseError::MissingData);
        }
        let object_id = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
        let word_2 = u32::from_ne_bytes([data[4], data[5], data[6], data[7]]);
        let opcode = (word_2 & 0x0000_FFFF) as u16;
        let len = (word_2 >> 16) as usize;

        let Some(obj) = obj_map.find(object_id) else {
            // This *really* should not happen, as e.g. incoming events for destroyed objects
            // (before the destruction was ACKed) are handled in the regular happy path.
            error!(
                "try_read_message: could not find object id {object_id} for msg (op={opcode}, len={len}), trying to skip",
            );
            if fds.len() != 0 {
                // FDs act as receive barriers, so they *should* belong to the first message we see in a recvmsg.
                // If these semantics hold with the virtgpu transport, this might even work, but scream extra loudly nonetheless.
                error!(
                    "try_read_message: expect further breakage since a message with file descriptors ({}) is being skipped",
                    fds.len()
                );
                fds.clear();
            }
            buffer.offset(len);
            return Err(MessageParseError::Malformed);
        };
        if let Some(sig) = obj
            .interface
            .events
            .get(opcode as usize)
            .map(|desc| desc.signature)
        {
            match parse_message(data, sig, fds) {
                Ok((msg, rest_data)) => ((msg, obj), data.len() - rest_data.len()),
                Err(e) => return Err(e),
            }
        } else {
            // no signature found ?
            return Err(MessageParseError::Malformed);
        }
    };

    buffer.offset(read_data);

    Ok(msg)
}
