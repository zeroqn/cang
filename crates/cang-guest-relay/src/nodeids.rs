//! The path -> nodeid table the watcher resolves guest-side writes through.
//!
//! A FUSE invalidation names an **inode** (`nodeid`), while `inotify` reports
//! **paths**, so the relay has to know which nodeid the mount holds for a path.
//! It learns that from its own replies, and only from them: the entry nodeid in
//! a `LOOKUP`/`CREATE`/`MKDIR` reply, and every entry in a `READDIRPLUS` reply.
//! A path the relay never answered for has nothing cached on the host's mount,
//! so there is nothing to invalidate for it - which is why an unknown path is
//! skipped rather than looked up on disk (the `ENOENT` arm note 66 measured is
//! the kernel agreeing: a nodeid the mount does not hold is refused).
//!
//! Keys are paths **relative to the served root**, `""` is the root itself
//! (nodeid 1). Both directions are kept: `nodeid -> path` resolves a request's
//! parent nodeid into a path, `path -> nodeid` resolves an event's path into the
//! nodeid an invalidation names.
//!
//! A `RENAME` reply carries no nodeid, so a rename is read from the *request*
//! (`newdir`, `oldname`, `newname`) and moves the subtree's keys: a host read
//! through a directory the guest moved must still resolve to that directory's
//! nodeid.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use fuse_backend_rs::abi::fuse_abi::{Dirent, EntryOut};

use crate::server::{IN_HEADER_LEN, OUT_HEADER_LEN};

/// The nodeid the kernel gives the root of every FUSE mount.
pub const ROOT_NODEID: u64 = 1;
pub const LOOKUP: u32 = 1;
pub const MKDIR: u32 = 9;
pub const RENAME: u32 = 12;
pub const CREATE: u32 = 35;
pub const READDIRPLUS: u32 = 44;
pub const RENAME2: u32 = 45;

/// `struct fuse_entry_out`.
const ENTRY_OUT_LEN: usize = std::mem::size_of::<EntryOut>();
/// `struct fuse_dirent`: `ino`, `off`, `namelen`, `type`.
const DIRENT_LEN: usize = std::mem::size_of::<Dirent>();
/// Request bodies that precede a name, after the 40-byte in-header.
const MKDIR_IN_LEN: usize = 8;
const CREATE_IN_LEN: usize = 16;
const RENAME_IN_LEN: usize = 8;
const RENAME2_IN_LEN: usize = 16;

#[derive(Default)]
struct Table {
    by_path: HashMap<PathBuf, u64>,
    by_node: HashMap<u64, PathBuf>,
}

/// The shared path/nodeid table. Cheap to clone: every clone is one table.
#[derive(Clone, Default)]
pub struct NodeIds {
    inner: Arc<Mutex<Table>>,
}

