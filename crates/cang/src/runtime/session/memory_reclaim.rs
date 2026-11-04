//! Host-side memory-pressure reclaim of guest memory.
//!
//! Guest RAM is one anonymous mapping, fixed for the life of the VM, and
//! libkrun's balloon only returns pages the guest has already freed: on Linux it
//! has no inflate path, so the host cannot ask a guest to give memory back. A
//! guest that is idle but still holding a page cache - the image's `/nix` store,
//! files read over virtiofs - therefore keeps that RAM out of the host's reach
//! at exactly the moment the host starts swapping.
//!
//! What returns those pages is the guest freeing them. This module watches the
//! host's own pressure and, when it is real, asks the guest to release its clean
//! page cache over the exec control channel (`Frame::ReclaimMemory`); the freed
//! pages come back to the host through the balloon's free-page reporting, which
//! `madvise(MADV_DONTNEED)`s them.
//!
//! The trigger is deliberately narrow. `/proc/pressure/memory`'s `full avg10`
//! counts only time that *every* runnable task was stalled, and a 10 s average
//! is already sustained pressure rather than a spike; the swap-out rate catches
//! a host that is swapping without stalling everything yet. Either fires, then a
//! cooldown keeps a host under lasting pressure from taking the guest's cache
//! back every second. `CANG_MEMORY_RECLAIM=0` turns the watcher off.

use anyhow::{Context, Result, anyhow, bail};
use cang_exec_protocol::{Frame, PROTOCOL_VERSION, read_frame, write_frame};
use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PSI_MEMORY_PATH: &str = "/proc/pressure/memory";
const VMSTAT_PATH: &str = "/proc/vmstat";

/// Opt-out switch, resolved once per launch.
const ENABLE_ENV: &str = "CANG_MEMORY_RECLAIM";
/// Overrides for the two trigger thresholds, for a host whose pressure profile
/// differs from the default, and for exercising the release path on a host under
/// no pressure at all.
const PSI_FULL_ENV: &str = "CANG_MEMORY_RECLAIM_PSI_FULL";
const SWAP_OUT_ENV: &str = "CANG_MEMORY_RECLAIM_SWAP_OUT";

/// Cadence of the host pressure samples.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// How often the watcher checks whether it should stop, so a task exit does not
/// wait for a whole sample interval.
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// `full avg10` at which the host is stalling on memory rather than merely
/// holding little of it.
const DEFAULT_PSI_FULL_PERCENT: f64 = 1.0;
/// Swap-out rate that counts as the host starting to swap, in 4 KiB pages per
/// second (a quarter of a megabyte per second).
const DEFAULT_SWAP_OUT_PAGES_PER_SECOND: f64 = 64.0;
/// Minimum time between two releases, so lasting pressure costs the guest its
/// cache once per window rather than once per sample.
const RECLAIM_COOLDOWN: Duration = Duration::from_secs(30);
/// Bound on the exchange with the guest: a wedged guest must not wedge the
/// watcher thread.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// One second of host pressure.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct PressureSample {
    full_avg10_percent: f64,
    swap_out_pages_per_second: f64,
}

/// The two trigger thresholds, resolved from the environment per launch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Thresholds {
    psi_full_percent: f64,
    swap_out_pages_per_second: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReclaimTrigger {
    MemoryPressure,
    SwapOut,
}

impl ReclaimTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::MemoryPressure => "memory-pressure",
            Self::SwapOut => "swap-out",
        }
    }
}

