//! The recursive inotify watcher: a guest-side write becomes a FUSE
//! invalidation for the host's mount.
//!
//! One `inotify` instance for the whole served tree, one watch per directory
//! (note 66 measured 1 374 directories in this repository, 3 641 with a
//! populated `target/`, against the guest's 122 262-watch limit). A directory
//! created or moved into the tree is watched recursively as soon as the event
//! names it, and a directory the guest moves keeps its watches (inotify follows
//! the inode) while its recorded path is rewritten, so an event under it still
//! resolves to a real path.
//!
//! What each event becomes:
//!
//! * `IN_MODIFY`/`IN_CLOSE_WRITE` on a file -> `INVAL_INODE` (`off = 0`,
//!   `len = 0`) for the nodeid the relay's own replies gave that path. That is
//!   the content lever (note 66): it drops the pages of a host descriptor that
//!   is already open, which the 5 s attribute timeout cannot do for a same-size
//!   rewrite.
//! * `IN_CREATE`/`IN_DELETE`/`IN_MOVED_FROM`/`IN_MOVED_TO` are classified and
//!   counted, and **deliberately not sent**. `INVAL_ENTRY` was the plan (a
//!   revalidation of `(parent nodeid, name)`), and it is the one notification
//!   kind that can hang this mount: the kernel's `fuse_reverse_inval_entry`
//!   takes the parent directory's inode lock (and may wait for an inode a host
//!   lookup is still creating), while the pump is the *only* server on the
//!   connection - so a host task holding that lock and waiting for the reply the
//!   pump owes deadlocks it. Measured live: the pump's write blocked in
//!   `fuse_reverse_inval_entry` (`/proc/<pid>/wchan`), `stat` through the mount
//!   never returned, and the frame never completed; the same run's `INVAL_INODE`
//!   frames all went out in ~1 ms, and note 66's 1 000-frame burst never blocked.
//!   Nothing is lost for the case this feature exists for: note 66 measured
//!   `INVAL_ENTRY` as *not* refreshing an open descriptor's pages (4.962-4.967 s,
//!   i.e. no help), and a stale name is a 5 s entry-timeout matter, not a
//!   content-staleness one. `inval` still carries the frame builder, so the
//!   mechanism and its rejection stay documented and testable; the watcher
//!   sends only `INVAL_INODE`.
//!
//! Coalescing is per path **within one drain batch** - everything readable
//! from the descriptor at one go - and the batch is flushed immediately (see
//! `inval`'s module documentation for why a time window is the wrong lever and
//! what the bound is).
//!
//! An event for a path the relay never sent a nodeid for is skipped: nothing of
//! that path is cached on the host's mount, so there is nothing to invalidate.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{CString, OsStr};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::inval::Notifier;
use crate::nodeids::NodeIds;

/// `struct inotify_event`'s fixed part: `wd`, `mask`, `cookie`, `len`.
const INOTIFY_EVENT_HEADER: usize = 16;
/// One batch is one `read(2)` of this much: 255 events carry a name each.
const EVENT_BATCH_BYTES: usize = 64 * 1024;
/// The poll tick: the loop's only reason to wake up when nothing happens, and
/// how quickly a stop is noticed.
const POLL_TICK: Duration = Duration::from_millis(100);
/// The watcher's own budget. The tree this serves is a workspace, and a
/// runaway directory set must not exhaust `fs.inotify.max_user_watches`
/// (122 262 in the guest, note 66 §4) for the rest of the image.
pub const MAX_WATCHES: usize = 32_768;
/// A summary line is printed at most this often, so a `target/` storm does not
/// turn the bridged guest console into the bottleneck.
const SUMMARY_INTERVAL: Duration = Duration::from_secs(1);

/// The guest environment variable that turns the watcher on. `cang` forwards it
/// into the guest environment from `CANG_WORKSPACE_RELAY_WATCH` (the same
/// spelling, one hop), which is how the OFF and ON arms of a measurement run the
/// same command line and the same image.
pub const WATCH_ENV: &str = "CANG_WORKSPACE_RELAY_WATCH";

