//! Guest-side release of reclaimable page cache for host memory pressure.
//!
//! Guest RAM is a fixed anonymous mapping for the life of the VM, so the host
//! cannot simply take memory back from a guest the way it would from one of its
//! own processes. A guest that is idle but holding a large page cache - the
//! image's `/nix` store, `/workspace` over virtiofs - keeps that memory away
//! from the host at exactly the moment the host starts swapping.
//!
//! libkrun's virtio-balloon has no inflate path, so no host-side request can
//! reclaim those pages. What returns them is the guest freeing them: the
//! balloon's free-page reporting then hands each reported run to the host
//! kernel with `MADV_DONTNEED`. This module is the guest half of that exchange -
//! the host's pressure watcher sends `ReclaimMemory` over the exec control
//! channel, this drops the clean page cache, and the reply says how much
//! `MemFree` grew so the host can log what the release actually bought.
//!
//! Two deliberate limits: `drop_caches` never discards dirty pages, so a guest
//! in the middle of a write keeps its data, and only the page cache goes, not
//! the dentry/inode slabs, whose rebuild cost outlives the pressure that
//! triggered the release.
//!
//! The guest's own answer to running out of memory is separate: see
//! [`crate::guest_init::components::swap`].

use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::Path;

const MEMINFO_PATH: &str = "/proc/meminfo";
const DROP_CACHES_PATH: &str = "/proc/sys/vm/drop_caches";
/// The `drop_caches` argument that selects clean page cache only.
const CLEAN_PAGE_CACHE: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) struct ReclaimReport {
    /// Growth of `MemFree`, in KiB, produced by the release.
    pub(in crate::guest_init) freed_kib: u64,
}

/// The two kernel interfaces a release touches, split out so tests can see the
/// accounting without writing a sysctl.
trait ReclaimBackend {
    fn free_kib(&self) -> Result<u64>;
    fn drop_clean_page_cache(&self) -> Result<()>;
}

struct SystemBackend;

impl ReclaimBackend for SystemBackend {
    fn free_kib(&self) -> Result<u64> {
        read_free_kib(Path::new(MEMINFO_PATH))
    }

    fn drop_clean_page_cache(&self) -> Result<()> {
        // Writing the sysctl frees the cache synchronously, so the caller can
        // read `MemFree` immediately afterwards and see the result.
        fs::write(DROP_CACHES_PATH, CLEAN_PAGE_CACHE).with_context(|| {
            format!(
                "failed to write {DROP_CACHES_PATH}; releasing host memory needs root in the guest"
            )
        })
    }
}

/// Releases the guest's clean page cache and reports what it freed.
///
/// Fails when the sysctl is unreadable or unwritable, which is the guest
/// saying it cannot help; the caller reports that instead of pretending the
/// release happened.
pub(in crate::guest_init) fn reclaim_clean_page_cache() -> Result<ReclaimReport> {
    reclaim_with(&SystemBackend)
}

fn reclaim_with(backend: &impl ReclaimBackend) -> Result<ReclaimReport> {
    let free_before_kib = backend.free_kib()?;
    backend.drop_clean_page_cache()?;
    let free_after_kib = backend.free_kib()?;
    Ok(ReclaimReport {
        freed_kib: free_after_kib.saturating_sub(free_before_kib),
    })
}

fn read_free_kib(path: &Path) -> Result<u64> {
    let meminfo =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_free_kib(&meminfo)
}

fn parse_free_kib(meminfo: &str) -> Result<u64> {
    let line = meminfo
        .lines()
        .find(|line| line.starts_with("MemFree:"))
        .ok_or_else(|| anyhow!("{MEMINFO_PATH} has no MemFree line"))?;
    let value = line
        .trim_start_matches("MemFree:")
        .trim()
        .strip_suffix("kB")
        .ok_or_else(|| anyhow!("{MEMINFO_PATH} MemFree is not reported in kB"))?
        .trim();
    value
        .parse::<u64>()
        .with_context(|| format!("{MEMINFO_PATH} MemFree '{value}' is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn parses_mem_free_from_meminfo() {
        let meminfo = "MemTotal:       16384000 kB\nMemFree:          524288 kB\nMemAvailable:    4096000 kB\n";
        assert_eq!(parse_free_kib(meminfo).unwrap(), 524_288);
    }

    #[test]
    fn rejects_meminfo_without_mem_free() {
        let err = parse_free_kib("MemTotal: 16384000 kB\n").unwrap_err();
        assert!(format!("{err:#}").contains("no MemFree line"));
    }

    #[test]
    fn rejects_mem_free_that_is_not_a_number() {
        let err = parse_free_kib("MemFree: plenty kB\n").unwrap_err();
        assert!(format!("{err:#}").contains("not a number"));
    }

    struct FakeBackend {
        free_kib: RefCell<Vec<u64>>,
        dropped: RefCell<u32>,
    }

    impl ReclaimBackend for FakeBackend {
        fn free_kib(&self) -> Result<u64> {
            let mut values = self.free_kib.borrow_mut();
            if values.len() > 1 {
                return Ok(values.remove(0));
            }
            Ok(values[0])
        }

        fn drop_clean_page_cache(&self) -> Result<()> {
            *self.dropped.borrow_mut() += 1;
            Ok(())
        }
    }

    #[test]
    fn reports_the_mem_free_growth_the_release_produced() {
        let backend = FakeBackend {
            free_kib: RefCell::new(vec![100_000, 900_000]),
            dropped: RefCell::new(0),
        };
        let report = reclaim_with(&backend).unwrap();
        assert_eq!(report.freed_kib, 800_000);
        assert_eq!(*backend.dropped.borrow(), 1);
    }

    #[test]
    fn a_release_that_frees_nothing_reports_zero() {
        let backend = FakeBackend {
            free_kib: RefCell::new(vec![100_000, 90_000]),
            dropped: RefCell::new(0),
        };
        assert_eq!(reclaim_with(&backend).unwrap().freed_kib, 0);
    }
}
