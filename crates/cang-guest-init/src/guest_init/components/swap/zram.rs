//! Guest swap on a zram device.
//!
//! Guest RAM is fixed for the life of the VM: libkrun has no balloon inflate
//! (the attached balloon only lets the host reclaim pages the guest has already
//! freed) and the pinned guest kernel has no virtio-mem, so a workload that
//! outgrows `--mem` can only be relieved by putting cold anonymous pages
//! somewhere else.
//! Without swap the guest OOM killer is the first responder, and because the
//! image entrypoint is a child of libkrun's init rather than PID 1, losing it
//! reboots the microVM. A zram device gives the kernel a RAM-backed device that
//! stores those pages compressed, which turns a sudden VM death into slower
//! progress.
//!
//! The pinned libkrunfw kernel is built with `CONFIG_SWAP` and `CONFIG_ZRAM`.
//! A kernel without them reports `state=unavailable` and leaves the guest
//! alone, because guest swap is a resilience feature rather than a boot
//! requirement.

use anyhow::{Context, Result, anyhow};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::guest_init::command;
use crate::guest_init::components::env::SWAP_STATUS_PATH;
use crate::guest_init::fs as guest_fs;

/// Swap device the zram driver exposes once the kernel has it built in.
const DEVICE: &str = "/dev/zram0";
/// zram is faster than any disk-backed swap device, so it goes first.
const SWAP_PRIORITY: &str = "100";
/// Swap capacity as a percentage of guest RAM.
///
/// The capacity counts uncompressed bytes, and zram keeps a page resident in
/// compressed form, so this is the ceiling for pages the guest can park in
/// swap rather than the amount of memory the device costs.
const SWAP_CAPACITY_PERCENT: u64 = 100;
/// Compressed-memory budget for the device as a percentage of guest RAM.
///
/// zram stores an incompressible page uncompressed, so without a budget the
/// device would spend guest RAM at roughly 1:1 on such pages, buying nothing
/// while pushing the guest into thrash instead of a prompt failure. Capping
/// the budget keeps that cost bounded; once the cap is reached the device
/// refuses further pages and the kernel falls back to its normal reclaim
/// behaviour.
const SWAP_MEM_LIMIT_PERCENT: u64 = 25;
/// Floor so a small `--mem` still gets a device worth having.
const MIN_SWAP_BYTES: u64 = 256 * 1024 * 1024;
/// zram accepts a size only while the device is unused; the budget can change
/// at any time, including after the device is active.
const DEVICE_SIZE_PATH: &str = "/sys/block/zram0/disksize";
const MEM_LIMIT_PATH: &str = "/sys/block/zram0/mem_limit";
const SWAPS_PATH: &str = "/proc/swaps";
const MEMINFO_PATH: &str = "/proc/meminfo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuestSwapState {
    /// The device is active: this call activated it, or an earlier one did.
    Ready,
    /// The booted kernel has no zram device to offer the guest.
    Unavailable,
    /// zram exists but could not be activated.
    Failed,
}

impl GuestSwapState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestSwapReport {
    state: GuestSwapState,
    size_bytes: Option<u64>,
    mem_limit_bytes: Option<u64>,
    error: Option<String>,
}

/// Adds guest swap if the booted kernel offers a zram device.
///
/// Never fails the session: swap is worth having, but a guest without it must
/// still boot. The outcome is recorded in `SWAP_STATUS_PATH` and, when the
/// device could not be activated, written to guest-init stderr.
pub(in crate::guest_init) fn ensure() {
    ensure_with(&SystemSwapBackend);
}

fn ensure_with(backend: &impl SwapBackend) {
    let report = match activate_with(backend) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("cang-guest-init: guest zram swap not active: {err:#}");
            GuestSwapReport::failed(&err)
        }
    };
    backend.record_status(&report.render());
}