/// The watcher's handle: dropping it stops the thread and waits for it.
///
/// It lives in the helper, which outlives the launcher for a detached task, so a
/// task left running for `cang attach` keeps releasing guest memory.
pub(crate) struct MemoryReclaimGuard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MemoryReclaimGuard {
    /// Starts watching, or returns `None` when the launch has no exec channel to
    /// reach the guest through or the watcher is switched off.
    ///
    /// A watcher that cannot be started is never fatal: it is an optimisation
    /// for the host's memory, not part of the boot contract.
    pub(crate) fn start(exec_socket: Option<&Path>) -> Option<Self> {
        let socket = exec_socket?;
        if !enabled_from_env() {
            tracing::debug!("host memory reclaim disabled by {ENABLE_ENV}");
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let socket = socket.to_path_buf();
        let spawned = thread::Builder::new()
            .name("cang-memory-reclaim".to_owned())
            .spawn(move || watch(thread_stop, socket));
        match spawned {
            Ok(thread) => Some(Self {
                stop,
                thread: Some(thread),
            }),
            Err(err) => {
                tracing::warn!("host memory reclaim watcher could not start: {err}");
                None
            }
        }
    }
}

impl Drop for MemoryReclaimGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(stop: Arc<AtomicBool>, socket: PathBuf) {
    let thresholds = resolve_thresholds(
        std::env::var(PSI_FULL_ENV).ok().as_deref(),
        std::env::var(SWAP_OUT_ENV).ok().as_deref(),
    );
    tracing::debug!(
        psi_full_percent = thresholds.psi_full_percent,
        swap_out_pages_per_second = thresholds.swap_out_pages_per_second,
        "host memory reclaim thresholds resolved"
    );
    let mut reader = PressureReader::new();
    let mut last_reclaim: Option<Instant> = None;
    while !wait_until_next_sample(&stop) {
        let sample = match reader.sample() {
            Ok(sample) => sample,
            Err(err) => {
                tracing::debug!("host memory pressure could not be read: {err:#}");
                continue;
            }
        };
        let elapsed_since_reclaim = last_reclaim.map(|at| at.elapsed());
        let Some(trigger) =
            reclaim_due(reclaim_trigger(&sample, &thresholds), elapsed_since_reclaim)
        else {
            continue;
        };
        match request_guest_reclaim(&socket) {
            Ok(freed_kib) => {
                last_reclaim = Some(Instant::now());
                tracing::info!(
                    trigger = trigger.as_str(),
                    freed_kib,
                    full_avg10_percent = sample.full_avg10_percent,
                    swap_out_pages_per_second = sample.swap_out_pages_per_second,
                    "host memory pressure: guest released its clean page cache"
                );
            }
            Err(err) => {
                // Cooldown covers failures too: a guest that cannot help must
                // not be asked once per second.
                last_reclaim = Some(Instant::now());
                tracing::warn!(
                    trigger = trigger.as_str(),
                    "host memory pressure reclaim failed: {err:#}"
                );
            }
        }
    }
}

/// Sleeps for one sample interval, returning `true` once the watcher is asked to
/// stop.
fn wait_until_next_sample(stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + SAMPLE_INTERVAL;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(STOP_CHECK_INTERVAL);
    }
}

fn reclaim_trigger(sample: &PressureSample, thresholds: &Thresholds) -> Option<ReclaimTrigger> {
    if sample.full_avg10_percent >= thresholds.psi_full_percent {
        return Some(ReclaimTrigger::MemoryPressure);
    }
    if sample.swap_out_pages_per_second >= thresholds.swap_out_pages_per_second {
        return Some(ReclaimTrigger::SwapOut);
    }
    None
}

fn resolve_thresholds(psi_full: Option<&str>, swap_out: Option<&str>) -> Thresholds {
    Thresholds {
        psi_full_percent: parse_threshold(psi_full, DEFAULT_PSI_FULL_PERCENT),
        swap_out_pages_per_second: parse_threshold(swap_out, DEFAULT_SWAP_OUT_PAGES_PER_SECOND),
    }
}

/// A threshold is a plain non-negative number; anything else keeps the default
/// rather than guessing, the same way the filesystem knobs resolve.
fn parse_threshold(value: Option<&str>, default: f64) -> f64 {
    match value
        .map(str::trim)
        .and_then(|value| value.parse::<f64>().ok())
    {
        Some(parsed) if parsed >= 0.0 && parsed.is_finite() => parsed,
        _ => default,
    }
}

fn reclaim_due(
    trigger: Option<ReclaimTrigger>,
    elapsed_since_reclaim: Option<Duration>,
) -> Option<ReclaimTrigger> {
    let trigger = trigger?;
    match elapsed_since_reclaim {
        Some(elapsed) if elapsed < RECLAIM_COOLDOWN => None,
        _ => Some(trigger),
    }
}

