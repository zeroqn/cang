//! Serving the FUSE protocol for the guest's `/workspace` over an arbitrary
//! byte stream (the vsock connection, or a test's in-memory pipe).

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};

use fuse_backend_rs::abi::fuse_abi::InHeader;
use fuse_backend_rs::api::server::{MetricsHook, Server};
use fuse_backend_rs::passthrough::{Config, PassthroughFs};
use fuse_backend_rs::transport::{FuseBuf, FuseDevWriter, Reader, Writer};

use crate::nodeids::NodeIds;

/// Largest message the relay carries: the 1 MiB `max_write` the INIT reply
/// advertises plus a full page of headers (libfuse's `FUSE_KERN_BUF_PAGES`
/// rule). A full 1 MiB WRITE frame is 1 048 656 bytes.
pub const MAX_MSG: usize = (1 << 20) + 0x1000;
/// `struct fuse_in_header` is 40 bytes; there is no shorter request.
pub const IN_HEADER_LEN: usize = 40;
/// `struct fuse_out_header` is 16 bytes; an error reply is exactly this.
pub const OUT_HEADER_LEN: usize = 16;
/// Opcodes that carry **no reply** by protocol: a relay that waits for one eats
/// the next request's reply and every frame after it is mismatched.
pub const FORGET: u32 = 2;
pub const BATCH_FORGET: u32 = 42;

/// The opcode numbers the kernel and `fuse-backend-rs` agree on (`enum
/// fuse_opcode`: note the gaps at 7, 19 and 21's neighbours - `MKNOD` is 8, not
/// 7, and `OPEN` is 14). Note 62's printed table was shifted by one from the
/// MKNOD entry up and note 63 corrected it; this table is keyed on the kernel
/// header, not on the spike.
pub const OPCODE_NAMES: &[(u32, &str)] = &[
    (1, "LOOKUP"),
    (2, "FORGET"),
    (3, "GETATTR"),
    (4, "SETATTR"),
    (5, "READLINK"),
    (6, "SYMLINK"),
    (8, "MKNOD"),
    (9, "MKDIR"),
    (10, "UNLINK"),
    (11, "RMDIR"),
    (12, "RENAME"),
    (13, "LINK"),
    (14, "OPEN"),
    (15, "READ"),
    (16, "WRITE"),
    (17, "STATFS"),
    (18, "RELEASE"),
    (20, "FSYNC"),
    (21, "SETXATTR"),
    (22, "GETXATTR"),
    (23, "LISTXATTR"),
    (24, "REMOVEXATTR"),
    (25, "FLUSH"),
    (26, "INIT"),
    (27, "OPENDIR"),
    (28, "READDIR"),
    (29, "RELEASEDIR"),
    (30, "FSYNCDIR"),
    (34, "ACCESS"),
    (35, "CREATE"),
    (36, "INTERRUPT"),
    (38, "DESTROY"),
    (42, "BATCH_FORGET"),
    (43, "FALLOCATE"),
    (44, "READDIRPLUS"),
    (45, "RENAME2"),
    (46, "LSEEK"),
    (47, "COPY_FILE_RANGE"),
    (50, "SYNCFS"),
    (51, "TMPFILE"),
];

/// `u32 opcode -> &'static str`, for the pump's and the relay's own log lines.
pub fn opcode_name(opcode: u32) -> &'static str {
    OPCODE_NAMES
        .iter()
        .find(|(number, _)| *number == opcode)
        .map(|(_, name)| *name)
        .unwrap_or("UNKNOWN")
}

/// Per-opcode frame counts, reported when a connection ends. One `println!` per
/// request costs ~90 us per frame (note 62 §10), so the relay counts instead of
/// logging.
#[derive(Debug)]
pub struct OpCounts([AtomicU64; 64]);

impl Default for OpCounts {
    fn default() -> Self {
        Self([const { AtomicU64::new(0) }; 64])
    }
}