/// Is the watcher asked for? `--watch` is the flag, `CANG_WORKSPACE_RELAY_WATCH`
/// the environment, and a truthy value is accepted the same way the host side
/// accepts it (`1`, `true`, `yes`, `on`) - not clap's bare `true`/`false`, so a
/// launch can set `1`.
pub fn watch_enabled(flag: bool, environment: Option<&str>) -> bool {
    flag || environment.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Where the watcher's log lines go: one `println!` per line, shared with the
/// relay's serve loop (which is why it is a callback and not a file).
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// What one drain batch holds.
#[derive(Default)]
struct Batch {
    /// Files whose content changed (a path the mount may hold pages for).
    inodes: BTreeSet<PathBuf>,
    /// `(parent directory path, entry name)` pairs.
    entries: BTreeSet<(PathBuf, Vec<u8>)>,
    /// Events read, before coalescing.
    events: u64,
}

impl Batch {
    fn is_empty(&self) -> bool {
        self.events == 0
    }
}

/// The watcher itself: one `inotify` instance plus the path table and the
/// notifier it emits through.
pub struct Watcher {
    inotify: RawFd,
    /// The served root: inotify reports absolute paths under it, while the
    /// nodeid table is keyed by paths relative to it.
    root: PathBuf,
    /// `wd -> absolute directory path` (a path inside the served tree).
    dirs: HashMap<i32, PathBuf>,
    /// `IN_MOVED_FROM` cooks that are waiting for their `IN_MOVED_TO`.
    moves: HashMap<u32, PathBuf>,
    nodeids: NodeIds,
    notifier: Notifier,
    log: Logger,
    events: u64,
    inodes: u64,
    entries: u64,
    skipped: u64,
    budget_reported: bool,
    started: Instant,
    last_summary: Instant,
}

impl Watcher {
    /// Create the instance and watch `root` recursively.
    ///
    /// The returned watcher has not read an event yet: [`Watcher::run`] is the
    /// loop, and [`Watcher::watch_count`] is what the relay logs at startup.
    pub fn new(
        root: &Path,
        nodeids: NodeIds,
        notifier: Notifier,
        log: Logger,
    ) -> std::io::Result<Self> {
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if inotify < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut watcher = Self {
            inotify,
            root: root.to_path_buf(),
            dirs: HashMap::new(),
            moves: HashMap::new(),
            nodeids,
            notifier,
            log,
            events: 0,
            inodes: 0,
            entries: 0,
            skipped: 0,
            budget_reported: false,
            started: Instant::now(),
            last_summary: Instant::now(),
        };
        watcher.watch_tree(root);
        Ok(watcher)
    }

    /// Directories currently watched.
    pub fn watch_count(&self) -> usize {
        self.dirs.len()
    }

    /// The loop: read events, coalesce one batch, flush it, repeat.
    ///
    /// It only returns when `stop` is set or the descriptor fails; a watcher is
    /// tied to the connection it emits through.
    pub fn run(&mut self, stop: &AtomicBool) {
        let mut batch = Batch::default();
        let mut buffer = vec![0u8; EVENT_BATCH_BYTES];
        while !stop.load(Ordering::Relaxed) {
            match self.wait_for_events(POLL_TICK) {
                Ok(true) => {}
                Ok(false) => {
                    continue;
                }
                Err(err) => {
                    (self.log)(&format!("relay: inotify wait failed: {err}"));
                    return;
                }
            }
            // Everything readable is one batch; draining stops at EAGAIN, which
            // is what bounds a storm's frames to one per path per batch.
            loop {
                match self.read_batch(&mut buffer, &mut batch) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(err) => {
                        (self.log)(&format!("relay: inotify read failed: {err}"));
                        return;
                    }
                }
            }
            if !batch.is_empty() {
                self.flush(&mut batch);
            }
        }
    }

    /// `true` when the descriptor has events, `false` on a plain timeout.
    fn wait_for_events(&self, timeout: Duration) -> std::io::Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.inotify,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let rc = unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as i32) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            return Ok(rc > 0 && fd.revents & libc::POLLIN != 0);
        }
    }

    /// Read one batch of events into `batch`, returning the bytes read (0 when
    /// the descriptor is drained).
    fn read_batch(&mut self, bytes: &mut [u8], batch: &mut Batch) -> std::io::Result<usize> {
        let read = unsafe { libc::read(self.inotify, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read < 0 {
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(libc::EAGAIN) | Some(libc::EINTR) => Ok(0),
                _ => Err(err),
            };
        }
        if read == 0 {
            return Ok(0);
        }
        self.handle_events(&bytes[..read as usize], batch);
        Ok(read as usize)
    }

    /// Turn one read's raw events into the batch's set of paths and entries.
    fn handle_events(&mut self, bytes: &[u8], batch: &mut Batch) {
        let mut offset = 0;
        while offset + INOTIFY_EVENT_HEADER <= bytes.len() {
            let wd = i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let mask = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let cookie = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
            let len =
                u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()) as usize;
            let name_start = offset + INOTIFY_EVENT_HEADER;
            let name_end = name_start.saturating_add(len).min(bytes.len());
            let name = trim_zeroes(&bytes[name_start.min(bytes.len())..name_end]);
            offset = name_start.saturating_add(len);
            batch.events += 1;

            if mask & libc::IN_Q_OVERFLOW != 0 {
                (self.log)("relay: inotify queue overflow: events were lost");
                continue;
            }
            let Some(dir) = self.dirs.get(&wd).cloned() else {
                // IN_IGNORED arrives after the watch is gone; anything else for
                // an unknown wd is a watch that was never ours.
                if mask & libc::IN_IGNORED != 0 {
                    self.dirs.remove(&wd);
                }
                continue;
            };
            if mask & libc::IN_IGNORED != 0 {
                self.dirs.remove(&wd);
                continue;
            }
            if name.is_empty() {
                continue;
            }
            let path = dir.join(Path::new(OsStr::from_bytes(name)));

            if mask & libc::IN_ISDIR != 0 {
                if mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    // A directory that appeared inside the tree: watch it, and
                    // everything under it, before misses accumulate.
                    self.watch_tree(&path);
                }
                if mask & libc::IN_MOVED_FROM != 0 {
                    // A move out of the tree never gets its MOVED_TO here, so the
                    // pairing table is pruned rather than trusted to empty.
                    if self.moves.len() > 1024 {
                        self.moves.clear();
                    }
                    self.moves.insert(cookie, path.clone());
                }
                if mask & libc::IN_MOVED_TO != 0 {
                    // The watches follow the inode; only their recorded paths
                    // are stale. Pair with the MOVED_FROM that named the old
                    // path, and rewrite that subtree.
                    if let Some(old) = self.moves.remove(&cookie) {
                        self.rename_watched(&old, &path);
                    }
                }
                batch.entries.insert((dir, name.to_vec()));
                continue;
            }

            if mask & (libc::IN_MODIFY | libc::IN_CLOSE_WRITE) != 0 {
                batch.inodes.insert(path);
            }
            if mask & (libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO)
                != 0
            {
                batch.entries.insert((dir, name.to_vec()));
            }
        }
    }

    /// Emit the batch's invalidations, coalesced per path.
    fn flush(&mut self, batch: &mut Batch) {
        let inode_paths = std::mem::take(&mut batch.inodes);
        let entry_pairs = std::mem::take(&mut batch.entries);
        let events = batch.events;
        batch.events = 0;

        for path in &inode_paths {
            // The table is keyed from the served root; inotify reports absolute
            // paths, so the root prefix is what turns one into the other.
            let Some(relative) = path.strip_prefix(&self.root).ok() else {
                self.skipped += 1;
                continue;
            };
            match self.nodeids.nodeid(relative) {
                Some(nodeid) => {
                    if let Err(err) = self.notifier.invalidate_inode(nodeid) {
                        (self.log)(&format!("relay: invalidation write failed: {err}"));
                        return;
                    }
                    self.inodes += 1;
                }
                // Nothing of this file is cached on the host's mount.
                None => self.skipped += 1,
            }
        }
        // Entry events are counted, not sent: `INVAL_ENTRY` can deadlock this
        // single-server mount (see the module documentation). The count stays in
        // the summary line so the cost of the decision is visible.
        self.entries += entry_pairs.len() as u64;
        self.events += events;
        if self.last_summary.elapsed() >= SUMMARY_INTERVAL {
            self.last_summary = Instant::now();
            let line = self.summary_line();
            (self.log)(&line);
        }
    }

    fn summary_line(&self) -> String {
        format!(
            "relay: watch events={} invalidated_inodes={} entry_events_not_sent={} skipped={} dirs={} elapsed_ms={}",
            self.events,
            self.inodes,
            self.entries,
            self.skipped,
            self.dirs.len(),
            self.started.elapsed().as_millis()
        )
    }

    /// Add one watch, if the budget allows.
    fn watch_dir(&mut self, dir: &Path) -> bool {
        if self.dirs.len() >= MAX_WATCHES {
            if !self.budget_reported {
                self.budget_reported = true;
                (self.log)(&format!(
                    "relay: watch budget reached ({MAX_WATCHES} directories): deeper writes will not be invalidated"
                ));
            }
            return false;
        }
        let Ok(path) = CString::new(dir.as_os_str().as_encoded_bytes()) else {
            return false;
        };
        let wd = unsafe {
            libc::inotify_add_watch(
                self.inotify,
                path.as_ptr(),
                libc::IN_MODIFY
                    | libc::IN_CLOSE_WRITE
                    | libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO,
            )
        };
        if wd < 0 {
            let err = std::io::Error::last_os_error();
            // A tree being churned can have a directory removed between the
            // readdir and the watch; that is not an error worth stopping for.
            if !matches!(err.raw_os_error(), Some(libc::ENOENT) | Some(libc::ENOTDIR)) {
                (self.log)(&format!("relay: cannot watch '{}': {err}", dir.display()));
            }
            return false;
        }
        self.dirs.insert(wd, dir.to_path_buf());
        true
    }

    /// Watch `root` and every directory under it, breadth-first through
    /// `read_dir` without following symlinks.
    fn watch_tree(&mut self, root: &Path) {
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            if self.dirs.len() >= MAX_WATCHES {
                self.watch_dir(&dir); // reports the budget once, then returns
                return;
            }
            if !self.watch_dir(&dir) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                // `file_type()` does not follow a symlink: a link to a
                // directory is not watched (and cannot loop the walk).
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(dir.join(entry.file_name()));
                }
            }
        }
    }

    /// Re-key the watches under `from` onto `to` after a directory move.
    fn rename_watched(&mut self, from: &Path, to: &Path) {
        for path in self.dirs.values_mut() {
            if let Ok(suffix) = path.strip_prefix(from) {
                let moved = match suffix.as_os_str().is_empty() {
                    true => to.to_path_buf(),
                    false => to.join(suffix),
                };
                *path = moved;
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.inotify) };
    }
}