fn enabled_from_env() -> bool {
    match std::env::var(ENABLE_ENV) {
        Ok(value) => enabled_from_value(Some(&value)),
        Err(_) => enabled_from_value(None),
    }
}

fn enabled_from_value(value: Option<&str>) -> bool {
    match value {
        Some(value) => !matches!(value.trim(), "0" | "false" | "no" | "off"),
        None => true,
    }
}

/// Holds the last swap-out counter so a sample can report a rate.
struct PressureReader {
    last_swap_out: Option<(u64, Instant)>,
}

impl PressureReader {
    fn new() -> Self {
        Self {
            last_swap_out: None,
        }
    }

    fn sample(&mut self) -> Result<PressureSample> {
        let full_avg10_percent = read_psi_full_avg10(&read_to_string(PSI_MEMORY_PATH)?)
            .with_context(|| format!("failed to parse {PSI_MEMORY_PATH}"))?;
        let swap_out = read_pswpout(&read_to_string(VMSTAT_PATH)?)
            .with_context(|| format!("failed to parse {VMSTAT_PATH}"))?;
        let now = Instant::now();
        let swap_out_pages_per_second = match self.last_swap_out {
            Some((previous, at)) => {
                let elapsed = now.saturating_duration_since(at).as_secs_f64();
                let pages = swap_out.saturating_sub(previous) as f64;
                if elapsed > 0.0 { pages / elapsed } else { 0.0 }
            }
            None => 0.0,
        };
        self.last_swap_out = Some((swap_out, now));
        Ok(PressureSample {
            full_avg10_percent,
            swap_out_pages_per_second,
        })
    }
}

fn read_to_string(path: &str) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read {path}"))
}

/// Reads `full avg10` from a PSI file, in percent.
fn read_psi_full_avg10(psi: &str) -> Result<f64> {
    let line = psi
        .lines()
        .find(|line| line.starts_with("full "))
        .ok_or_else(|| anyhow!("no full line"))?;
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))
        .ok_or_else(|| anyhow!("full line has no avg10"))?;
    value
        .parse::<f64>()
        .with_context(|| format!("avg10 '{value}' is not a number"))
}

/// Reads the cumulative swap-out page counter from `/proc/vmstat`.
fn read_pswpout(vmstat: &str) -> Result<u64> {
    let line = vmstat
        .lines()
        .find(|line| line.starts_with("pswpout "))
        .ok_or_else(|| anyhow!("no pswpout line"))?;
    let value = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("pswpout line has no value"))?;
    value
        .parse::<u64>()
        .with_context(|| format!("pswpout '{value}' is not a number"))
}