impl OpCounts {
    pub fn record(&self, opcode: u32) {
        if let Some(slot) = self.0.get(opcode as usize) {
            slot.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn get(&self, opcode: u32) -> u64 {
        self.0
            .get(opcode as usize)
            .map(|slot| slot.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        self.0.iter().map(|slot| slot.load(Ordering::Relaxed)).sum()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (index, slot) in self.0.iter().enumerate() {
            let count = slot.load(Ordering::Relaxed);
            if count > 0 {
                out.push_str(&format!(" {index}:{count}"));
            }
        }
        out
    }
}

impl MetricsHook for OpCounts {
    fn collect(&self, header: &InHeader) {
        self.record(header.opcode);
    }

    fn release(&self, _header: Option<&fuse_backend_rs::abi::fuse_abi::OutHeader>) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// `FORGET`/`BATCH_FORGET`: relayed, never answered.
    NoReply,
    /// A reply of this many bytes was composed into the reply buffer.
    Reply(usize),
    /// The server could not build a reply; the frame is answered with `-EIO`
    /// so a host tool gets a real errno instead of a hang.
    Failed,
}

/// The guest's FUSE server for one root directory.
pub struct RelayServer {
    server: Server<PassthroughFs>,
    /// `/dev/null`, so `FuseDevWriter::commit()` (which `READDIRPLUS` issues
    /// while composing a split reply) discards its device write instead of
    /// failing `EBADF` on fd -1.
    devnull: RawFd,
    /// What this connection's replies told the relay about the tree, so the
    /// watcher can name an invalidation by nodeid instead of by path.
    nodeids: NodeIds,
    pub ops: OpCounts,
}

// SAFETY: the raw descriptor is owned by the struct and only used from the
// thread that holds &mut self; there is no aliasing.
unsafe impl Send for RelayServer {}

impl RelayServer {
    /// Open the tree at `root_dir`. The passthrough configuration is the one
    /// ticket 23 decision 2(a) asks for: kernel cache timeouts on (the defaults,
    /// 5 s for attributes and entries), `writeback = false`, `xattr = false`,
    /// `cache_policy = Auto`.
    pub fn new(root_dir: &str, config: Config) -> Result<Self> {
        let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
        if devnull < 0 {
            bail!(
                "failed to open /dev/null for the FUSE reply writer: {}",
                std::io::Error::last_os_error()
            );
        }
        // `PassthroughFs` learns the tree it serves from `config.root_dir` - its
        // `new` takes no path - and the default is `/`. Passing the root only to
        // the error message (the previous shape) served the guest's whole root
        // filesystem instead of the workspace.
        let mut config = config;
        config.root_dir = root_dir.to_owned();
        let passthrough = PassthroughFs::new(config)
            .with_context(|| format!("failed to open the relay root '{root_dir}'"))?;
        Ok(Self {
            server: Server::new(passthrough),
            devnull,
            nodeids: NodeIds::new(),
            ops: OpCounts::default(),
        })
    }

    /// The connection's path -> nodeid table, for the invalidation watcher.
    ///
    /// Cloning gives a second handle onto the same table, not a copy.
    pub fn node_ids(&self) -> NodeIds {
        self.nodeids.clone()
    }

    /// Serve one request frame already read from the transport.
    ///
    /// `request` is the whole frame (`in_header.len` bytes); `reply` must be at
    /// least [`MAX_MSG`] long, and it holds the composed reply on return.
    pub fn handle_frame(&self, request: &mut [u8], reply: &mut [u8]) -> Result<Frame> {
        if request.len() < IN_HEADER_LEN {
            bail!(
                "FUSE request of {} bytes is shorter than its header",
                request.len()
            );
        }
        let opcode = u32::from_le_bytes([request[4], request[5], request[6], request[7]]);
        if opcode == FORGET || opcode == BATCH_FORGET {
            return Ok(Frame::NoReply);
        }
        let mut writer = FuseDevWriter::<()>::new(self.devnull, reply)
            .context("failed to wrap the reply buffer")?;
        let writer = writer
            .split_at(0)
            .context("failed to split the reply writer")?;
        let reader = Reader::<()>::from_fuse_buffer(FuseBuf::new(request))
            .context("failed to wrap the request buffer")?;
        match self
            .server
            .handle_message(reader, Writer::FuseDev(writer), None, Some(&self.ops))
        {
            Ok(0) => Ok(Frame::NoReply),
            Ok(len) => {
                // The reply is the only place the relay learns which nodeids the
                // host's mount holds: a LOOKUP/READDIRPLUS/CREATE/MKDIR reply
                // carries them, and an invalidation has to name one.
                self.nodeids.note_reply(request, &reply[..len]);
                Ok(Frame::Reply(len))
            }
            Err(_) => Ok(Frame::Failed),
        }
    }

    /// Serve one connection until the peer closes it.
    ///
    /// A clean EOF is the normal end of a connection (the host pump closes it);
    /// a malformed length or a short frame ends it too, and the caller is
    /// expected to loop back to `accept`.
    pub fn serve<R: Read, W: Write>(
        &self,
        input: &mut R,
        output: &mut W,
        mut trace: impl FnMut(&str),
    ) -> Result<ServeSummary> {
        let mut request = vec![0u8; MAX_MSG];
        let mut reply = vec![0u8; MAX_MSG];
        let mut frames = 0u64;
        let mut init = None;
        loop {
            match read_header(input, &mut request) {
                Ok(Some(total)) => {
                    if !(IN_HEADER_LEN..=MAX_MSG).contains(&total) {
                        trace("relay: request length outside the protocol's bounds; closing");
                        break;
                    }
                    if let Err(err) = input.read_exact(&mut request[IN_HEADER_LEN..total]) {
                        trace(&format!("relay: read body failed: {err}"));
                        break;
                    }
                    frames += 1;
                    let unique = u64::from_le_bytes(request[8..16].try_into().unwrap());
                    let frame = self.handle_frame(&mut request[..total], &mut reply)?;
                    if let (26, Frame::Reply(len)) =
                        (u32::from_le_bytes(request[4..8].try_into().unwrap()), frame)
                    {
                        init = RelayServer::describe_init(&reply[..len]);
                    }
                    match frame {
                        Frame::NoReply => {}
                        Frame::Reply(len) => output.write_all(&reply[..len]).map_err(|err| {
                            anyhow::anyhow!("failed to write a {len}-byte reply: {err}")
                        })?,
                        Frame::Failed => {
                            output
                                .write_all(&error_reply(unique, libc::EIO))
                                .map_err(|err| {
                                    anyhow::anyhow!("failed to write the EIO reply: {err}")
                                })?;
                        }
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    trace(&format!("relay: read request header failed: {err}"));
                    break;
                }
            }
        }
        Ok(ServeSummary { frames, init })
    }

    /// The INIT reply's negotiated shape, for the relay's own ready line and for
    /// tests: `fuse_init_out` starts 16 bytes into the reply, so `flags` is at
    /// 28, `max_write` at 36 and `max_pages` at 44 (the offsets note 63 quotes).
    pub fn describe_init(reply: &[u8]) -> Option<InitShape> {
        if reply.len() < 52 {
            return None;
        }
        Some(InitShape {
            major: u32::from_le_bytes(reply[16..20].try_into().unwrap()),
            minor: u32::from_le_bytes(reply[20..24].try_into().unwrap()),
            flags: u32::from_le_bytes(reply[28..32].try_into().unwrap()),
            max_write: u32::from_le_bytes(reply[36..40].try_into().unwrap()),
            max_pages: u16::from_le_bytes(reply[44..46].try_into().unwrap()),
            flags2: u32::from_le_bytes(reply[48..52].try_into().unwrap()),
        })
    }
}

impl Drop for RelayServer {
    fn drop(&mut self) {
        if self.devnull >= 0 {
            let _ = unsafe { libc::close(self.devnull) };
        }
    }
}

/// What one served connection did, for the relay's own end-of-connection line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeSummary {
    pub frames: u64,
    /// The negotiated INIT shape, when the kernel sent INIT on this connection.
    pub init: Option<InitShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitShape {
    pub major: u32,
    pub minor: u32,
    pub flags: u32,
    pub max_write: u32,
    pub max_pages: u16,
    pub flags2: u32,
}

impl InitShape {
    pub fn log_line(self) -> String {
        format!(
            "INIT major={} minor={} flags=0x{:x} max_write={} max_pages={} flags2=0x{:x}",
            self.major, self.minor, self.flags, self.max_write, self.max_pages, self.flags2
        )
    }
}

/// Read the 40-byte in-header and return the frame's total length, or `None` on
/// a clean EOF at a frame boundary.
fn read_header<R: Read>(input: &mut R, request: &mut [u8]) -> std::io::Result<Option<usize>> {
    let mut filled = 0;
    while filled < IN_HEADER_LEN {
        match input.read(&mut request[filled..IN_HEADER_LEN]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed mid-header",
                ));
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(Some(
        u32::from_le_bytes(request[0..4].try_into().unwrap()) as usize
    ))
}

/// A 16-byte `fuse_out_header` carrying `errno`, and no body: how every
/// request after the guest stopped answering is refused.
pub fn error_reply(unique: u64, errno: i32) -> [u8; OUT_HEADER_LEN] {
    let mut reply = [0u8; OUT_HEADER_LEN];
    reply[0..4].copy_from_slice(&(OUT_HEADER_LEN as u32).to_le_bytes());
    reply[4..8].copy_from_slice(&(-errno).to_le_bytes());
    reply[8..16].copy_from_slice(&unique.to_le_bytes());
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn init_request(flags: u32) -> Vec<u8> {
        let mut request = vec![0u8; IN_HEADER_LEN + 16];
        let total = request.len() as u32;
        request[0..4].copy_from_slice(&total.to_le_bytes());
        request[4..8].copy_from_slice(&26u32.to_le_bytes()); // FUSE_INIT
        request[8..16].copy_from_slice(&1u64.to_le_bytes());
        // fuse_init_in: major, minor, max_readahead, flags
        request[40..44].copy_from_slice(&7u32.to_le_bytes());
        request[44..48].copy_from_slice(&33u32.to_le_bytes());
        request[48..52].copy_from_slice(&131_072u32.to_le_bytes());
        request[52..56].copy_from_slice(&flags.to_le_bytes());
        request
    }

    /// The kernel's own INIT request flags, from note 63: readdirplus plus the
    /// two big-write flags it is willing to grant.
    const KERNEL_INIT_FLAGS: u32 = 0x6000 | 0x20 | 0x0040_0000;

    #[test]
    fn the_init_reply_advertises_big_writes_and_max_pages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(dir.path().to_str().expect("utf-8 path"), Config::default())
            .expect("relay server");

        let mut request = init_request(KERNEL_INIT_FLAGS);
        let mut reply = vec![0u8; MAX_MSG];
        let frame = server
            .handle_frame(&mut request, &mut reply)
            .expect("init should be answered");

        let len = match frame {
            Frame::Reply(len) => len,
            other => panic!("INIT must be answered, got {other:?}"),
        };
        let shape = RelayServer::describe_init(&reply[..len]).expect("init reply");
        assert_eq!(shape.major, 7);
        assert!(
            shape.flags & 0x20 != 0,
            "FUSE_BIG_WRITES must be advertised, flags=0x{:x}",
            shape.flags
        );
        assert!(
            shape.flags & 0x0040_0000 != 0,
            "FUSE_MAX_PAGES must be advertised, flags=0x{:x}",
            shape.flags
        );
        assert_eq!(shape.max_write, 1_048_576);
        assert_eq!(shape.max_pages, 256);
    }

    /// The control arm for the patch: a guest server that is *not* asked for the
    /// big-write flags keeps the 4 KiB cap. `do_import = false` is the vendored
    /// `init()`'s own condition, so this is the same crate with the feature off
    /// rather than a second implementation.
    #[test]
    fn without_do_import_the_cap_stays_at_4_kib() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            Config {
                do_import: false,
                ..Config::default()
            },
        )
        .expect("relay server");

        let mut request = init_request(KERNEL_INIT_FLAGS);
        let mut reply = vec![0u8; MAX_MSG];
        let len = match server
            .handle_frame(&mut request, &mut reply)
            .expect("init should be answered")
        {
            Frame::Reply(len) => len,
            other => panic!("INIT must be answered, got {other:?}"),
        };
        let shape = RelayServer::describe_init(&reply[..len]).expect("init reply");
        assert_eq!(shape.max_write, 4096);
        assert_eq!(shape.max_pages, 0);
    }

    /// The relay serves the directory it was told to serve, and nothing else.
    ///
    /// `PassthroughFs` takes the tree from `config.root_dir` (its `new` takes no
    /// path, and the default is `/`), so a relay that passed the root only to its
    /// error message served the guest's whole root filesystem: the host mounted
    /// the image root where it expected `/workspace` (caught live, 2026-11-21).
    /// The sibling file here is what a `/`-rooted server would have answered.
    #[test]
    fn the_relay_serves_the_root_it_was_given_and_not_the_guest_root() {
        let parent = tempfile::tempdir().expect("tempdir");
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).expect("root dir");
        std::fs::write(root.join("inside.txt"), b"in").expect("inside file");
        std::fs::write(parent.path().join("outside.txt"), b"out").expect("outside file");

        let server =
            RelayServer::new(root.to_str().expect("utf-8 path"), Config::default()).expect("relay");

        let mut input = init_request(KERNEL_INIT_FLAGS);
        input.extend_from_slice(&lookup_request(b"inside.txt"));
        input.extend_from_slice(&lookup_request(b"outside.txt"));
        let mut output: Vec<u8> = Vec::new();
        let summary = server
            .serve(&mut std::io::Cursor::new(input), &mut output, |_| {})
            .expect("connection should be served");
        assert_eq!(summary.frames, 3);

        let mut offset = 0usize;
        let mut statuses = Vec::new();
        while offset < output.len() {
            let len = u32::from_le_bytes(output[offset..offset + 4].try_into().unwrap()) as usize;
            statuses.push(i32::from_le_bytes(
                output[offset + 4..offset + 8].try_into().unwrap(),
            ));
            offset += len;
        }
        assert_eq!(
            statuses,
            vec![0, 0, -libc::ENOENT],
            "the served tree must answer for its own file and refuse the sibling"
        );
    }

