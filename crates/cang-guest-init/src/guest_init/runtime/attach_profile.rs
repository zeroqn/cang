use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(in crate::guest_init::runtime) const ATTACH_PROFILE_ENV: &str = "CANG_ATTACH_PROFILE";

pub(in crate::guest_init::runtime) fn clear_process_env() {
    // SAFETY: guest-init consumes attach profiling during single-threaded
    // managed-session setup before forking the user shell, so child commands do
    // not inherit this diagnostic flag.
    unsafe { std::env::remove_var(ATTACH_PROFILE_ENV) };
}

pub(in crate::guest_init::runtime) fn env_value_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "summary"
    )
}

/// Per-read accounting a dedicated PTY reader thread accumulates on its own.
///
/// The attached session reads the master on a reader thread, so the counters
/// cannot live in the session-owned profiler; they are folded back into it with
/// [`GuestAttachProfiler::merge_pty_reader`] when the session ends.
#[derive(Debug, Default)]
pub(in crate::guest_init::runtime) struct PtyReaderStats {
    reads: AtomicU64,
    bytes: AtomicU64,
    read_max_bytes: AtomicU64,
    read_total_nanos: AtomicU64,
    read_max_nanos: AtomicU64,
    saturated: AtomicU64,
}

/// A plain copy of [`PtyReaderStats`] for the single-threaded report.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::guest_init::runtime) struct PtyReaderSnapshot {
    pub(in crate::guest_init::runtime) reads: u64,
    pub(in crate::guest_init::runtime) bytes: u64,
    pub(in crate::guest_init::runtime) read_max_bytes: usize,
    pub(in crate::guest_init::runtime) read_total: Duration,
    pub(in crate::guest_init::runtime) read_max: Duration,
    pub(in crate::guest_init::runtime) saturated: u64,
}

impl PtyReaderStats {
    pub(in crate::guest_init::runtime) fn new() -> Self {
        Self::default()
    }

