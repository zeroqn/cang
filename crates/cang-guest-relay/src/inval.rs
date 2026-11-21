//! FUSE cache invalidation: the frames a guest-side write sends back to the
//! host's kernel, and the one writer replies and notifications share.
//!
//! A guest-side write (`cargo fmt`, a checkout, a codegen step) changes the
//! guest's btrfs tree directly and is invisible to the host's page cache: a host
//! reader that keeps a descriptor open sees the old bytes until its attribute
//! timeout (5 s) - and never, for a same-size rewrite. The FUSE protocol's answer
//! is a *notification*: a write to the mount's `/dev/fuse` whose
//! `out_header.unique` is `0` and whose `out_header.error` is a
//! `fuse_notify_code`. The kernel takes it for one whatever the mount
//! negotiated, and **only `INVAL_INODE` with `off = 0`** drops the pages of an
//! open inode (`off = -1` bumps the attributes and leaves the pages stale;
//! `INVAL_ENTRY` revalidates a name and its attributes but never an open
//! inode's page cache). Note 66 measured all of it live.
//!
//! # Coalescing
//!
//! A `target/`-style storm is ~4.4 inotify events per file (CREATE, MODIFY,
//! CLOSE_WRITE, DELETE from tar's unlink-then-create) at 7.5-8.1 k events/s, so
//! one notification per event would be 4.4 frames per file for two distinct
//! facts. The watcher therefore coalesces **per path within one drain batch**:
//! a batch is everything readable from the inotify descriptor at one go, its
//! events collapse to a set of content paths and a set of `(parent, name)`
//! entry pairs, and the batch is flushed immediately. There is deliberately **no
//! time window**: a window would add its own latency to the single-write case
//! this feature exists for (the acceptance band is ~1 ms), and the batch already
//! grows exactly when the queue backs up, which is the storm. What a batch
//! collapse buys is bounded and measurable: MODIFY+CLOSE_WRITE for one file are
//! one `INVAL_INODE` frame, and a repeated content event for a path already in
//! the batch is dropped - so a storm of N files costs at most one `INVAL_INODE`
//! per file, not 4.4 events.
//!
//! A notification the host kernel refuses (a nodeid the mount no longer holds)
//! answers `ENOENT` to the pump's write; the pump logs and skips it (note 66
//! §3) - that is why a nodeid this side never sent is not an error, just a
//! write that is not performed.
//!
//! Both notification kinds have a frame builder here, but the watcher sends only
//! **`INVAL_INODE`**: `INVAL_ENTRY`'s kernel path (`fuse_reverse_inval_entry`)
//! waits on the parent directory's inode lock, and with one serial server on the
//! connection that becomes a hung mount (`watch`'s module documentation records
//! the measurement). The entry builder stays, with its tests, because the
//! mechanism and its rejection belong in the code.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fuse_backend_rs::abi::fuse_abi::{NotifyInvalEntryOut, NotifyInvalInodeOut, NotifyOpcode};

use crate::server::OUT_HEADER_LEN;

/// 24 bytes: `ino`, `off`, `len` (`struct fuse_notify_inval_inode_out`).
const INVAL_INODE_BODY_LEN: usize = std::mem::size_of::<NotifyInvalInodeOut>();
/// 16 bytes: `parent`, `namelen`, `flags` (`struct fuse_notify_inval_entry_out`).
const INVAL_ENTRY_BODY_LEN: usize = std::mem::size_of::<NotifyInvalEntryOut>();
/// The kernel's own limit on an `INVAL_ENTRY` name (`fs/fuse/dev.c` refuses a
/// longer one), so a longer one is not sent at all.
pub const MAX_INVAL_ENTRY_NAME: usize = 1024;
/// The `INVAL_INODE` frame: the header plus `ino`/`off`/`len`.
pub const INVAL_INODE_FRAME_LEN: usize = OUT_HEADER_LEN + INVAL_INODE_BODY_LEN;

/// `FUSE_NOTIFY_INVAL_INODE` with `off = 0, len = 0`: drop the whole page cache
/// of `nodeid`, and its attributes with it.
///
/// `unique` stays 0 - that is what makes the kernel read the write as a
/// notification rather than as a reply - and `off = 0` is the content lever
/// (`off = -1` would bump `attr_version` only, leaving the pages of an open
/// descriptor stale, which is the measured `not invalidated` arm).
pub fn inval_inode_frame(nodeid: u64) -> Vec<u8> {
    let mut frame = vec![0u8; INVAL_INODE_FRAME_LEN];
    frame[0..4].copy_from_slice(&(INVAL_INODE_FRAME_LEN as u32).to_le_bytes());
    frame[4..8].copy_from_slice(&(NotifyOpcode::InvalInode as i32).to_le_bytes());
    // 8..16 = unique = 0.
    frame[16..24].copy_from_slice(&nodeid.to_le_bytes());
    frame[24..32].copy_from_slice(&0i64.to_le_bytes());
    frame[32..40].copy_from_slice(&0i64.to_le_bytes());
    frame
}

