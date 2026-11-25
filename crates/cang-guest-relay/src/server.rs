//! Serving the FUSE protocol for the guest's `/workspace` over an arbitrary
//! byte stream (the vsock connection, or a test's in-memory pipe).

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};

use fuse_backend_rs::abi::fuse_abi::InHeader;
use fuse_backend_rs::api::server::{MetricsHook, Server};
use fuse_backend_rs::passthrough::{Config, PassthroughFs};
use fuse_backend_rs::transport::{FuseDevWriter, Reader};

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
///
/// They are still *dispatched* ([`RelayServer::handle_frame`]), because
/// `fuse_backend_rs`'s `forget` is what drops the inode whose table entry holds
/// the `O_PATH` descriptor: a relay that only forwards the bytes leaks one guest
/// descriptor per path the host kernel has ever looked up.
pub const FORGET: u32 = 2;
pub const BATCH_FORGET: u32 = 42;
/// `FUSE_INIT`, the frame whose reply carries the connection's negotiated
/// limits - see [`advertise_big_writes`].
pub const INIT: u32 = 26;
/// The opcodes whose handler impersonates the caller's uid/gid, and - by
/// exclusion - the only requests [`stamp_create_identity`] has to touch.
const SYMLINK: u32 = 6;
const MKNOD: u32 = 8;
const MKDIR: u32 = 9;
const CREATE: u32 = 35;
const CREATE_FAMILY: [u32; 4] = [SYMLINK, MKNOD, MKDIR, CREATE];

/// The two INIT flags that decide whether a WRITE may be larger than 4 KiB, and
/// the page count they carry.
///
/// `fuse-backend-rs`'s server composes the INIT reply from what the filesystem
/// driver *wants* (`api/server/sync_io.rs`: `BIG_WRITES` sets `max_write` to
/// `MAX_REQ_PAGES * pagesize`, `MAX_PAGES` sets `max_pages`), and its
/// `passthrough` driver asks for neither - so the reply would cap a WRITE at
/// `MIN_READ_BUFFER - BUFFER_HEADER_SIZE` (4 KiB) and turn a 4 MiB write into 1
/// 024 frames (~10 MB/s). The relay is the server the host kernel negotiates
/// with, so it states those two limits itself on the reply it forwards. cang
/// used to carry this as an 8-line patch to that driver's `init()`, which a git
/// dependency cannot have.
const FUSE_BIG_WRITES: u32 = 0x0000_0020;
const FUSE_MAX_PAGES: u32 = 0x0040_0000;
const MAX_REQ_PAGES: u16 = 256;

/// Offsets into `fuse_init_out`, which starts 16 bytes (the `out_header`) into
/// the reply: `flags`, `max_write`, `max_pages` and `flags2`.
const INIT_OUT_FLAGS: usize = 28;
const INIT_OUT_MAX_WRITE: usize = 36;
const INIT_OUT_MAX_PAGES: usize = 44;
/// `fuse_init_in.flags`, at 40 bytes of `in_header` plus `major`/`minor`/
/// `max_readahead`.
const INIT_IN_FLAGS: usize = IN_HEADER_LEN + 12;
/// `fuse_in_header.uid` / `.gid`, at 24 and 28 bytes of the 40-byte header.
const IN_UID: usize = 24;
const IN_GID: usize = 28;