/// A running watcher thread, tied to the connection it emits through.
pub struct WatchThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WatchThread {
    /// Start the watcher and log what it watches.
    pub fn start(
        root: &Path,
        nodeids: NodeIds,
        notifier: Notifier,
        log: Logger,
    ) -> std::io::Result<Self> {
        let logger = Arc::clone(&log);
        let mut watcher = Watcher::new(root, nodeids, notifier, log)?;
        let watched = watcher.watch_count();
        (logger)(&format!(
            "relay: watching '{}' with {watched} inotify watches",
            root.display()
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("cang-relay-watch".to_owned())
            .spawn(move || {
                watcher.run(&thread_stop);
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    /// Ask the loop to stop and wait for it (it notices at the next poll tick).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WatchThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn trim_zeroes(name: &[u8]) -> &[u8] {
    match name.iter().position(|&byte| byte == 0) {
        Some(end) => &name[..end],
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inval::SharedWriter;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn frame_header(frame: &[u8]) -> (u32, i32) {
        (
            u32::from_le_bytes(frame[0..4].try_into().unwrap()),
            i32::from_le_bytes(frame[4..8].try_into().unwrap()),
        )
    }

    /// A watcher over a temp directory, with the notifier's connection in the
    /// test's hands: `(watcher thread, peer stream, stop)`.
    fn watcher_over(
        root: &Path,
        nodeids: NodeIds,
    ) -> (WatchThread, UnixStream, Arc<std::sync::atomic::AtomicU64>) {
        let (ours, peer) = UnixStream::pair().expect("pair");
        let notifier = Notifier::new(SharedWriter::new(ours));
        let logged = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&logged);
        let thread = WatchThread::start(
            root,
            nodeids,
            notifier,
            Arc::new(move |_line| {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("watcher");
        (thread, peer, logged)
    }

    fn read_frames(peer: &mut UnixStream, wait: Duration) -> Vec<Vec<u8>> {
        use std::io::Read;
        let mut frames = Vec::new();
        let deadline = Instant::now() + wait;
        loop {
            peer.set_read_timeout(Some(Duration::from_millis(20)))
                .expect("timeout");
            let mut head = [0u8; 4];
            match peer.read_exact(&mut head) {
                Ok(()) => {}
                Err(_) => {
                    if Instant::now() >= deadline {
                        return frames;
                    }
                    continue;
                }
            }
            let total = u32::from_le_bytes(head) as usize;
            let mut frame = head.to_vec();
            frame.resize(total, 0);
            peer.read_exact(&mut frame[4..total]).expect("body");
            frames.push(frame);
        }
    }

    /// A guest-side rewrite of a file the relay has answered a LOOKUP for is one
    /// `INVAL_INODE off=0 len=0` for that file's nodeid.
    #[test]
    fn a_content_write_becomes_one_inval_inode_for_its_nodeid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("hello.txt");
        std::fs::write(&file, b"one").expect("file");
        let nodeids = NodeIds::new();
        nodeids.record(PathBuf::from("hello.txt"), 77);
        let (thread, mut peer, _logged) = watcher_over(dir.path(), nodeids);

        // Let the watch exist before the write.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&file, b"two").expect("rewrite");

        let frames = read_frames(&mut peer, Duration::from_millis(500));
        assert!(
            frames.iter().any(|frame| {
                let (len, error) = frame_header(frame);
                len == 40
                    && error == 2
                    && u64::from_le_bytes(frame[16..24].try_into().unwrap()) == 77
            }),
            "expected an INVAL_INODE for nodeid 77, got {frames:?}"
        );
        thread.stop();
    }

    /// A file created inside the tree is classified as an entry event and
    /// **not** sent (see the module documentation: it deadlocks the mount), and a
    /// path the relay never answered for is skipped instead of emitted.
    #[test]
    fn a_new_file_sends_no_inval_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nodeids = NodeIds::new();
        nodeids.record(PathBuf::new(), 1);
        let (thread, mut peer, _logged) = watcher_over(dir.path(), nodeids);

        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("fresh.txt"), b"new").expect("create");

        let frames = read_frames(&mut peer, Duration::from_millis(500));
        assert!(
            frames.iter().all(|frame| frame_header(frame).1 != 3),
            "no INVAL_ENTRY may be written: fuse_reverse_inval_entry waits on the              parent's inode lock and the pump is the only server, so it deadlocks              the mount; got {frames:?}"
        );
        // The created file itself has no nodeid (the relay never answered a
        // lookup for it), so its content event is skipped rather than
        // invalidating a nodeid the mount never held.
        thread.stop();
    }

    /// The knob: a flag, or a truthy environment value (`1`, which is what a
    /// launch sets), and nothing else.
    #[test]
    fn the_watch_knob_accepts_a_flag_or_a_truthy_value() {
        assert!(!watch_enabled(false, None));
        assert!(watch_enabled(true, None));
        assert!(watch_enabled(false, Some("1")));
        assert!(watch_enabled(false, Some(" ON ")));
        assert!(!watch_enabled(false, Some("0")));
        assert!(!watch_enabled(false, Some("false")));
        assert!(!watch_enabled(false, Some("")));
        assert_eq!(WATCH_ENV, "CANG_WORKSPACE_RELAY_WATCH");
    }

    /// The watcher's idle cost is a poll tick, not a spin: a second of doing
    /// nothing emits nothing.
    #[test]
    fn an_idle_tree_emits_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("still.txt"), b"still").expect("file");
        let (thread, mut peer, _logged) = watcher_over(dir.path(), NodeIds::new());
        std::thread::sleep(Duration::from_millis(1100));
        let frames = read_frames(&mut peer, Duration::from_millis(100));
        assert!(frames.is_empty(), "idle watcher emitted {frames:?}");
        thread.stop();
    }
}
