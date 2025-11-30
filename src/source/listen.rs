use std::{
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            net::{UnixListener, UnixStream},
            prelude::BorrowedFd,
        },
    },
    path::Path,
};

use anyhow::Context;
use calloop::{
    EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory, generic::Generic,
};
use log::info;
use wayland_server::{BindError, ListeningSocket};

/// A Wayland listening socket event source.
///
/// This implements [`EventSource`] and may be inserted into an event loop.
#[derive(Debug)]
pub struct ListeningSocketSource {
    socket: Generic<Socket>,
}

#[derive(Debug)]
enum Socket {
    WlServer(ListeningSocket),
    DirectUnix(UnixListener),
}

impl Socket {
    pub fn accept(&self) -> std::io::Result<Option<UnixStream>> {
        match self {
            Socket::WlServer(wl) => wl.accept(),
            Socket::DirectUnix(s) => match s.accept() {
                Ok((socket, _)) => Ok(Some(socket)),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(err) => Err(err),
            },
        }
    }
}

impl From<ListeningSocket> for Socket {
    fn from(value: ListeningSocket) -> Self {
        Socket::WlServer(value)
    }
}

impl From<UnixListener> for Socket {
    fn from(value: UnixListener) -> Self {
        Socket::DirectUnix(value)
    }
}

impl AsFd for Socket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Socket::WlServer(wl) => wl.as_fd(),
            Socket::DirectUnix(s) => s.as_fd(),
        }
    }
}

impl ListeningSocketSource {
    /// Creates a new listening socket, automatically choosing the next available `wayland` socket name.
    pub fn new_auto() -> Result<ListeningSocketSource, BindError> {
        // Try socket numbers 1-32. Remember the upper bound of Range is exclusive.
        //
        // We don't try wayland-0 due since clients may connect to the wrong compositor. Clients these days
        // should be connecting based off the WAYLAND_DISPLAY or WAYLAND_SOCKET environment variables.
        let socket = ListeningSocket::bind_auto("wayland", 1..33)?;
        info!("Created new socket: {:?}", socket.socket_name());

        Ok(ListeningSocketSource {
            socket: Generic::new(socket.into(), Interest::READ, Mode::Level),
        })
    }

    /// Creates a new listening socket with the specified name.
    pub fn with_name(name: &str) -> Result<ListeningSocketSource, BindError> {
        let socket = ListeningSocket::bind(name)?;
        info!("Created new socket: {:?}", socket.socket_name());

        Ok(ListeningSocketSource {
            socket: Generic::new(socket.into(), Interest::READ, Mode::Level),
        })
    }

    /// Creates a new listening socket with the specified path.
    pub fn with_path(path: impl AsRef<Path>) -> Result<ListeningSocketSource, BindError> {
        let socket = ListeningSocket::bind_absolute(path.as_ref().to_path_buf())?;

        Ok(ListeningSocketSource {
            socket: Generic::new(socket.into(), Interest::READ, Mode::Level),
        })
    }
}

impl TryFrom<OwnedFd> for ListeningSocketSource {
    type Error = io::Error;

    fn try_from(value: OwnedFd) -> Result<Self, io::Error> {
        let listener = UnixListener::from(value);
        listener.set_nonblocking(true)?;
        Ok(ListeningSocketSource {
            socket: Generic::new(listener.into(), Interest::READ, Mode::Level),
        })
    }
}

impl EventSource for ListeningSocketSource {
    /// A stream to the new client.
    ///
    /// You must register the  client using the stream by calling
    /// [`DisplayHandle::insert_client`](wayland_server::DisplayHandle::insert_client).
    type Event = UnixStream;
    type Metadata = ();
    type Ret = Result<(), anyhow::Error>;
    type Error = anyhow::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, anyhow::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut res = Ok(PostAction::Continue);

        self.socket
            .process_events(readiness, token, |_, socket| {
                while let Some(client) = socket.accept()? {
                    info!("New client connected: {:?}", client);
                    if let Err(err) = callback(client, &mut ()) {
                        res = Err(err);
                    }
                }

                Ok(PostAction::Continue)
            })
            .context("Failed to process wayland events")?;

        res
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.socket.unregister(poll)
    }
}
