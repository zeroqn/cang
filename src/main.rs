use std::{
    ffi::OsString,
    fs, io,
    os::fd::{AsFd, BorrowedFd},
    rc::Rc,
    str::FromStr,
};

use anyhow::Context;
#[cfg(target_os = "linux")]
use calloop::signals::{Signal, Signals};
use calloop::{EventLoop, LoopHandle};
use drm::{
    Device,
    control::Device as ControlDevice,
    node::{CreateDrmNodeError, DrmNode},
};
use log::warn;

use crate::{source::channel::ClientChannel, virtio_gpu::VirtioDevice};

mod cross_domain;
mod source;
#[allow(unused)]
mod virtio_gpu;
mod wl_proto;

#[derive(Debug, Clone)]
struct DrmDevice(Rc<fs::File>);
impl AsFd for DrmDevice {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for DrmDevice {}
impl ControlDevice for DrmDevice {}
impl VirtioDevice for DrmDevice {}

fn find_virtio_dri_node() -> io::Result<DrmNode> {
    // TODO: Use udev for this? and not hardcode the path?
    const DRI_PATH: &'static str = "/dev/dri";

    for path in fs::read_dir(DRI_PATH)?
        .filter_map(|res| res.ok())
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().starts_with("/dev/dri/renderD"))
    {
        let file = match fs::File::open(&path) {
            Ok(candidate) => DrmDevice(Rc::new(candidate)),
            Err(err) => {
                warn!("Skipping over {}: {:?}", path.display(), err);
                continue;
            }
        };

        let Ok(drv) = file.get_driver() else { continue };

        if drv.name == OsString::from_str("virtio_gpu").unwrap() {
            match DrmNode::from_file(&file) {
                Ok(node) => return Ok(node),
                Err(CreateDrmNodeError::NotDrmNode) => continue,
                Err(CreateDrmNodeError::Io(err)) => return Err(err),
            };
        }
    }

    Err(io::ErrorKind::NotFound.into())
}

struct State {
    evlh: LoopHandle<'static, Self>,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let drm_device =
        find_virtio_dri_node().with_context(|| "Failed to find virtio_gpu dri device")?;
    let wayland_source = self::source::listen::ListeningSocketSource::new_auto()
        .with_context(|| "Failed to listen for wayland connections")?;

    let mut event_loop =
        EventLoop::<'static, State>::try_new().with_context(|| "Failed to create event loop")?;
    let handle = event_loop.handle();
    handle.insert_source(wayland_source, move |client_stream, _, state| {
        let channel = ClientChannel::new(client_stream, drm_device.clone())?;
        state
            .evlh
            .insert_source(channel, |_, _, _| {})
            .map_err(|ins| ins.error)
            .context("Error on client channel")?;
        Ok(())
    })?;

    #[cfg(target_os = "linux")]
    {
        let signals = Signals::new(&[Signal::SIGINT])
            .context("Failed to register interrupt signal handler")?;
        let sig = event_loop.get_signal();
        handle.insert_source(signals, move |_, _, _| {
            sig.stop();
            sig.wakeup();
        })?;
    }

    let mut state = State { evlh: handle };
    event_loop
        .run(None, &mut state, |_| {})
        .context("Event loop crashed")
}