impl NodeIds {
    /// A table with only the root in it.
    pub fn new() -> Self {
        let table = Self::default();
        table.record(PathBuf::new(), ROOT_NODEID);
        table
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn record(&self, path: PathBuf, nodeid: u64) {
        let mut table = self.table();
        if let Some(previous) = table.by_node.get(&nodeid).cloned() {
            table.by_path.remove(&previous);
        }
        if let Some(previous) = table.by_path.insert(path.clone(), nodeid) {
            table.by_node.remove(&previous);
        }
        table.by_node.insert(nodeid, path);
    }

    /// The nodeid the mount holds for `path`, if the relay ever answered for it.
    pub fn nodeid(&self, path: &Path) -> Option<u64> {
        self.table().by_path.get(path).copied()
    }

    /// The parent nodeid and the entry name for `path`, i.e. what an
    /// `INVAL_ENTRY` needs.
    pub fn entry(&self, path: &Path) -> Option<(u64, Vec<u8>)> {
        let name = path.file_name()?.as_encoded_bytes().to_vec();
        let parent = path.parent()?;
        let parent_nodeid = self.nodeid(parent)?;
        Some((parent_nodeid, name))
    }

    /// Paths the relay has sent a nodeid for.
    pub fn len(&self) -> usize {
        self.table().by_path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 1
    }

    /// Record what this request's reply says about the tree.
    ///
    /// Every opcode that carries an entry in its reply is here; anything else
    /// (`GETATTR`, `READ`, `WRITE`, a `FORGET`) says nothing about nodeids, and
    /// an error reply says nothing at all.
    pub fn note_reply(&self, request: &[u8], reply: &[u8]) {
        if request.len() < IN_HEADER_LEN || reply.len() < OUT_HEADER_LEN {
            return;
        }
        if i32::from_le_bytes(reply[4..8].try_into().unwrap()) != 0 {
            return;
        }
        let opcode = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let nodeid = u64::from_le_bytes(request[16..24].try_into().unwrap());
        match opcode {
            LOOKUP => {
                let Some(name) = nul_terminated(request, IN_HEADER_LEN) else {
                    return;
                };
                self.note_entry(nodeid, name, reply, OUT_HEADER_LEN);
            }
            MKDIR => {
                let Some(name) = nul_terminated(request, IN_HEADER_LEN + MKDIR_IN_LEN) else {
                    return;
                };
                self.note_entry(nodeid, name, reply, OUT_HEADER_LEN);
            }
            CREATE => {
                let Some(name) = nul_terminated(request, IN_HEADER_LEN + CREATE_IN_LEN) else {
                    return;
                };
                self.note_entry(nodeid, name, reply, OUT_HEADER_LEN);
            }
            READDIRPLUS => self.note_readdirplus(nodeid, reply),
            RENAME => self.note_rename(nodeid, request, RENAME_IN_LEN),
            RENAME2 => self.note_rename(nodeid, request, RENAME2_IN_LEN),
            _ => {}
        }
    }

    /// `parent/name` now has the nodeid the entry at `entry_offset` carries.
    fn note_entry(&self, parent: u64, name: &[u8], reply: &[u8], entry_offset: usize) {
        if entry_offset + ENTRY_OUT_LEN > reply.len() {
            return;
        }
        let nodeid = u64::from_le_bytes(reply[entry_offset..entry_offset + 8].try_into().unwrap());
        if nodeid == 0 {
            return;
        }
        let Some(parent_path) = self.table().by_node.get(&parent).cloned() else {
            return;
        };
        self.record(parent_path.join(OsStr::from_bytes(name)), nodeid);
    }

    /// A `READDIRPLUS` reply is a run of `(entry_out, dirent, name, padding)`.
    fn note_readdirplus(&self, dir: u64, reply: &[u8]) {
        let Some(dir_path) = self.table().by_node.get(&dir).cloned() else {
            return;
        };
        let mut offset = OUT_HEADER_LEN;
        while offset + ENTRY_OUT_LEN + DIRENT_LEN <= reply.len() {
            let nodeid = u64::from_le_bytes(reply[offset..offset + 8].try_into().unwrap());
            let namelen = u32::from_le_bytes(
                reply[offset + ENTRY_OUT_LEN + 16..offset + ENTRY_OUT_LEN + 20]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let name_start = offset + ENTRY_OUT_LEN + DIRENT_LEN;
            if namelen == 0 || name_start + namelen > reply.len() {
                return;
            }
            let name = &reply[name_start..name_start + namelen];
            // `.` and `..` name the directory itself and its parent, which the
            // table already has under their real paths.
            if nodeid != 0 && name != b"." && name != b".." {
                self.record(dir_path.join(OsStr::from_bytes(name)), nodeid);
            }
            // Directory entries are padded to eight bytes.
            let padded = (DIRENT_LEN + namelen).div_ceil(8) * 8;
            offset += ENTRY_OUT_LEN + padded;
        }
    }

    /// `RENAME`/`RENAME2`: `oldname` in `olddir` is now `newname` in `newdir`.
    fn note_rename(&self, olddir: u64, request: &[u8], body_len: usize) {
        let names_start = IN_HEADER_LEN + body_len;
        let Some(oldname) = nul_terminated(request, names_start) else {
            return;
        };
        let Some(newname) = nul_terminated(request, names_start + oldname.len() + 1) else {
            return;
        };
        let newdir_offset = IN_HEADER_LEN + body_len - 8;
        let newdir = u64::from_le_bytes(
            request[newdir_offset..newdir_offset + 8]
                .try_into()
                .unwrap(),
        );
        // One lock acquisition: the table's mutex is not reentrant, and two
        // lookups in one expression would each hold a guard of their own.
        let (old_parent, new_parent) = {
            let table = self.table();
            let Some(old_parent) = table.by_node.get(&olddir).cloned() else {
                return;
            };
            let Some(new_parent) = table.by_node.get(&newdir).cloned() else {
                return;
            };
            (old_parent, new_parent)
        };
        let from = old_parent.join(OsStr::from_bytes(oldname));
        let to = new_parent.join(OsStr::from_bytes(newname));
        self.move_subtree(&from, &to);
    }

    /// Re-key everything under `from` onto `to`, so a subtree the guest moved
    /// keeps resolving to the same nodeids.
    fn move_subtree(&self, from: &Path, to: &Path) {
        let mut table = self.table();
        let keys: Vec<PathBuf> = table
            .by_path
            .keys()
            .filter(|path| path.starts_with(from))
            .cloned()
            .collect();
        for key in keys {
            let Some(nodeid) = table.by_path.remove(&key) else {
                continue;
            };
            let moved = match key.strip_prefix(from) {
                Ok(suffix) if suffix.as_os_str().is_empty() => to.to_path_buf(),
                Ok(suffix) => to.join(suffix),
                Err(_) => continue,
            };
            table.by_node.insert(nodeid, moved.clone());
            table.by_path.insert(moved, nodeid);
        }
    }
}

/// The NUL-terminated string at `start`, if there is one.
fn nul_terminated(bytes: &[u8], start: usize) -> Option<&[u8]> {
    let rest = bytes.get(start..)?;
    let end = rest.iter().position(|&byte| byte == 0)?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; IN_HEADER_LEN + body.len()];
        let total = frame.len() as u32;
        frame[0..4].copy_from_slice(&total.to_le_bytes());
        frame[4..8].copy_from_slice(&opcode.to_le_bytes());
        frame[8..16].copy_from_slice(&1u64.to_le_bytes());
        frame[16..24].copy_from_slice(&nodeid.to_le_bytes());
        frame[IN_HEADER_LEN..].copy_from_slice(body);
        frame
    }

