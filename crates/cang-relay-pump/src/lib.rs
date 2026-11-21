//! The host half of cang's workspace reverse share: a `/dev/fuse` <-> vsock pump.
//!
//! With `--workspace-relay` cang mounts the launch directory on the host and
//! answers every operation from the *guest*, which serves the btrfs tree it
//! mounted at `/workspace`. This crate is the host end of that: it owns the
//! mount, owns `/dev/fuse`, and relays one FUSE message at a time to the guest
//! over the unix socket libkrun bound for the guest's vsock listener.
//!
//! # Why this is not run under cang's Landlock/seccomp
//!
//! The mount itself comes from libfuse's setuid helper: `fusermount3` performs
//! the `mount(2)` and hands the `/dev/fuse` descriptor back over a Unix socket
//! pair (`_FUSE_COMMFD` + `SCM_RIGHTS`) - exactly what libfuse's own
//! `fuse_mount()` does. A setuid helper cannot do its job for a process that
//! carries `no_new_privs`, which is how cang's VM worker runs (Landlock +
//! seccomp are installed on it before the VMM is built). The pump is therefore
//! started as a plain child of the cang CLI process, before the supervisor, and
//! confined by neither: it is the mount owner, not guest-facing parsing code.
//! What it touches is `/dev/fuse`, one unix socket, and its own stderr.
//!
//! # Failure: `-EIO`, never an unmount
//!
//! Ticket 23 decision 4(a), as amended by note 63: when the guest stops
//! answering, every subsequent request is answered `-EIO` **immediately** and
//! the mount stays. Path syscalls then fail with `EIO` and a fault on an
//! already-mapped page gets `SIGBUS` ~20 ms after its READ is refused. The two
//! alternatives are worse: closing `/dev/fuse` turns `EIO` into
//! `ENOTCONN`/`ECONNABORTED`, and a lazy `fusermount3 -u -z` leaves the (empty)
//! underlying directory in place, so path tools stop failing and start
//! reporting an empty workspace. The mount is unmounted only when cang itself
//! exits: the pump unmounts on `SIGTERM`, which is what cang sends after the
//! task ends.

pub mod fusermount;
pub mod mount;
pub mod pump;