/// `FUSE_NOTIFY_INVAL_ENTRY` for `name` in `parent`: revalidate the name and its
/// attributes (`fuse_reverse_inval_entry`).
///
/// `None` when the name cannot be sent: empty, or longer than the kernel's
/// limit.
pub fn inval_entry_frame(parent: u64, name: &[u8]) -> Option<Vec<u8>> {
    if name.is_empty() || name.len() > MAX_INVAL_ENTRY_NAME {
        return None;
    }
    let total = OUT_HEADER_LEN + INVAL_ENTRY_BODY_LEN + name.len() + 1;
    let mut frame = vec![0u8; total];
    frame[0..4].copy_from_slice(&(total as u32).to_le_bytes());
    frame[4..8].copy_from_slice(&(NotifyOpcode::InvalEntry as i32).to_le_bytes());
    // 8..16 = unique = 0.
    frame[16..24].copy_from_slice(&parent.to_le_bytes());
    frame[24..28].copy_from_slice(&(name.len() as u32).to_le_bytes());
    // 28..32 = flags = 0 (no FUSE_EXPIRE_ONLY: the entry is dropped, not expired).
    frame[OUT_HEADER_LEN + INVAL_ENTRY_BODY_LEN..total - 1].copy_from_slice(name);
    // The last byte is the NUL terminator the kernel expects after the name.
    Some(frame)
}