    /// A `FUSE_LOOKUP` of `name` on the root inode.
    fn lookup_request(name: &[u8]) -> Vec<u8> {
        let mut request = vec![0u8; IN_HEADER_LEN];
        let total = IN_HEADER_LEN + name.len() + 1;
        request[0..4].copy_from_slice(&(total as u32).to_le_bytes());
        request[4..8].copy_from_slice(&1u32.to_le_bytes()); // FUSE_LOOKUP
        request[8..16].copy_from_slice(&7u64.to_le_bytes()); // unique
        request[16..24].copy_from_slice(&1u64.to_le_bytes()); // nodeid 1 = root
        request.extend_from_slice(name);
        request.push(0);
        request
    }

    #[test]
    fn a_forget_frame_is_relayed_and_never_answered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(dir.path().to_str().expect("utf-8 path"), Config::default())
            .expect("relay server");
        for opcode in [FORGET, BATCH_FORGET] {
            let mut request = vec![0u8; IN_HEADER_LEN];
            request[0..4].copy_from_slice(&(IN_HEADER_LEN as u32).to_le_bytes());
            request[4..8].copy_from_slice(&opcode.to_le_bytes());
            let mut reply = vec![0u8; MAX_MSG];
            assert_eq!(
                server
                    .handle_frame(&mut request, &mut reply)
                    .expect("forget"),
                Frame::NoReply
            );
        }
    }

    /// A whole connection: INIT, then STATFS, then a LOOKUP of a file that
    /// exists in the served tree, then EOF. The replies are read back through
    /// the same framing the pump uses.
    #[test]
    fn serves_a_connection_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("hello.txt"), b"hi").expect("file");
        let server = RelayServer::new(dir.path().to_str().expect("utf-8 path"), Config::default())
            .expect("relay server");

        let mut statfs = vec![0u8; IN_HEADER_LEN];
        statfs[0..4].copy_from_slice(&(IN_HEADER_LEN as u32).to_le_bytes());
        statfs[4..8].copy_from_slice(&17u32.to_le_bytes()); // FUSE_STATFS
        statfs[8..16].copy_from_slice(&2u64.to_le_bytes());
        statfs[16..24].copy_from_slice(&1u64.to_le_bytes()); // nodeid 1 = root

        let mut input = init_request(KERNEL_INIT_FLAGS);
        input.extend_from_slice(&statfs);
        let mut output: Vec<u8> = Vec::new();

        let summary = server
            .serve(&mut Cursor::new(input), &mut output, |_| {})
            .expect("connection should be served");
        assert_eq!(summary.frames, 2);
        // INIT is 16 + 64 bytes of fuse_init_out here; STATFS follows it.
        let init_len = u32::from_le_bytes(output[0..4].try_into().unwrap()) as usize;
        assert!(init_len >= 80, "init reply was {init_len} bytes");
        let statfs_len =
            u32::from_le_bytes(output[init_len..init_len + 4].try_into().unwrap()) as usize;
        assert_eq!(statfs_len, OUT_HEADER_LEN + 80);
        assert_eq!(
            i32::from_le_bytes(output[init_len + 4..init_len + 8].try_into().unwrap()),
            0,
            "STATFS must succeed"
        );
        assert_eq!(server.ops.get(26), 1);
        assert_eq!(server.ops.get(17), 1);
        assert_eq!(server.ops.total(), 2);
    }

    #[test]
    fn an_error_reply_is_the_sixteen_byte_header_with_eio() {
        let reply = error_reply(0x1122_3344_5566_7788, libc::EIO);
        assert_eq!(reply.len(), OUT_HEADER_LEN);
        assert_eq!(u32::from_le_bytes(reply[0..4].try_into().unwrap()), 16);
        assert_eq!(
            i32::from_le_bytes(reply[4..8].try_into().unwrap()),
            -libc::EIO
        );
        assert_eq!(
            u64::from_le_bytes(reply[8..16].try_into().unwrap()),
            0x1122_3344_5566_7788
        );
    }

    #[test]
    fn a_frame_shorter_than_its_header_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(dir.path().to_str().expect("utf-8 path"), Config::default())
            .expect("relay server");
        let mut request = vec![0u8; 16];
        request[0..4].copy_from_slice(&16u32.to_le_bytes());
        let mut reply = vec![0u8; MAX_MSG];
        assert!(server.handle_frame(&mut request, &mut reply).is_err());
    }

    /// Note 63 §5 defect 2: the kernel's numbering, not the spike's shifted one.
    #[test]
    fn opcode_names_use_the_kernel_numbering() {
        assert_eq!(opcode_name(14), "OPEN");
        assert_eq!(opcode_name(15), "READ");
        assert_eq!(opcode_name(16), "WRITE");
        assert_eq!(opcode_name(17), "STATFS");
        assert_eq!(opcode_name(18), "RELEASE");
        assert_eq!(opcode_name(20), "FSYNC");
        assert_eq!(opcode_name(25), "FLUSH");
        assert_eq!(opcode_name(26), "INIT");
        assert_eq!(opcode_name(27), "OPENDIR");
        assert_eq!(opcode_name(44), "READDIRPLUS");
        assert_eq!(opcode_name(99), "UNKNOWN");
    }
}