fn activate_with(backend: &impl SwapBackend) -> Result<GuestSwapReport> {
    if !backend.path_exists(Path::new(DEVICE_SIZE_PATH)) {
        return Ok(GuestSwapReport::unavailable());
    }

    let swaps = backend
        .read(Path::new(SWAPS_PATH))
        .context("failed to read active swap devices")?;
    if swap_active(&swaps, DEVICE) {
        // An active device is left exactly as it is; the report reads back the
        // geometry whoever configured it, so the status file stays truthful.
        return Ok(GuestSwapReport::ready(
            read_size(backend, DEVICE_SIZE_PATH),
            read_size(backend, MEM_LIMIT_PATH),
        ));
    }

    let meminfo = backend
        .read(Path::new(MEMINFO_PATH))
        .context("failed to read guest memory size before sizing swap")?;
    let mem_total_bytes = parse_mem_total(&meminfo)?;
    let size_bytes = plan_swap_capacity(mem_total_bytes);
    let mem_limit_bytes = plan_mem_limit(mem_total_bytes);

    backend
        .write(Path::new(DEVICE_SIZE_PATH), &format!("{size_bytes}\n"))
        .with_context(|| format!("failed to set the {DEVICE} size to {size_bytes} bytes"))?;
    backend
        .write(Path::new(MEM_LIMIT_PATH), &format!("{mem_limit_bytes}\n"))
        .with_context(|| {
            format!("failed to bound the {DEVICE} memory use to {mem_limit_bytes} bytes")
        })?;
    backend
        .run("mkswap", &[DEVICE])
        .with_context(|| format!("failed to write a swap signature to {DEVICE}"))?;
    backend
        .run("swapon", &["-p", SWAP_PRIORITY, DEVICE])
        .with_context(|| format!("failed to activate {DEVICE} as swap"))?;
    Ok(GuestSwapReport::ready(
        Some(size_bytes),
        Some(mem_limit_bytes),
    ))
}

/// A byte count the kernel reports in one of the device's sysfs attributes.
fn read_size(backend: &impl SwapBackend, path: &str) -> Option<u64> {
    backend
        .read(Path::new(path))
        .ok()
        .and_then(|value| value.trim().parse().ok())
}

/// Swap capacity for a guest with this much RAM, floored so a small `--mem`
/// still gets a device worth having.
///
/// Sized to guest RAM rather than to a fraction of it: the capacity is only
/// reachable by pages the device can compress, which cost a fraction of the
/// space they occupy, and [`plan_mem_limit`] bounds what the device may spend.
fn plan_swap_capacity(mem_total_bytes: u64) -> u64 {
    percent_of(mem_total_bytes, SWAP_CAPACITY_PERCENT).max(MIN_SWAP_BYTES)
}

/// Compressed-memory budget for the device, which always stays below the
/// capacity returned by [`plan_swap_capacity`].
fn plan_mem_limit(mem_total_bytes: u64) -> u64 {
    percent_of(mem_total_bytes, SWAP_MEM_LIMIT_PERCENT)
}

fn percent_of(mem_total_bytes: u64, percent: u64) -> u64 {
    mem_total_bytes * percent / 100
}

fn parse_mem_total(meminfo: &str) -> Result<u64> {
    let kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| anyhow!("{MEMINFO_PATH} has no parseable MemTotal line"))?;
    Ok(kib * 1024)
}

/// `/proc/swaps` lists the active devices in its first column, header aside.
fn swap_active(swaps: &str, device: &str) -> bool {
    swaps
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .any(|path| path == device)
}

impl GuestSwapReport {
    fn ready(size_bytes: Option<u64>, mem_limit_bytes: Option<u64>) -> Self {
        Self {
            state: GuestSwapState::Ready,
            size_bytes,
            mem_limit_bytes,
            error: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            state: GuestSwapState::Unavailable,
            size_bytes: None,
            mem_limit_bytes: None,
            error: None,
        }
    }

    fn failed(error: &anyhow::Error) -> Self {
        Self {
            state: GuestSwapState::Failed,
            size_bytes: None,
            mem_limit_bytes: None,
            error: Some(format!("{error:#}")),
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "state={}", self.state.as_str());
        let _ = writeln!(out, "device={DEVICE}");
        if let Some(size_bytes) = self.size_bytes {
            let _ = writeln!(out, "size_bytes={size_bytes}");
        }
        if let Some(mem_limit_bytes) = self.mem_limit_bytes {
            let _ = writeln!(out, "mem_limit_bytes={mem_limit_bytes}");
        }
        let _ = writeln!(out, "priority={SWAP_PRIORITY}");
        if let Some(error) = &self.error {
            let _ = writeln!(out, "error={error}");
        }
        out
    }
}

trait SwapBackend {
    fn path_exists(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, value: &str) -> Result<()>;
    fn run(&self, program: &str, args: &[&str]) -> Result<()>;
    fn record_status(&self, contents: &str);
}

struct SystemSwapBackend;

impl SwapBackend for SystemSwapBackend {
    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read(&self, path: &Path) -> Result<String> {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
    }

    fn write(&self, path: &Path, value: &str) -> Result<()> {
        fs::write(path, value).with_context(|| format!("failed to write {}", path.display()))
    }

    fn run(&self, program: &str, args: &[&str]) -> Result<()> {
        command::run(program, args)
    }

    /// Reporting must never fail a session, so a failed status write is dropped.
    fn record_status(&self, contents: &str) {
        let _ = guest_fs::write_file(Path::new(SWAP_STATUS_PATH), contents, 0o644);
    }
}

#[cfg(test)]
#[path = "zram_tests.rs"]
mod tests;