    pub(in crate::guest_init::runtime) fn record_read(
        &self,
        bytes: usize,
        duration: Duration,
        buffer_size: usize,
    ) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        if bytes == buffer_size {
            self.saturated.fetch_add(1, Ordering::Relaxed);
        }
        self.read_total_nanos
            .fetch_add(duration.as_nanos() as u64, Ordering::Relaxed);
        let nanos = duration.as_nanos() as u64;
        self.read_max_nanos.fetch_max(nanos, Ordering::Relaxed);
        self.read_max_bytes
            .fetch_max(bytes as u64, Ordering::Relaxed);
    }

    pub(in crate::guest_init::runtime) fn snapshot(&self) -> PtyReaderSnapshot {
        PtyReaderSnapshot {
            reads: self.reads.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            read_max_bytes: self.read_max_bytes.load(Ordering::Relaxed) as usize,
            read_total: Duration::from_nanos(self.read_total_nanos.load(Ordering::Relaxed)),
            read_max: Duration::from_nanos(self.read_max_nanos.load(Ordering::Relaxed)),
            saturated: self.saturated.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::guest_init::runtime) struct GuestAttachProfiler {
    enabled: bool,
    pty_readable: u64,
    pty_reads: u64,
    pty_bytes: u64,
    pty_read_max_bytes: usize,
    pty_read_saturated_count: u64,
    pty_drain_events: u64,
    pty_drain_would_block_count: u64,
    pty_drain_bound_hit_count: u64,
    pty_drain_reads_max: usize,
    pty_drain_coalesced_events: u64,
    pty_drain_coalesced_reads: u64,
    pty_read_total: Duration,
    pty_read_max: Duration,
    normalize_parse_total: Duration,
    normalize_parse_max: Duration,
    normalize_total: Duration,
    normalize_max: Duration,
    parser_total: Duration,
    parser_max: Duration,
    frame_write_total: Duration,
    frame_write_max: Duration,
    frame_bytes: u64,
    frame_max_bytes: usize,
    frames: u64,
}

impl GuestAttachProfiler {
    pub(in crate::guest_init::runtime) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    pub(in crate::guest_init::runtime) fn record_pty_readable(&mut self) {
        if self.enabled {
            self.pty_readable += 1;
        }
    }

    /// Fold the reader thread's counters into the per-read profile fields.
    pub(in crate::guest_init::runtime) fn merge_pty_reader(
        &mut self,
        snapshot: &PtyReaderSnapshot,
    ) {
        if !self.enabled {
            return;
        }
        self.pty_reads = self.pty_reads.saturating_add(snapshot.reads);
        self.pty_bytes = self.pty_bytes.saturating_add(snapshot.bytes);
        self.pty_read_saturated_count = self
            .pty_read_saturated_count
            .saturating_add(snapshot.saturated);
        self.pty_read_max_bytes = self.pty_read_max_bytes.max(snapshot.read_max_bytes);
        self.pty_read_total += snapshot.read_total;
        self.pty_read_max = self.pty_read_max.max(snapshot.read_max);
    }

    pub(in crate::guest_init::runtime) fn record_pty_drain(
        &mut self,
        reads: usize,
        stopped_on_would_block: bool,
        stopped_on_bound: bool,
    ) {
        if !self.enabled {
            return;
        }
        self.pty_drain_events += 1;
        if stopped_on_would_block {
            self.pty_drain_would_block_count += 1;
        }
        if stopped_on_bound {
            self.pty_drain_bound_hit_count += 1;
        }
        self.pty_drain_reads_max = self.pty_drain_reads_max.max(reads);
        if reads > 1 {
            self.pty_drain_coalesced_events += 1;
            self.pty_drain_coalesced_reads = self
                .pty_drain_coalesced_reads
                .saturating_add((reads - 1) as u64);
        }
    }

    pub(in crate::guest_init::runtime) fn record_terminal_processing(
        &mut self,
        normalize_duration: Duration,
        parser_duration: Duration,
    ) {
        if !self.enabled {
            return;
        }
        let combined_duration = normalize_duration + parser_duration;
        self.normalize_parse_total += combined_duration;
        self.normalize_parse_max = self.normalize_parse_max.max(combined_duration);
        self.normalize_total += normalize_duration;
        self.normalize_max = self.normalize_max.max(normalize_duration);
        self.parser_total += parser_duration;
        self.parser_max = self.parser_max.max(parser_duration);
    }

    pub(in crate::guest_init::runtime) fn record_frame_write(
        &mut self,
        bytes: usize,
        duration: Duration,
    ) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        self.frame_bytes = self.frame_bytes.saturating_add(bytes as u64);
        self.frame_max_bytes = self.frame_max_bytes.max(bytes);
        self.frame_write_total += duration;
        self.frame_write_max = self.frame_write_max.max(duration);
    }

    pub(in crate::guest_init::runtime) fn report_to(
        &self,
        writer: &mut impl Write,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        writer.write_all(self.summary_line().as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()
    }

    fn summary_line(&self) -> String {
        format!(
            "cang attach profile role=guest pty_readable={} pty_reads={} pty_bytes={} pty_read_max_bytes={} pty_read_avg_bytes={} pty_read_saturated_count={} pty_drain_events={} pty_drain_would_block_count={} pty_drain_bound_hit_count={} pty_drain_reads_max={} pty_drain_coalesced_events={} pty_drain_coalesced_reads={} pty_read_total_us={} pty_read_max_us={} normalize_parse_total_us={} normalize_parse_max_us={} normalize_total_us={} normalize_max_us={} parser_total_us={} parser_max_us={} frame_write_total_us={} frame_write_max_us={} frame_bytes={} frame_max_bytes={} frame_avg_bytes={} frames={}",
            self.pty_readable,
            self.pty_reads,
            self.pty_bytes,
            self.pty_read_max_bytes,
            average(self.pty_bytes, self.pty_reads),
            self.pty_read_saturated_count,
            self.pty_drain_events,
            self.pty_drain_would_block_count,
            self.pty_drain_bound_hit_count,
            self.pty_drain_reads_max,
            self.pty_drain_coalesced_events,
            self.pty_drain_coalesced_reads,
            duration_micros(self.pty_read_total),
            duration_micros(self.pty_read_max),
            duration_micros(self.normalize_parse_total),
            duration_micros(self.normalize_parse_max),
            duration_micros(self.normalize_total),
            duration_micros(self.normalize_max),
            duration_micros(self.parser_total),
            duration_micros(self.parser_max),
            duration_micros(self.frame_write_total),
            duration_micros(self.frame_write_max),
            self.frame_bytes,
            self.frame_max_bytes,
            average(self.frame_bytes, self.frames),
            self.frames,
        )
    }
}

fn average(total: u64, count: u64) -> u64 {
    total.checked_div(count).unwrap_or(0)
}

fn duration_micros(duration: Duration) -> u128 {
    duration.as_micros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_profile_env_accepts_only_truthy_values() {
        assert!(!env_value_enabled(""));
        assert!(!env_value_enabled("0"));
        assert!(!env_value_enabled("false"));
        assert!(env_value_enabled("1"));
        assert!(env_value_enabled("true"));
        assert!(env_value_enabled("summary"));
    }

    #[test]
    fn guest_attach_profile_summary_has_required_fields() {
        let mut profiler = GuestAttachProfiler::new(true);
        profiler.record_pty_readable();
        let reader_stats = PtyReaderStats::new();
        reader_stats.record_read(16, Duration::from_micros(3), 16);
        profiler.merge_pty_reader(&reader_stats.snapshot());
        profiler.record_pty_drain(3, true, false);
        profiler.record_pty_drain(2, false, true);
        profiler.record_terminal_processing(Duration::from_micros(2), Duration::from_micros(3));
        profiler.record_terminal_processing(Duration::from_micros(7), Duration::from_micros(11));
        profiler.record_frame_write(12, Duration::from_micros(13));
        let mut output = Vec::new();

        profiler.report_to(&mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("cang attach profile role=guest "));
        assert!(output.contains("pty_readable=1"));
        assert!(output.contains("pty_reads=1"));
        assert!(output.contains("pty_read_max_bytes=16"));
        assert!(output.contains("pty_read_avg_bytes=16"));
        assert!(output.contains("pty_read_saturated_count=1"));
        assert!(output.contains("pty_drain_events=2"));
        assert!(output.contains("pty_drain_would_block_count=1"));
        assert!(output.contains("pty_drain_bound_hit_count=1"));
        assert!(output.contains("pty_drain_reads_max=3"));
        assert!(output.contains("pty_drain_coalesced_events=2"));
        assert!(output.contains("pty_drain_coalesced_reads=3"));
        assert!(output.contains("normalize_parse_total_us=23"));
        assert!(output.contains("normalize_parse_max_us=18"));
        assert!(output.contains("normalize_total_us=9"));
        assert!(output.contains("normalize_max_us=7"));
        assert!(output.contains("parser_total_us=14"));
        assert!(output.contains("parser_max_us=11"));
        assert!(output.contains("frame_write_total_us=13"));
        assert!(output.contains("frame_max_bytes=12"));
        assert!(output.contains("frame_avg_bytes=12"));
        assert!(output.contains("frames=1"));
    }

    #[test]
    fn disabled_guest_attach_profile_is_quiet() {
        let profiler = GuestAttachProfiler::new(false);
        let mut output = Vec::new();

        profiler.report_to(&mut output).unwrap();

        assert!(output.is_empty());
    }
}