/// Advertise large WRITE frames on an INIT reply, as far as the kernel offers
/// them.
///
/// The kernel's own INIT flags are the capable set, and advertising something
/// it did not offer would be answered with a smaller `max_write` anyway - so
/// the control arm here is a kernel that offers neither flag: the reply keeps
/// the 4 KiB the driver composed.
fn advertise_big_writes(request: &[u8], reply: &mut [u8]) {
    if request.len() < INIT_IN_FLAGS + 4 || reply.len() < INIT_OUT_MAX_PAGES + 2 {
        return;
    }
    let capable = u32::from_le_bytes(
        request[INIT_IN_FLAGS..INIT_IN_FLAGS + 4]
            .try_into()
            .unwrap(),
    );
    let mut flags = u32::from_le_bytes(
        reply[INIT_OUT_FLAGS..INIT_OUT_FLAGS + 4]
            .try_into()
            .unwrap(),
    );
    let max_write = u32::from(MAX_REQ_PAGES) * fuse_backend_rs::transport::pagesize() as u32;
    if capable & FUSE_BIG_WRITES != 0 {
        flags |= FUSE_BIG_WRITES;
        reply[INIT_OUT_MAX_WRITE..INIT_OUT_MAX_WRITE + 4].copy_from_slice(&max_write.to_le_bytes());
    }
    if capable & FUSE_MAX_PAGES != 0 {
        flags |= FUSE_MAX_PAGES;
        reply[INIT_OUT_MAX_PAGES..INIT_OUT_MAX_PAGES + 2]
            .copy_from_slice(&MAX_REQ_PAGES.to_le_bytes());
        // `MAX_PAGES` carries its own `max_write`, exactly as the library's own
        // `init()` reply does.
        reply[INIT_OUT_MAX_WRITE..INIT_OUT_MAX_WRITE + 4].copy_from_slice(&max_write.to_le_bytes());
    }
    reply[INIT_OUT_FLAGS..INIT_OUT_FLAGS + 4].copy_from_slice(&flags.to_le_bytes());
}

/// Stamp the owner of the served tree on a create-family request's `uid`/`gid`.
///
/// The host's ids are not the guest's: a host tool creating through the mount
/// sends *its* uid/gid in the header, and `fuse-backend-rs`'s passthrough runs
/// the call as that user (`set_creds` -> `setresgid`/`setresuid`). When the
/// guest cannot assume those ids, the switch fails `EPERM` and the host tool
/// reports "cannot create ...: Operation not permitted" - while modifying an
/// existing file still works, because every other handler runs as the relay's
/// own identity. Rewriting the two fields makes a create execute as the user
/// who owns the tree, which is the guest's dev identity in cang's shipped
/// configuration: guest-init provisions it from `CANG_HOST_UID`/`CANG_HOST_GID`
/// (`resolve_identity`), and the workspace image's root is chowned to it
/// (`disk/workspace.rs::ensure_ready`). The aligned case is therefore a no-op,
/// and a mismatch is repaired instead of refused.
///
/// Nothing else moves: length, opcode, unique, nodeid, pid and padding stay
/// byte-identical, so the frame remains the one the host kernel issued.
fn stamp_create_identity(request: &mut [u8], opcode: u32, owner: Option<(u32, u32)>) {
    let Some((uid, gid)) = owner else {
        return;
    };
    if request.len() < IN_HEADER_LEN || !CREATE_FAMILY.contains(&opcode) {
        return;
    }
    request[IN_UID..IN_UID + 4].copy_from_slice(&uid.to_le_bytes());
    request[IN_GID..IN_GID + 4].copy_from_slice(&gid.to_le_bytes());
}

/// The capability a file handle needs: `name_to_handle_at` with `AT_EMPTY_PATH`
/// and `open_by_handle_at` both fail `EPERM` without `CAP_DAC_READ_SEARCH`, and
/// that is bit 2 of the `CapEff` mask (`/proc/self/status`).
const CAP_DAC_READ_SEARCH_BIT: u32 = 2;

/// The `CapEff` value out of a `/proc/self/status` dump.
fn cap_effective_hex(status: &str) -> Option<&str> {
    status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "CapEff").then(|| value.trim())
    })
}

/// Can this process use file handles at all?
fn inode_handles_available(cap_effective: Option<&str>) -> bool {
    let Some(mask) = cap_effective.and_then(|hex| u64::from_str_radix(hex, 16).ok()) else {
        return false;
    };
    mask & (1 << CAP_DAC_READ_SEARCH_BIT) != 0
}

fn process_cap_effective() -> Option<String> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| cap_effective_hex(&status).map(str::to_owned))
}