/// The one connection the FUSE server and the watcher write through.
///
/// Replies and notifications share the guest's single transport, so every write
/// goes through one lock and one whole-message `write_all`: a notification can
/// then land between two replies (the pump delivers it to `/dev/fuse` on its
/// own `unique`) but never inside one.
#[derive(Clone)]
pub struct SharedWriter {
    /// Boxed so a test can hand it a socketpair: any `Write` the connection is
    /// made of serves, and nothing here reads.
    connection: Arc<Mutex<Box<dyn Write + Send>>>,
    frames: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

impl SharedWriter {
    pub fn new(connection: impl Write + Send + 'static) -> Self {
        Self {
            connection: Arc::new(Mutex::new(Box::new(connection) as Box<dyn Write + Send>)),
            frames: Arc::new(AtomicU64::new(0)),
            bytes: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Write one whole FUSE message, holding the connection for the whole of it.
    pub fn write_frame(&self, frame: &[u8]) -> io::Result<()> {
        let mut connection = self.connection.lock().map_err(|_| {
            io::Error::other("the relay connection's write lock was poisoned by a panicking writer")
        })?;
        connection.write_all(frame)?;
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(frame.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Messages written through this handle, replies and notifications alike.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Whole-buffer on purpose: `Write::write` returning a short count would
        // let `write_all` split one FUSE message across two lock acquisitions,
        // and another frame could then land in the middle of it.
        self.write_frame(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut connection = self.connection.lock().map_err(|_| {
            io::Error::other("the relay connection's write lock was poisoned by a panicking writer")
        })?;
        connection.flush()
    }
}

/// Sends invalidations, and counts them.
#[derive(Clone)]
pub struct Notifier {
    writer: SharedWriter,
    emitted: Arc<AtomicU64>,
    refused: Arc<AtomicU64>,
}

impl Notifier {
    pub fn new(writer: SharedWriter) -> Self {
        Self {
            writer,
            emitted: Arc::new(AtomicU64::new(0)),
            refused: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn writer(&self) -> &SharedWriter {
        &self.writer
    }

    /// `INVAL_INODE off=0 len=0` for `nodeid`.
    pub fn invalidate_inode(&self, nodeid: u64) -> io::Result<()> {
        self.writer.write_frame(&inval_inode_frame(nodeid))?;
        self.emitted.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// `INVAL_ENTRY` for `name` in `parent`; `false` when the name could not be
    /// carried (nothing is sent for it).
    pub fn invalidate_entry(&self, parent: u64, name: &[u8]) -> io::Result<bool> {
        let Some(frame) = inval_entry_frame(parent, name) else {
            self.refused.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };
        self.writer.write_frame(&frame)?;
        self.emitted.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Notifications written to the transport.
    pub fn emitted(&self) -> u64 {
        self.emitted.load(Ordering::Relaxed)
    }

    /// Names that could not be carried at all (too long for the kernel).
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::thread;

    fn header(frame: &[u8]) -> (u32, i32, u64) {
        (
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            i32::from_le_bytes(frame[4..8].try_into().unwrap()),
            u64::from_le_bytes(frame[8..16].try_into().unwrap()),
        )
    }

    /// Note 66 §1 measured the frame the kernel accepts: 40 bytes, `error` 2
    /// (`INVAL_INODE`), `unique` 0, then `ino`, `off`, `len`.
    #[test]
    fn the_inval_inode_frame_is_the_forty_byte_notification() {
        let frame = inval_inode_frame(0x2a);
        assert_eq!(frame.len(), 40);
        assert_eq!(header(&frame), (40, NotifyOpcode::InvalInode as i32, 0));
        assert_eq!(u64::from_le_bytes(frame[16..24].try_into().unwrap()), 0x2a);
        assert_eq!(i64::from_le_bytes(frame[24..32].try_into().unwrap()), 0);
        assert_eq!(i64::from_le_bytes(frame[32..40].try_into().unwrap()), 0);
    }

    /// `INVAL_ENTRY` is `16 + 16 + namelen + 1` with the name NUL-terminated, so
    /// a 15-character name is 48 bytes (note 66's table).
    #[test]
    fn the_inval_entry_frame_carries_the_parent_and_a_nul_terminated_name() {
        let frame = inval_entry_frame(1, b"workspace-file1").expect("frame");
        assert_eq!(frame.len(), 16 + 16 + 15 + 1);
        assert_eq!(header(&frame), (48, NotifyOpcode::InvalEntry as i32, 0));
        assert_eq!(u64::from_le_bytes(frame[16..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(frame[24..28].try_into().unwrap()), 15);
        assert_eq!(u32::from_le_bytes(frame[28..32].try_into().unwrap()), 0);
        assert_eq!(&frame[32..47], b"workspace-file1");
        assert_eq!(frame[47], 0);
    }

    /// A name the kernel would refuse is never sent.
    #[test]
    fn an_entry_name_the_kernel_cannot_carry_is_refused() {
        assert!(inval_entry_frame(1, b"").is_none());
        assert!(inval_entry_frame(1, &[b'x'; MAX_INVAL_ENTRY_NAME + 1]).is_none());
        assert!(inval_entry_frame(1, &[b'x'; MAX_INVAL_ENTRY_NAME]).is_some());
    }

    /// Two writers sharing one connection never interleave: every frame the peer
    /// reads back is a whole message with its own length.
    #[test]
    fn concurrent_writers_never_split_a_frame() {
        let (ours, mut peer) = UnixStream::pair().expect("pair");
        let writer = SharedWriter::new(ours.try_clone().expect("clone"));
        let mut handles = Vec::new();
        for writer_index in 0..2u8 {
            let writer = writer.clone();
            handles.push(thread::spawn(move || {
                for index in 0..50u64 {
                    // A frame with a distinctive length and body.
                    let size = 40 + (index as usize % 7) * 8 + writer_index as usize;
                    let mut frame = vec![writer_index; size];
                    frame[0..4].copy_from_slice(&(size as u32).to_le_bytes());
                    frame[4..8].copy_from_slice(&(NotifyOpcode::InvalInode as i32).to_le_bytes());
                    writer.write_frame(&frame).expect("write");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer");
        }
        drop(writer);
        drop(ours);

        let mut seen = 0usize;
        loop {
            let mut head = [0u8; 4];
            let mut filled = 0;
            while filled < 4 {
                match std::io::Read::read(&mut peer, &mut head[filled..]) {
                    Ok(0) => return,
                    Ok(n) => filled += n,
                    Err(err) => panic!("read failed: {err}"),
                }
            }
            let total = u32::from_le_bytes(head) as usize;
            let mut body = vec![0u8; total - 4];
            std::io::Read::read_exact(&mut peer, &mut body).expect("body");
            // One writer per frame: everything after the header fields is one
            // writer's fill byte, so a frame spliced from two would show it.
            let fill = body[4];
            assert!(
                body[4..].iter().all(|&byte| byte == fill),
                "frame {seen} was spliced from two writers"
            );
            seen += 1;
        }
    }

    /// The writer counts what it wrote, replies included.
    #[test]
    fn the_shared_writer_counts_frames() {
        let (ours, _peer) = UnixStream::pair().expect("pair");
        let writer = SharedWriter::new(ours);
        writer.write_frame(&inval_inode_frame(1)).expect("frame");
        let notifier = Notifier::new(writer.clone());
        assert!(notifier.invalidate_entry(1, b"x").expect("entry"));
        assert_eq!(
            writer.frames(),
            2,
            "the reply writer's own frame + one notification"
        );
        assert_eq!(
            notifier.emitted(),
            1,
            "the notifier counts only its own frames"
        );
        assert_eq!(
            writer.bytes(),
            40 + 34,
            "40-byte INVAL_INODE + 16+16+1+1 INVAL_ENTRY"
        );
    }
}
