use std::io::{self, Write};
use std::time::Duration;

pub(in crate::runtime::session) const ATTACH_PROFILE_ENV: &str = "CANG_ATTACH_PROFILE";

pub(in crate::runtime::session) fn enabled_from_process_env() -> bool {
    std::env::var(ATTACH_PROFILE_ENV)
        .ok()
        .as_deref()
        .is_some_and(env_value_enabled)
}

pub(in crate::runtime::session) fn guest_env_pair_from_process_env() -> Option<(String, String)> {
    guest_env_pair_from_value(std::env::var(ATTACH_PROFILE_ENV).ok().as_deref())
}

pub(in crate::runtime::session) fn guest_env_pair_from_value(
    value: Option<&str>,
) -> Option<(String, String)> {
    value
        .filter(|value| env_value_enabled(value))
        .map(|_| (ATTACH_PROFILE_ENV.to_owned(), "1".to_owned()))
}

fn env_value_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "summary"
    )
}

#[derive(Debug, Default)]
pub(in crate::runtime::session) struct HostAttachProfiler {
    enabled: bool,
    frames: u64,
    bytes: u64,
    frame_max_bytes: usize,
    frame_read_total: Duration,
    frame_read_max: Duration,
    stdout_batches: u64,
    stdout_batch_frames_max: usize,
    stdout_batch_bytes_max: usize,
    stdout_batch_frames_total: u64,
    stdout_write_total: Duration,
    stdout_write_max: Duration,
    stdout_write_count: u64,
    stdout_flush_total: Duration,
    stdout_flush_max: Duration,
    stdout_flush_count: u64,
    outbound_deferred_writes: u64,
    outbound_pending_max_bytes: usize,
    stdout_blocked_total: Duration,
    stdout_blocked_max: Duration,
}

impl HostAttachProfiler {
    pub(in crate::runtime::session) fn from_process_env() -> Self {
        Self::new(enabled_from_process_env())
    }

    pub(in crate::runtime::session) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    pub(in crate::runtime::session) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(in crate::runtime::session) fn record_frame_read(&mut self, duration: Duration) {
        if !self.enabled {
            return;
        }
        self.frame_read_total += duration;
        self.frame_read_max = self.frame_read_max.max(duration);
    }