/// Asks the guest to release its clean page cache and returns the KiB it freed.
fn request_guest_reclaim(socket: &Path) -> Result<u64> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("failed to reach the guest at '{}'", socket.display()))?;
    stream.set_read_timeout(Some(EXCHANGE_TIMEOUT))?;
    stream.set_write_timeout(Some(EXCHANGE_TIMEOUT))?;
    write_frame(
        &mut stream,
        &Frame::Hello {
            version: PROTOCOL_VERSION,
        },
    )?;
    match read_frame(&mut stream)? {
        Some(Frame::Ready) => {}
        Some(Frame::Error(message)) => bail!("guest rejected the reclaim channel: {message}"),
        other => bail!("unexpected reply to the reclaim hello: {other:?}"),
    }
    write_frame(&mut stream, &Frame::ReclaimMemory)?;
    match read_frame(&mut stream)? {
        Some(Frame::ReclaimedMemory { freed_kib }) => Ok(freed_kib),
        Some(Frame::Error(message)) => bail!("{message}"),
        other => bail!("unexpected reply to a memory reclaim: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(full_avg10_percent: f64, swap_out_pages_per_second: f64) -> PressureSample {
        PressureSample {
            full_avg10_percent,
            swap_out_pages_per_second,
        }
    }

    #[test]
    fn parses_the_full_line_of_a_psi_file() {
        let psi = "some avg10=0.00 avg60=0.00 avg300=0.01 total=91\nfull avg10=12.34 avg60=1.20 avg300=0.32 total=7\n";
        assert!((read_psi_full_avg10(psi).unwrap() - 12.34).abs() < 1e-9);
    }

    #[test]
    fn rejects_a_psi_file_without_a_full_line() {
        let err =
            read_psi_full_avg10("some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n").unwrap_err();
        assert!(format!("{err:#}").contains("no full line"));
    }

    #[test]
    fn parses_the_swap_out_counter() {
        let vmstat = "nr_free_pages 123456\npswpin 7\npswpout 4242\n";
        assert_eq!(read_pswpout(vmstat).unwrap(), 4242);
    }

    #[test]
    fn rejects_vmstat_without_a_swap_out_counter() {
        let err = read_pswpout("pswpin 7\n").unwrap_err();
        assert!(format!("{err:#}").contains("no pswpout line"));
    }

    fn defaults() -> Thresholds {
        resolve_thresholds(None, None)
    }

    #[test]
    fn a_stalling_host_triggers_on_memory_pressure() {
        assert_eq!(
            reclaim_trigger(&sample(DEFAULT_PSI_FULL_PERCENT, 0.0), &defaults()),
            Some(ReclaimTrigger::MemoryPressure)
        );
    }

    #[test]
    fn a_swapping_host_triggers_on_swap_out() {
        assert_eq!(
            reclaim_trigger(&sample(0.0, DEFAULT_SWAP_OUT_PAGES_PER_SECOND), &defaults()),
            Some(ReclaimTrigger::SwapOut)
        );
    }

    #[test]
    fn a_quiet_host_does_not_trigger() {
        assert_eq!(reclaim_trigger(&sample(0.0, 0.0), &defaults()), None);
        assert_eq!(
            reclaim_trigger(
                &sample(
                    DEFAULT_PSI_FULL_PERCENT / 2.0,
                    DEFAULT_SWAP_OUT_PAGES_PER_SECOND / 2.0
                ),
                &defaults()
            ),
            None
        );
    }

    #[test]
    fn thresholds_default_and_can_be_lowered() {
        assert_eq!(
            defaults(),
            Thresholds {
                psi_full_percent: DEFAULT_PSI_FULL_PERCENT,
                swap_out_pages_per_second: DEFAULT_SWAP_OUT_PAGES_PER_SECOND,
            }
        );
        // Zero is what a verification run uses to fire on a quiet host.
        let forced = resolve_thresholds(Some("0"), Some("0"));
        assert_eq!(forced.psi_full_percent, 0.0);
        assert_eq!(forced.swap_out_pages_per_second, 0.0);
        assert!(reclaim_trigger(&sample(0.0, 0.0), &forced).is_some());
    }

    #[test]
    fn an_unreadable_threshold_keeps_the_default() {
        let thresholds = resolve_thresholds(Some("soon"), Some("-1"));
        assert_eq!(thresholds.psi_full_percent, DEFAULT_PSI_FULL_PERCENT);
        assert_eq!(
            thresholds.swap_out_pages_per_second,
            DEFAULT_SWAP_OUT_PAGES_PER_SECOND
        );
    }

    #[test]
    fn the_first_trigger_is_never_held_back_by_the_cooldown() {
        assert_eq!(
            reclaim_due(Some(ReclaimTrigger::SwapOut), None),
            Some(ReclaimTrigger::SwapOut)
        );
    }

    #[test]
    fn a_second_trigger_inside_the_cooldown_is_skipped() {
        assert_eq!(
            reclaim_due(
                Some(ReclaimTrigger::SwapOut),
                Some(RECLAIM_COOLDOWN - Duration::from_secs(1))
            ),
            None
        );
        assert_eq!(
            reclaim_due(Some(ReclaimTrigger::SwapOut), Some(RECLAIM_COOLDOWN)),
            Some(ReclaimTrigger::SwapOut)
        );
    }

    #[test]
    fn the_watcher_is_off_only_when_the_switch_says_so() {
        for value in ["0", "false", "no", "off", " 0 "] {
            assert!(
                !enabled_from_value(Some(value)),
                "{value} should switch it off"
            );
        }
        for value in ["1", "true", "on", "yes"] {
            assert!(
                enabled_from_value(Some(value)),
                "{value} should leave it on"
            );
        }
        assert!(enabled_from_value(None), "an unset variable means on");
    }
}
