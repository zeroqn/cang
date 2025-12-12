use anyhow::{Context, bail};
use pico_args::Arguments;
use std::{ffi::OsString, os::fd::OwnedFd, path::PathBuf};

#[derive(Debug)]
pub struct Args {
    pub mode: Mode,
    pub filters: Vec<String>,
}

impl From<Mode> for Args {
    fn from(mode: Mode) -> Self {
        Args {
            mode,
            filters: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum Mode {
    Help,
    Version,
    SingleSocket {
        launch_args: Option<Vec<OsString>>,
        fd: Option<OwnedFd>,
    },
    Listen {
        socket_name: Option<String>,
        socket_path: Option<PathBuf>,
        fd: Option<OwnedFd>,
    },
}

pub const HELP: &str = "\
wl-cross-domain-proxy

Tool to proxy wayland connections via virtio-gpu cross domain contexts.

USAGE:
  wl-cross-domain-proxy [OPTIONS] [-- PROGRAM ARGS...]

OPTIONS:
  -h, --help                    Prints help information
  -V, --version                 Prints version information
  --listen                      Proxy will listen on the next free `wayland-*`-socket in `$XDG_RUNTIME_DIR`.
                                (Default; Conflicts with '--listen-fd' and '--accept-fd'.)
    --socket-name <NAME>        Explicitly specifies the name to use for the listening socket in '$XDG_RUNTIME_DIR'.
                                (Implies '--listen'; Conflicts with '--socket-path'.)
    --socket-path <PATH>        Explicitly specifies the full path to use for the listening socket.
                                (Implies '--listen'; Conflicts with '--socket-name'.)
  --listen-fd                   Proxy will expect to be passed a listening wayland socket via
                                socket-activation it shall accept connections from.
                                (Conflicts with '--listen' and '--accept-fd'.)
  --accept-fd                   Proxy will expect to be passed a single connected wayland socket
                                via socket-activation. (Conflicts with '--listen' and '--listen-fd'.)
  -f, --filter-global <NAME>    Hides the specified global name from proxied applications.

ARGS:
  <PROGRAM> <ARGS..>            Spawns the specified program and sets `$WAYLAND_SOCKET` to the proxy.
                                (Conflicts with '--listen', '--listen-fd' and '--accept-fd'.)
";

pub fn parse_args() -> Result<Args, anyhow::Error> {
    // -- <launch_args>         | launch process with WAYLAND_SOCKET
    // --accept-fd              | single socket activation mode
    // --listen-fd              | listen socket activation mode
    // --listen                 | listen socket mode (default)
    //   --socket-name          | explicitly set WAYLAND_DISPLAY
    //   --socket-path          | explicitly set socket path
    // --filter-global <name>   | filter wayland global
    // --help / -v

    let mut args: Vec<_> = std::env::args_os().collect();
    args.remove(0); // remove the executable path.

    let launch_args = if let Some(dash_dash) = args.iter().position(|arg| arg == "--") {
        let launch_args = args.drain(dash_dash + 1..).collect();
        // .. then remove the `--`
        args.pop();
        Some(launch_args)
    } else {
        None
    };

    let mut args = Arguments::from_vec(args);

    let exclusive_flags = ["--accept-fd", "--listen-fd", "--listen"];
    let exclusive_listen_flags = ["--socket-name", "--socket-path"];

    let check_flag = |to_check: &str, mut args: Arguments| -> Result<(), anyhow::Error> {
        for flag in exclusive_flags.iter().filter(|f| **f != to_check) {
            if args.contains(*flag) {
                bail!("Cannot combine `{}` with `{}`", to_check, flag);
            }
        }
        if to_check != "--listen" {
            for flag in &exclusive_listen_flags {
                if args.contains(*flag) {
                    bail!("`{}` is only valid with `--listen` mode", flag);
                }
            }
        }
        let remaining = args.finish();
        if !remaining.is_empty() {
            bail!("Unused/unknown flags {:?}", remaining);
        }

        if launch_args.is_some() {
            bail!("Cannot combine <-- launch_flags> with {}", to_check);
        }

        Ok(())
    };

    let mut socket_fd = sd_listen_fds::get()
        .ok()
        .and_then(|fds| fds.into_iter().next())
        .map(|(_name, fd)| fd.into_std());

    let filters = args
        .values_from_str(["-f", "--filter-global"])
        .context("Failed to parse arguments")?;

    if args.contains(["-h", "--help"]) {
        Ok(Mode::Help.into())
    } else if args.contains(["-V", "--version"]) {
        Ok(Mode::Version.into())
    } else if args.contains("--accept-fd") {
        check_flag("--accept-fd", args)?;
        if socket_fd.is_none() {
            bail!("`--accept-fd` without socket-activation");
        }

        Ok(Args {
            mode: Mode::SingleSocket {
                launch_args: None,
                fd: socket_fd,
            },
            filters,
        })
    } else if args.contains("--listen-fd") {
        check_flag("--listen-fd", args)?;
        if socket_fd.is_none() {
            bail!("`--listen-fd` without socket-activation");
        }

        Ok(Args {
            mode: Mode::Listen {
                socket_name: None,
                socket_path: None,
                fd: socket_fd,
            },
            filters,
        })
    } else {
        let socket_name = args.value_from_str("--socket-name").ok();
        let socket_path = args.value_from_str("--socket-path").ok();
        if args.contains("--listen") || socket_name.is_some() || socket_path.is_some() {
            socket_fd.take();
        } else if launch_args.is_some() {
            return Ok(Args {
                mode: Mode::SingleSocket {
                    launch_args,
                    fd: None,
                },
                filters,
            });
        }
        check_flag("--listen", args)?;

        if socket_name.is_some() && socket_path.is_some() {
            bail!("`--socket-name` and `--socket-path` are mutually exclusive");
        }

        Ok(Args {
            mode: Mode::Listen {
                socket_name,
                socket_path,
                fd: socket_fd,
            },
            filters,
        })
    }
}