    pub(in crate::runtime::session) fn record_data_frame(&mut self, bytes: usize) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        self.bytes = self.bytes.saturating_add(bytes as u64);
        self.frame_max_bytes = self.frame_max_bytes.max(bytes);
    }

    pub(in crate::runtime::session) fn record_stdout_write(&mut self, duration: Duration) {
        if !self.enabled {
            return;
        }
        self.stdout_write_count += 1;
        self.stdout_write_total += duration;
        self.stdout_write_max = self.stdout_write_max.max(duration);
    }

    pub(in crate::runtime::session) fn record_stdout_flush(&mut self, duration: Duration) {
        if !self.enabled {
            return;
        }
        self.stdout_flush_count += 1;
        self.stdout_flush_total += duration;
        self.stdout_flush_max = self.stdout_flush_max.max(duration);
    }

    pub(in crate::runtime::session) fn record_stdout_batch(
        &mut self,
        frame_count: usize,
        byte_count: usize,
    ) {
        if !self.enabled || frame_count == 0 {
            return;
        }
        self.stdout_batches += 1;
        self.stdout_batch_frames_total = self
            .stdout_batch_frames_total
            .saturating_add(frame_count as u64);
        self.stdout_batch_frames_max = self.stdout_batch_frames_max.max(frame_count);
        self.stdout_batch_bytes_max = self.stdout_batch_bytes_max.max(byte_count);
    }

    pub(in crate::runtime::session) fn report_to(&self, writer: &mut impl Write) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        writer.write_all(self.summary_line().as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()
    }

    /// Fold the outbound writer's own measurements in.
    ///
    /// When the terminal is drained off-loop its write/flush counters come from
    /// the writer thread rather than the reader, so they merge here; the queue
    /// counters say how much output had to wait and for how long.
    pub(in crate::runtime::session) fn merge_outbound(
        &mut self,
        stats: super::attach::OutboundStats,
    ) {
        if !self.enabled {
            return;
        }
        // Replace, not add: the reader's own numbers measured the queue push, so
        // the writer thread's terminal timings are the ones that count.
        self.stdout_write_count = stats.writes;
        self.stdout_write_total = stats.write_total;
        self.stdout_write_max = stats.write_max;
        self.stdout_flush_count = stats.flushes;
        self.stdout_flush_total = stats.flush_total;
        self.stdout_flush_max = stats.flush_max;
        self.outbound_deferred_writes += stats.deferred_writes;
        self.outbound_pending_max_bytes =
            self.outbound_pending_max_bytes.max(stats.pending_max_bytes);
        self.stdout_blocked_total += stats.blocked_total;
        self.stdout_blocked_max = self.stdout_blocked_max.max(stats.blocked_max);
    }

    fn summary_line(&self) -> String {
        format!(
            "cang attach profile role=host frames={} bytes={} frame_max_bytes={} frame_avg_bytes={} frame_read_total_us={} frame_read_max_us={} stdout_batches={} stdout_batch_frames_max={} stdout_batch_bytes_max={} stdout_batch_frames_avg={} stdout_write_count={} stdout_write_total_us={} stdout_write_max_us={} stdout_flush_count={} stdout_flush_total_us={} stdout_flush_max_us={} outbound_deferred_writes={} outbound_pending_max_bytes={} stdout_blocked_us={} stdout_blocked_max_us={}",
            self.frames,
            self.bytes,
            self.frame_max_bytes,
            average(self.bytes, self.frames),
            duration_micros(self.frame_read_total),
            duration_micros(self.frame_read_max),
            self.stdout_batches,
            self.stdout_batch_frames_max,
            self.stdout_batch_bytes_max,
            average(self.stdout_batch_frames_total, self.stdout_batches),
            self.stdout_write_count,
            duration_micros(self.stdout_write_total),
            duration_micros(self.stdout_write_max),
            self.stdout_flush_count,
            duration_micros(self.stdout_flush_total),
            duration_micros(self.stdout_flush_max),
            self.outbound_deferred_writes,
            self.outbound_pending_max_bytes,
            duration_micros(self.stdout_blocked_total),
            duration_micros(self.stdout_blocked_max),
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
    fn attach_profile_env_pair_requires_truthy_value() {
        assert_eq!(guest_env_pair_from_value(None), None);
        assert_eq!(guest_env_pair_from_value(Some("")), None);
        assert_eq!(guest_env_pair_from_value(Some("0")), None);
        assert_eq!(guest_env_pair_from_value(Some("false")), None);
        assert_eq!(
            guest_env_pair_from_value(Some("summary")),
            Some((ATTACH_PROFILE_ENV.to_owned(), "1".to_owned()))
        );
    }

    #[test]
    fn host_attach_profile_summary_has_stable_shape() {
        let mut profiler = HostAttachProfiler::new(true);
        profiler.record_frame_read(Duration::from_micros(7));
        profiler.record_data_frame(4);
        profiler.record_stdout_batch(1, 4);
        profiler.record_stdout_write(Duration::from_micros(11));
        profiler.record_stdout_flush(Duration::from_micros(13));
        let mut output = Vec::new();

        profiler.report_to(&mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("cang attach profile role=host "));
        assert!(output.contains("frames=1"));
        assert!(output.contains("bytes=4"));
        assert!(output.contains("frame_max_bytes=4"));
        assert!(output.contains("frame_avg_bytes=4"));
        assert!(output.contains("frame_read_total_us=7"));
        assert!(output.contains("stdout_batches=1"));
        assert!(output.contains("stdout_batch_frames_max=1"));
        assert!(output.contains("stdout_batch_bytes_max=4"));
        assert!(output.contains("stdout_batch_frames_avg=1"));
        assert!(output.contains("stdout_write_count=1"));
        assert!(output.contains("stdout_write_total_us=11"));
        assert!(output.contains("stdout_flush_count=1"));
        assert!(output.contains("stdout_flush_total_us=13"));
    }

    #[test]
    fn the_summary_reports_what_the_outbound_writer_saw() {
        let mut profiler = HostAttachProfiler::new(true);
        profiler.merge_outbound(super::super::attach::OutboundStats {
            deferred_writes: 3,
            pending_max_bytes: 4096,
            blocked_total: Duration::from_micros(900),
            blocked_max: Duration::from_micros(700),
            writes: 2,
            write_total: Duration::from_micros(11),
            write_max: Duration::from_micros(9),
            flushes: 2,
            flush_total: Duration::from_micros(13),
            flush_max: Duration::from_micros(8),
        });
        let line = profiler.summary_line();
        assert!(line.contains("outbound_deferred_writes=3"), "{line}");
        assert!(line.contains("outbound_pending_max_bytes=4096"), "{line}");
        assert!(line.contains("stdout_blocked_us=900"), "{line}");
        assert!(line.contains("stdout_blocked_max_us=700"), "{line}");
        // The terminal's own writes now come from the writer thread.
        assert!(line.contains("stdout_write_count=2"), "{line}");
    }

    #[test]
    fn disabled_host_attach_profile_is_quiet() {
        let profiler = HostAttachProfiler::new(false);
        let mut output = Vec::new();

        profiler.report_to(&mut output).unwrap();

        assert!(output.is_empty());
    }
}