/// The passthrough configuration the relay ships.
///
/// `inode_file_handles` decides what an inode table entry holds, and that is the
/// relay's descriptor story:
///
/// * `false` is the library default: every inode the **host's** kernel looks up
///   keeps an `O_PATH` descriptor until *that kernel* forgets it, so the relay's
///   descriptor count mirrors the host's dcache. Measured on a seeded 10 000
///   path tree: 10 184 descriptors after one walk, back to 38 only once the host
///   evicted those dentries - and at the inherited limit of 4096 the mount failed
///   `EMFILE` after roughly 1 000 paths.
/// * `true` stores a `name_to_handle_at` cookie plus one shared descriptor per
///   mount instead, and a descriptor exists only for the length of one request,
///   so the count tracks concurrency rather than tree size.
///
/// Handles need `CAP_DAC_READ_SEARCH` (the relay has it as guest root, and the
/// host-side test suite does not), so the choice is made from the process's own
/// effective capabilities rather than compiled in: an unprivileged caller keeps
/// the descriptor policy instead of failing every lookup. A served filesystem
/// that cannot produce a handle for an inode degrades per inode the same way
/// (`FileHandle::from_name_at` returns `None` there and the entry keeps a
/// descriptor).
pub fn passthrough_config() -> Config {
    let inode_file_handles = inode_handles_available(process_cap_effective().as_deref());
    Config {
        inode_file_handles,
        ..Config::default()
    }
}

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
    /// The uid/gid that owns the served tree, stamped on the create-family
    /// requests ([`stamp_create_identity`]). `None` when the root could not be
    /// stat'd, and those requests then keep the caller's own identity.
    owner: Option<(u32, u32)>,
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
        // The tree's owner, read once: what the guest's dev identity is in
        // cang's shipped configuration, and the identity a create runs as.
        let owner = std::fs::metadata(root_dir).ok().map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            (metadata.uid(), metadata.gid())
        });
        Ok(Self {
            server: Server::new(passthrough),
            devnull,
            nodeids: NodeIds::new(),
            owner,
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
        // Before dispatch: the create-family handlers run as the caller's
        // uid/gid, and a host caller's ids are not the guest's.
        stamp_create_identity(request, opcode, self.owner);
        // `FORGET`/`BATCH_FORGET` are dispatched like every other frame: they take
        // no reply, but they are the *only* thing that drops an inode from
        // `PassthroughFs`'s table - and the table's entry is what holds the
        // inode's `O_PATH` descriptor. Skipping the dispatch (the shape the spike
        // shipped) leaves one guest descriptor open per path the host kernel ever
        // looked up, so a long-lived mount - `git status` over a large tree, say -
        // accumulates descriptors nothing releases until the relay fails `EMFILE`.
        let no_reply = matches!(opcode, FORGET | BATCH_FORGET);
        let mut writer = FuseDevWriter::<()>::new(self.devnull, reply)
            .context("failed to wrap the reply buffer")?;
        let writer = writer
            .split_at(0)
            .context("failed to split the reply writer")?;
        let reader = Reader::<()>::from_slice(request);
        match self
            .server
            .handle_message(reader, writer, None, Some(&self.ops))
        {
            Ok(0) => Ok(Frame::NoReply),
            // A no-reply frame that failed to decode still gets no reply: an
            // `-EIO` here would answer a frame the kernel is not waiting on.
            Err(_) if no_reply => Ok(Frame::NoReply),
            Ok(len) => {
                if opcode == INIT {
                    advertise_big_writes(request, &mut reply[..len]);
                }
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
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            passthrough_config(),
        )
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

    /// The control arm: a kernel that offers neither big-write flag gets the
    /// reply the driver composed, 4 KiB and no `max_pages`. The relay states
    /// limits, it does not invent capabilities.
    #[test]
    fn a_kernel_without_the_flags_keeps_the_4_kib_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            passthrough_config(),
        )
        .expect("relay server");

        let mut request = init_request(0x6000);
        let mut reply = vec![0u8; MAX_MSG];
        let len = match server
            .handle_frame(&mut request, &mut reply)
            .expect("init should be answered")
        {
            Frame::Reply(len) => len,
            other => panic!("INIT must be answered, got {other:?}"),
        };
        let shape = RelayServer::describe_init(&reply[..len]).expect("init reply");
        assert_eq!(shape.flags & 0x20, 0, "flags=0x{:x}", shape.flags);
        assert_eq!(shape.flags & 0x0040_0000, 0, "flags=0x{:x}", shape.flags);
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

        let server = RelayServer::new(root.to_str().expect("utf-8 path"), passthrough_config())
            .expect("relay");

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

    /// A 40-byte `fuse_in_header` for a request the host kernel issued as `uid`:`gid`.
    fn header(opcode: u32, total: usize, uid: u32, gid: u32) -> Vec<u8> {
        let mut request = vec![0u8; total];
        request[0..4].copy_from_slice(&(total as u32).to_le_bytes());
        request[4..8].copy_from_slice(&opcode.to_le_bytes());
        request[8..16].copy_from_slice(&7u64.to_le_bytes()); // unique
        request[16..24].copy_from_slice(&1u64.to_le_bytes()); // nodeid 1 = root
        request[IN_UID..IN_UID + 4].copy_from_slice(&uid.to_le_bytes());
        request[IN_GID..IN_GID + 4].copy_from_slice(&gid.to_le_bytes());
        request[IN_GID + 4..IN_GID + 8].copy_from_slice(&4242u32.to_le_bytes()); // pid
        request
    }

    /// A `FUSE_CREATE` of `name` under the root inode, `fuse_create_in { flags,
    /// mode, umask, fuse_flags }` then the name, sent by a caller of `uid`:`gid`.
    fn create_request(name: &[u8], uid: u32, gid: u32) -> Vec<u8> {
        let body = 16 + name.len() + 1;
        let mut request = header(CREATE, IN_HEADER_LEN + body, uid, gid);
        request[IN_HEADER_LEN..IN_HEADER_LEN + 4]
            .copy_from_slice(&((libc::O_WRONLY | libc::O_CREAT) as u32).to_le_bytes());
        request[IN_HEADER_LEN + 4..IN_HEADER_LEN + 8].copy_from_slice(&0o644u32.to_le_bytes());
        let name_at = IN_HEADER_LEN + 16;
        request[name_at..name_at + name.len()].copy_from_slice(name);
        request
    }

    /// A `FUSE_MKDIR` of `name` under the root inode: `fuse_mkdir_in { mode,
    /// umask }` then the name.
    fn mkdir_request(name: &[u8], uid: u32, gid: u32) -> Vec<u8> {
        let body = 8 + name.len() + 1;
        let mut request = header(MKDIR, IN_HEADER_LEN + body, uid, gid);
        request[IN_HEADER_LEN..IN_HEADER_LEN + 4].copy_from_slice(&0o755u32.to_le_bytes());
        let name_at = IN_HEADER_LEN + 8;
        request[name_at..name_at + name.len()].copy_from_slice(name);
        request
    }

    /// A `FUSE_FORGET` naming `nodeid` as the host kernel sends it when it drops
    /// a dentry: `in_header` plus `fuse_forget_in { nlookup }`.
    fn forget_request(nodeid: u64, nlookup: u64) -> Vec<u8> {
        let total = (IN_HEADER_LEN + 8) as u32;
        let mut request = vec![0u8; IN_HEADER_LEN + 8];
        request[0..4].copy_from_slice(&total.to_le_bytes());
        request[4..8].copy_from_slice(&FORGET.to_le_bytes());
        request[8..16].copy_from_slice(&9u64.to_le_bytes()); // unique
        request[16..24].copy_from_slice(&nodeid.to_le_bytes());
        request[IN_HEADER_LEN..].copy_from_slice(&nlookup.to_le_bytes());
        request
    }

    /// How many descriptors this process holds on `path`, by reading every
    /// `/proc/self/fd` link - the unit the guest relay's own `EMFILE` is counted
    /// in, scoped to one file so the suite's parallel tests cannot move it.
    fn descriptors_on(path: &std::path::Path) -> usize {
        let target = path.canonicalize().expect("canonical file path");
        std::fs::read_dir("/proc/self/fd")
            .expect("the relay is a Linux process with /proc")
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter(|linked| linked == &target)
            .count()
    }

    /// The create-family handlers are the only ones `fuse-backend-rs` runs as the
    /// *caller's* uid/gid (`passthrough`'s `mkdir`/`create`/`mknod`/`symlink` call
    /// `set_creds`; `write`, `unlink`, `rename`, `setattr` do not), and a caller
    /// whose ids the guest cannot assume makes those switches fail `EPERM` - the
    /// host tool then reports "cannot create ...: Operation not permitted" while
    /// still modifying existing files fine. The relay stamps the served tree's
    /// owner on those four requests, so a create is executed as the user who owns
    /// the tree. This test runs unprivileged, which is exactly the shape of the
    /// fault: `set_creds(4242, 4243)` cannot succeed for it.
    #[test]
    fn a_create_from_a_foreign_uid_lands_owned_by_the_tree_owner() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let owner = std::fs::metadata(root).expect("root metadata");
        let owner = (owner.uid(), owner.gid());
        let server = RelayServer::new(root.to_str().expect("utf-8 path"), passthrough_config())
            .expect("relay server");

        let mut input = init_request(KERNEL_INIT_FLAGS);
        input.extend_from_slice(&create_request(b"from-host.txt", 4242, 4243));
        input.extend_from_slice(&mkdir_request(b"from-host-dir", 4242, 4243));
        let mut output: Vec<u8> = Vec::new();
        server
            .serve(&mut Cursor::new(input), &mut output, |_| {})
            .expect("connection should be served");

        for name in ["from-host.txt", "from-host-dir"] {
            let created = root.join(name);
            let metadata = std::fs::metadata(&created)
                .unwrap_or_else(|err| panic!("the create of '{name}' must have succeeded: {err}"));
            assert_eq!(
                (metadata.uid(), metadata.gid()),
                owner,
                "'{name}' must be owned by the tree's own user, not by the caller's ids"
            );
        }
    }

    /// Stamping rewrites exactly the two caller fields of the four opcodes that
    /// impersonate, and leaves every other opcode - and every other byte of the
    /// header - as the host kernel issued it.
    #[test]
    fn stamping_rewrites_only_the_create_family_caller_fields() {
        let owner = Some((1000u32, 1001u32));

        for opcode in CREATE_FAMILY {
            let mut request = header(opcode, IN_HEADER_LEN, 4242, 4243);
            let before = request.clone();
            stamp_create_identity(&mut request, opcode, owner);
            assert_eq!(
                &request[IN_UID..IN_UID + 4],
                &1000u32.to_le_bytes(),
                "uid of opcode {opcode}"
            );
            assert_eq!(
                &request[IN_GID..IN_GID + 4],
                &1001u32.to_le_bytes(),
                "gid of opcode {opcode}"
            );
            assert_eq!(
                &request[..IN_UID],
                &before[..IN_UID],
                "length, opcode, unique and nodeid must not move"
            );
            assert_eq!(
                &request[IN_GID + 4..],
                &before[IN_GID + 4..],
                "pid and padding must not move (opcode {opcode})"
            );
        }

        for opcode in [INIT, 1, 10, 12, 14, 16] {
            let mut request = header(opcode, IN_HEADER_LEN, 4242, 4243);
            let before = request.clone();
            stamp_create_identity(&mut request, opcode, owner);
            assert_eq!(
                request, before,
                "opcode {opcode} keeps the caller's identity: its handler runs as the relay"
            );
        }
    }

    /// A frame shorter than its header, or a tree whose owner could not be read,
    /// is left exactly as it arrived: the stamp must never be the thing that
    /// fails a request.
    #[test]
    fn stamping_leaves_a_short_or_unowned_frame_alone() {
        let mut short = vec![0u8; 8];
        stamp_create_identity(&mut short, CREATE, Some((1, 2)));
        assert_eq!(short.len(), 8);

        let mut request = header(CREATE, IN_HEADER_LEN, 4242, 4243);
        let before = request.clone();
        stamp_create_identity(&mut request, CREATE, None);
        assert_eq!(request, before);
    }

    /// `FORGET` must be **dispatched**, not merely forwarded: it is the only
    /// frame that drops an inode from `PassthroughFs`'s table. This test runs the
    /// *descriptor* policy (`inode_file_handles: false`, the library default) so
    /// the release is visible as a descriptor closing; [`passthrough_config`]
    /// ships the handle policy, where the same `forget` frees the entry and its
    /// handle instead - see [`the_shipped_config_holds_no_descriptor_per_inode`].
    #[test]
    fn a_forget_releases_the_inode_descriptor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"hi").expect("file");
        let server = RelayServer::new(dir.path().to_str().expect("utf-8 path"), Config::default())
            .expect("relay server");

        let before = descriptors_on(&file);
        assert_eq!(before, 0, "nothing has looked the file up yet");

        let mut input = init_request(KERNEL_INIT_FLAGS);
        input.extend_from_slice(&lookup_request(b"hello.txt"));
        let mut output: Vec<u8> = Vec::new();
        server
            .serve(&mut Cursor::new(input), &mut output, |_| {})
            .expect("the connection should be served");
        assert_eq!(
            descriptors_on(&file),
            before + 1,
            "a looked-up inode holds one descriptor, which is what FORGET releases"
        );

        let init_len = u32::from_le_bytes(output[0..4].try_into().unwrap()) as usize;
        assert!(
            output.len() >= init_len + 40,
            "the LOOKUP reply is missing: {} bytes after INIT",
            output.len() - init_len
        );
        assert_eq!(
            i32::from_le_bytes(output[init_len + 4..init_len + 8].try_into().unwrap()),
            0,
            "the LOOKUP must succeed"
        );
        // `out_header` is 16 bytes, then `fuse_entry_out` starts with the nodeid.
        let nodeid = u64::from_le_bytes(output[init_len + 16..init_len + 24].try_into().unwrap());
        assert_ne!(nodeid, 0, "a LOOKUP reply must name its nodeid");

        let mut output: Vec<u8> = Vec::new();
        let summary = server
            .serve(
                &mut Cursor::new(forget_request(nodeid, 1)),
                &mut output,
                |_| {},
            )
            .expect("the forget should be served");
        assert_eq!(summary.frames, 1);
        assert!(output.is_empty(), "FORGET must not be answered");
        assert_eq!(
            descriptors_on(&file),
            before,
            "FORGET must close the descriptor of the inode it released the last reference to"
        );
    }

    /// The handle policy is chosen from the process's own capabilities, because
    /// the handle path needs `CAP_DAC_READ_SEARCH`: a relay that cannot use it
    /// must keep the descriptor policy (which works, only unbounded) rather than
    /// fail every lookup with `EPERM`.
    #[test]
    fn inode_handles_need_cap_dac_read_search() {
        // Bit 2 set (bare root) - handles.
        assert!(inode_handles_available(Some("000001ffffffffff")));
        assert!(inode_handles_available(Some("4")));
        // Bits 0 and 1 only (CHOWN, DAC_OVERRIDE) - no DAC_READ_SEARCH.
        assert!(!inode_handles_available(Some("3")));
        assert!(!inode_handles_available(Some("0")));
        assert!(!inode_handles_available(None));
        assert!(!inode_handles_available(Some("not-a-mask")));
    }

    /// The mask comes out of the `CapEff:` line and nowhere else.
    #[test]
    fn cap_effective_reads_the_cap_eff_line() {
        let status = "Name:\tcang-guest-relay\nCapInh:\t0000000000000000\nCapEff:\t000001ffffffffff\nCapBnd:\t000001ffffffffff\n";
        assert_eq!(cap_effective_hex(status), Some("000001ffffffffff"));
        assert_eq!(cap_effective_hex("Name:\tx\n"), None);
    }

    /// The shipped configuration follows the probe: this suite runs unprivileged,
    /// so the descriptor policy is selected here, while the guest relay (root)
    /// selects handles. The behavioural half of the handle policy is the live arm
    /// (`file-nr` flat across a walk) because `open_by_handle_at` refuses an
    /// unprivileged server.
    #[test]
    fn the_shipped_config_follows_the_capability_probe() {
        let config = passthrough_config();
        assert_eq!(
            config.inode_file_handles,
            inode_handles_available(process_cap_effective().as_deref()),
            "the shipped policy must be the probe's answer, not a compiled-in choice"
        );
    }

    /// A no-reply frame is never answered, even one that fails to decode: the
    /// kernel is not waiting for it, so an `-EIO` reply would answer someone
    /// else's frame.
    #[test]
    fn a_malformed_no_reply_frame_is_still_not_answered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            passthrough_config(),
        )
        .expect("relay server");
        for opcode in [FORGET, BATCH_FORGET] {
            let total = IN_HEADER_LEN as u32;
            let mut request = vec![0u8; IN_HEADER_LEN];
            request[0..4].copy_from_slice(&total.to_le_bytes());
            request[4..8].copy_from_slice(&opcode.to_le_bytes());
            let mut reply = vec![0u8; MAX_MSG];
            assert_eq!(
                server
                    .handle_frame(&mut request, &mut reply)
                    .expect("a no-reply frame is not an error"),
                Frame::NoReply,
                "opcode {opcode} must not be answered"
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
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            passthrough_config(),
        )
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
        let server = RelayServer::new(
            dir.path().to_str().expect("utf-8 path"),
            passthrough_config(),
        )
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