    fn named_request(opcode: u32, nodeid: u64, prefix: &[u8], name: &[u8]) -> Vec<u8> {
        let mut body = prefix.to_vec();
        body.extend_from_slice(name);
        body.push(0);
        request(opcode, nodeid, &body)
    }

    fn entry_reply(nodeid: u64, error: i32) -> Vec<u8> {
        let mut reply = vec![0u8; OUT_HEADER_LEN + ENTRY_OUT_LEN];
        reply[0..4].copy_from_slice(&((OUT_HEADER_LEN + ENTRY_OUT_LEN) as u32).to_le_bytes());
        reply[4..8].copy_from_slice(&error.to_le_bytes());
        reply[OUT_HEADER_LEN..OUT_HEADER_LEN + 8].copy_from_slice(&nodeid.to_le_bytes());
        reply
    }

    #[test]
    fn only_the_root_is_known_before_the_relay_answers_anything() {
        let nodeids = NodeIds::new();
        assert_eq!(nodeids.nodeid(Path::new("")), Some(ROOT_NODEID));
        assert_eq!(nodeids.nodeid(Path::new("src/main.rs")), None);
        assert_eq!(nodeids.entry(Path::new("src/main.rs")), None);
        assert_eq!(nodeids.len(), 1);
    }

    #[test]
    fn a_lookup_reply_registers_the_child_of_its_parent() {
        let nodeids = NodeIds::new();
        nodeids.note_reply(
            &named_request(LOOKUP, ROOT_NODEID, b"", b"src"),
            &entry_reply(11, 0),
        );
        assert_eq!(nodeids.nodeid(Path::new("src")), Some(11));
        // ...and then a lookup *inside* it, which needs the parent's path.
        nodeids.note_reply(
            &named_request(LOOKUP, 11, b"", b"main.rs"),
            &entry_reply(12, 0),
        );
        assert_eq!(nodeids.nodeid(Path::new("src/main.rs")), Some(12));
        assert_eq!(
            nodeids.entry(Path::new("src/main.rs")),
            Some((11, b"main.rs".to_vec()))
        );
        assert_eq!(nodeids.len(), 3);
    }

