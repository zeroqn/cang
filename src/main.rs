use std::{
    ffi::OsString,
    fs, io,
    os::{
        fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Command, ExitCode},
    rc::Rc,
    str::FromStr,
};

use anyhow::Context;
use calloop::{EventLoop, LoopHandle};
use drm::{
    Device,
    control::Device as ControlDevice,
    node::{CreateDrmNodeError, DrmNode},
};
use log::warn;

use crate::{
    args::{Args, Mode},
    source::channel::ClientChannel,
    virtio_gpu::VirtioDevice,
};

mod args;
mod cross_domain;
mod sigbus;
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
    res: ExitCode,
}

fn main() -> Result<ExitCode, anyhow::Error> {
    env_logger::init();
    sigbus::install_handler();

    let drm_device =
        find_virtio_dri_node().with_context(|| "Failed to find virtio_gpu dri device")?;

    let mut event_loop =
        EventLoop::<'static, State>::try_new().with_context(|| "Failed to create event loop")?;
    let handle = event_loop.handle();

    match self::args::parse_args()? {
        Args {
            mode: Mode::Help, ..
        } => {
            println!("{}", self::args::HELP);
            return Ok(ExitCode::SUCCESS);
        }
        Args {
            mode: Mode::Version,
            ..
        } => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Args {
            mode: Mode::SingleSocket { launch_args, fd },
            filters,
        } => {
            let client_stream = if let Some(fd) = fd {
                UnixStream::from(fd)
            } else if let Some(args) = launch_args {
                let (client_stream, client_end) =
                    UnixStream::pair().context("Failed to allocate unix stream pair")?;
                let fd = Into::<OwnedFd>::into(client_end).into_raw_fd();
                let mut cmd = Command::new(&args[0]);

                cmd.args(&args[1..]);
                cmd.env("WAYLAND_SOCKET", fd.to_string());

                unsafe {
                    cmd.pre_exec(move || {
                        rustix::io::ioctl_fionclex(BorrowedFd::borrow_raw(fd)).map_err(Into::into)
                    });
                }

                let mut _child = cmd.spawn().context("Failed to spawn child process")?;
                #[cfg(target_os = "linux")]
                {
                    use calloop::{Interest, Mode::Edge, generic::Generic};
                    use rustix::process::{Pid, PidfdFlags, pidfd_open};

                    let fd = pidfd_open(Pid::from_child(&_child), PidfdFlags::NONBLOCK)
                        .context("Failed to get pidfd of child")?;
                    let signal = event_loop.get_signal();
                    handle
                        .insert_source(
                            Generic::new_with_error::<anyhow::Error>(fd, Interest::READ, Edge),
                            move |_, _, state| {
                                use calloop::PostAction;

                                signal.stop();
                                state.res = _child
                                    .wait()
                                    .map(|status| {
                                        if let Some(code) = status.code() {
                                            (code as u8).into()
                                        } else {
                                            if status.success() {
                                                ExitCode::SUCCESS
                                            } else {
                                                ExitCode::FAILURE
                                            }
                                        }
                                    })
                                    .context("Failed to receive child status")?;
                                Ok(PostAction::Remove)
                            },
                        )
                        .context("Failed to watch child status")?;
                }

                client_stream
            } else {
                unreachable!()
            };

            client_stream
                .set_nonblocking(true)
                .context("Failed to set stream non-blocking")?;
            let channel = ClientChannel::new(client_stream, drm_device.clone(), filters)?;
            handle
                .insert_source(channel, |_, _, _| {})
                .map_err(|ins| ins.error)
                .context("Error on client channel")?;
        }
        Args {
            mode:
                Mode::Listen {
                    socket_name,
                    socket_path,
                    fd,
                },
            filters,
        } => {
            let source = if let Some(fd) = fd {
                self::source::listen::ListeningSocketSource::try_from(fd)
                    .with_context(|| "Failed to create source from listen fd")?
            } else {
                if let Some(path) = socket_path {
                    self::source::listen::ListeningSocketSource::with_path(path)
                        .with_context(|| "Failed to listen for wayland connections")?
                } else if let Some(name) = socket_name {
                    self::source::listen::ListeningSocketSource::with_name(&name)
                        .with_context(|| "Failed to listen for wayland connections")?
                } else {
                    self::source::listen::ListeningSocketSource::new_auto()
                        .with_context(|| "Failed to listen for wayland connections")?
                }
            };

            handle.insert_source(source, move |client_stream, _, state| {
                let channel =
                    ClientChannel::new(client_stream, drm_device.clone(), filters.clone())?;
                state
                    .evlh
                    .insert_source(channel, |_, _, _| {})
                    .map_err(|ins| ins.error)
                    .context("Error on client channel")?;
                Ok(())
            })?;
        }
    }

    #[cfg(target_os = "linux")]
    {
        use calloop::signals::{Signal, Signals};

        let signals = Signals::new(&[Signal::SIGINT])
            .context("Failed to register interrupt signal handler")?;
        let sig = event_loop.get_signal();
        handle.insert_source(signals, move |_, _, _| {
            sig.stop();
            sig.wakeup();
        })?;
    }

    let mut state = State {
        evlh: handle,
        res: ExitCode::SUCCESS,
    };
    event_loop
        .run(None, &mut state, |_| {})
        .context("Event loop crashed")?;

    Ok(state.res)
}