    #[test]
    fn create_and_mkdir_register_their_new_entry() {
        let nodeids = NodeIds::new();
        nodeids.note_reply(
            &named_request(CREATE, ROOT_NODEID, &[0u8; CREATE_IN_LEN], b"new.txt"),
            &entry_reply(20, 0),
        );
        assert_eq!(nodeids.nodeid(Path::new("new.txt")), Some(20));
        nodeids.note_reply(
            &named_request(MKDIR, ROOT_NODEID, &[0u8; MKDIR_IN_LEN], b"out"),
            &entry_reply(21, 0),
        );
        assert_eq!(nodeids.nodeid(Path::new("out")), Some(21));
    }

    #[test]
    fn a_readdirplus_reply_registers_every_entry_it_lists() {
        let nodeids = NodeIds::new();
        let mut reply = vec![0u8; OUT_HEADER_LEN];
        let mut push = |name: &[u8], nodeid: u64| {
            let padded_name = name.len().div_ceil(8) * 8;
            let mut entry = vec![0u8; ENTRY_OUT_LEN + DIRENT_LEN + padded_name];
            entry[0..8].copy_from_slice(&nodeid.to_le_bytes());
            entry[ENTRY_OUT_LEN + 16..ENTRY_OUT_LEN + 20]
                .copy_from_slice(&(name.len() as u32).to_le_bytes());
            entry[ENTRY_OUT_LEN + DIRENT_LEN..ENTRY_OUT_LEN + DIRENT_LEN + name.len()]
                .copy_from_slice(name);
            reply.extend_from_slice(&entry);
        };
        push(b".", ROOT_NODEID);
        push(b"..", ROOT_NODEID);
        push(b"crates", 30);
        push(b"Cargo.toml", 31);
        let total = reply.len() as u32;
        reply[0..4].copy_from_slice(&total.to_le_bytes());
        nodeids.note_reply(&request(READDIRPLUS, ROOT_NODEID, &[0u8; 24]), &reply);

        assert_eq!(nodeids.nodeid(Path::new("crates")), Some(30));
        assert_eq!(nodeids.nodeid(Path::new("Cargo.toml")), Some(31));
        // `.` and `..` must not create entries called "." or "..".
        assert_eq!(nodeids.nodeid(Path::new(".")), None);
        assert_eq!(nodeids.nodeid(Path::new("..")), None);
        assert_eq!(nodeids.len(), 3);
    }

    #[test]
    fn an_error_reply_registers_nothing() {
        let nodeids = NodeIds::new();
        nodeids.note_reply(
            &named_request(LOOKUP, ROOT_NODEID, b"", b"gone"),
            &entry_reply(99, -libc::ENOENT),
        );
        assert_eq!(nodeids.nodeid(Path::new("gone")), None);
    }

    #[test]
    fn a_rename_moves_the_subtree_it_names() {
        let nodeids = NodeIds::new();
        nodeids.note_reply(
            &named_request(MKDIR, ROOT_NODEID, &[0u8; MKDIR_IN_LEN], b"old"),
            &entry_reply(40, 0),
        );
        nodeids.note_reply(
            &named_request(LOOKUP, 40, b"", b"inner"),
            &entry_reply(41, 0),
        );
        nodeids.note_reply(
            &named_request(MKDIR, ROOT_NODEID, &[0u8; MKDIR_IN_LEN], b"new"),
            &entry_reply(42, 0),
        );
        // RENAME: newdir (root), then "old\0new\0".
        let mut body = vec![0u8; RENAME_IN_LEN];
        body[0..8].copy_from_slice(&ROOT_NODEID.to_le_bytes());
        body.extend_from_slice(b"old\0old-renamed\0");
        nodeids.note_reply(&request(RENAME, ROOT_NODEID, &body), &entry_reply(0, 0));

        assert_eq!(nodeids.nodeid(Path::new("old")), None);
        assert_eq!(nodeids.nodeid(Path::new("old-renamed")), Some(40));
        assert_eq!(nodeids.nodeid(Path::new("old-renamed/inner")), Some(41));
    }
}
