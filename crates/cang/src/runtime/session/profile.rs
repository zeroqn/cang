use anyhow::Result;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const CANG_HOST_PROFILE_ENV: &str = "CANG_HOST_PROFILE";
const VM_WORKER_WAIT_DETAIL_FILENAME: &str = "vm-worker-host-profile.tsv";

#[derive(Debug)]
pub(crate) struct CangHostProfiler {
    enabled: bool,
    started_at: Instant,
    metadata: Vec<ProfileMetadata>,
    records: Vec<ProfileRecord>,
    raw_records: Vec<RawProfileRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileMetadata {
    label: &'static str,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProfileRecord {
    label: &'static str,
    duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawProfileRecord {
    label: String,
    nanos: u128,
}

impl CangHostProfiler {
    pub(in crate::runtime::session) fn new_started_at(enabled: bool, started_at: Instant) -> Self {
        Self {
            enabled,
            started_at,
            metadata: Vec::new(),
            records: Vec::new(),
            raw_records: Vec::new(),
        }
    }

    pub(in crate::runtime::session) fn from_env_started_now() -> Self {
        Self::new_started_at(host_profile_env_enabled(), Instant::now())
    }

    pub(in crate::runtime::session) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(in crate::runtime::session) fn record_metadata(
        &mut self,
        label: &'static str,
        value: impl Into<String>,
    ) {
        if self.enabled {
            self.metadata.push(ProfileMetadata {
                label,
                value: value.into(),
            });
        }
    }

    pub(in crate::runtime::session) fn measure_result<T>(
        &mut self,
        label: &'static str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.enabled {
            return f();
        }

        let started_at = Instant::now();
        let result = f();
        self.records.push(ProfileRecord {
            label,
            duration: started_at.elapsed(),
        });
        result
    }

    pub(in crate::runtime::session) fn measure_result_with_duration<T>(
        &mut self,
        label: &'static str,
        f: impl FnOnce() -> Result<T>,
    ) -> Result<(T, Duration)> {
        if !self.enabled {
            return f().map(|value| (value, Duration::ZERO));
        }

        let started_at = Instant::now();
        let result = f();
        let duration = started_at.elapsed();
        self.record_duration(label, duration);
        result.map(|value| (value, duration))
    }

    pub(in crate::runtime::session) fn measure_result_with<T>(
        &mut self,
        label: &'static str,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if !self.enabled {
            return f(self);
        }

        let started_at = Instant::now();
        let result = f(self);
        self.records.push(ProfileRecord {
            label,
            duration: started_at.elapsed(),
        });
        result
    }

    pub(in crate::runtime::session) fn rootfs_materialization_recorder(
        &mut self,
    ) -> CangRootfsMaterializationRecorder<'_> {
        CangRootfsMaterializationRecorder { profiler: self }
    }

    pub(in crate::runtime::session) fn write_vm_worker_wait_details(
        &self,
        task_state_dir: &Path,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let path = vm_worker_wait_detail_path(task_state_dir);
        let temp_path = vm_worker_wait_detail_temp_path(task_state_dir);
        let raw_libkrun_records = read_raw_libkrun_profile_records(&path)?;
        let mut file = File::create(&temp_path)?;
        for record in &self.records {
            if is_vm_worker_wait_detail_label(record.label) {
                writeln!(file, "{}\t{}", record.label, record.duration.as_nanos())?;
            }
        }
        for record in raw_libkrun_records {
            writeln!(file, "{}\t{}", record.label, record.nanos)?;
        }
        file.flush()?;
        fs::rename(temp_path, path)
    }

    pub(in crate::runtime::session) fn record_vm_worker_wait_details(
        &mut self,
        task_state_dir: &Path,
        wait_duration: Duration,
    ) {
        if !self.enabled {
            return;
        }
        let _ = self.try_record_vm_worker_wait_details(task_state_dir, wait_duration);
    }

    pub(in crate::runtime::session) fn emit_to_stderr(&self) {
        let stderr = io::stderr();
        let mut writer = stderr.lock();
        let _ = self.write_report(&mut writer);
    }

    pub(in crate::runtime::session) fn finalize_result<T>(
        &mut self,
        result: Result<T>,
    ) -> Result<T> {
        let profile_result = if result.is_ok() { "ok" } else { "error" };
        self.record_metadata("profile_result", profile_result);
        self.emit_to_stderr();
        result
    }

    fn write_report(&self, writer: &mut impl Write) -> io::Result<()> {
        self.write_report_with_total(writer, self.started_at.elapsed())
    }

    fn write_report_with_total(&self, writer: &mut impl Write, total: Duration) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        writeln!(writer, "cang host profile")?;
        for metadata in &self.metadata {
            writeln!(writer, "  {}: {}", metadata.label, metadata.value)?;
        }
        for record in &self.records {
            writeln!(
                writer,
                "  {}: {}",
                record.label,
                format_duration(record.duration)
            )?;
        }
        writeln!(
            writer,
            "  total_profiled_host_runtime: {}",
            format_duration(total)
        )?;
        if !self.raw_records.is_empty() {
            writeln!(writer, "libkrun profile")?;
            for record in &self.raw_records {
                writeln!(
                    writer,
                    "  {}: {}ns ({})",
                    record.label,
                    record.nanos,
                    format_duration(duration_from_nanos_saturating(record.nanos))
                )?;
            }
        }
        writer.flush()
    }

    fn record_duration(&mut self, label: &'static str, duration: Duration) {
        if self.enabled {
            self.records.push(ProfileRecord { label, duration });
        }
    }

    pub(in crate::runtime::session) fn record_vm_worker_libkrun_configure(
        &mut self,
        duration: Duration,
    ) {
        self.record_duration("vm_worker_libkrun_configure", duration);
    }

    pub(in crate::runtime::session) fn record_vm_worker_libkrun_session(
        &mut self,
        duration: Duration,
    ) {
        self.record_duration("vm_worker_libkrun_session", duration);
    }

    pub(in crate::runtime::session) fn record_vm_worker_libkrun_enter(
        &mut self,
        duration: Duration,
    ) {
        self.record_duration("vm_worker_libkrun_enter", duration);
    }

    fn try_record_vm_worker_wait_details(
        &mut self,
        task_state_dir: &Path,
        wait_duration: Duration,
    ) -> io::Result<()> {
        let text = fs::read_to_string(vm_worker_wait_detail_path(task_state_dir))?;
        let mut accounted = Duration::ZERO;
        let mut recorded_any = false;

        for line in text.lines() {
            let Some((child_label, nanos_text)) = line.split_once('\t') else {
                continue;
            };
            let Ok(nanos) = nanos_text.parse::<u128>() else {
                continue;
            };
            if is_raw_libkrun_profile_label(child_label) {
                self.record_raw_profile(child_label, nanos);
                recorded_any = true;
                continue;
            }
            let Some(mapping) = vm_worker_wait_detail_parent_mapping(child_label) else {
                continue;
            };
            let duration = duration_from_nanos_saturating(nanos);
            accounted = accounted.saturating_add(duration);
            self.record_duration(mapping.parent_label, duration);
            recorded_any = true;
        }

        if recorded_any {
            let unattributed = wait_duration.saturating_sub(accounted);
            self.record_duration("helper_wait_vm_worker_child_unattributed", unattributed);
        }
        Ok(())
    }

    fn record_raw_profile(&mut self, label: &str, nanos: u128) {
        self.raw_records.push(RawProfileRecord {
            label: label.to_owned(),
            nanos,
        });
    }
}

pub(in crate::runtime::session) trait RootfsMaterializationRecorder {
    fn measure_result<T, F>(&mut self, label: &'static str, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>;
}

pub(in crate::runtime::session) struct DisabledRootfsMaterializationRecorder;

impl RootfsMaterializationRecorder for DisabledRootfsMaterializationRecorder {
    fn measure_result<T, F>(&mut self, _label: &'static str, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        f()
    }
}

pub(in crate::runtime::session) struct CangRootfsMaterializationRecorder<'a> {
    profiler: &'a mut CangHostProfiler,
}

impl RootfsMaterializationRecorder for CangRootfsMaterializationRecorder<'_> {
    fn measure_result<T, F>(&mut self, label: &'static str, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        self.profiler.measure_result(label, f)
    }
}

fn host_profile_env_enabled() -> bool {
    matches!(
        std::env::var(CANG_HOST_PROFILE_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("on")
    )
}

fn format_duration(duration: Duration) -> String {
    format!("{:.3}ms", duration.as_secs_f64() * 1000.0)
}

pub(in crate::runtime::session) fn vm_worker_wait_detail_path(task_state_dir: &Path) -> PathBuf {
    task_state_dir.join(VM_WORKER_WAIT_DETAIL_FILENAME)
}

fn vm_worker_wait_detail_temp_path(task_state_dir: &Path) -> PathBuf {
    task_state_dir.join(format!("{VM_WORKER_WAIT_DETAIL_FILENAME}.tmp"))
}

fn read_raw_libkrun_profile_records(path: &Path) -> io::Result<Vec<RawProfileRecord>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let records = text
        .lines()
        .filter_map(parse_raw_libkrun_profile_record)
        .collect();
    Ok(records)
}

fn parse_raw_libkrun_profile_record(line: &str) -> Option<RawProfileRecord> {
    let (label, nanos_text) = line.split_once('\t')?;
    if !is_raw_libkrun_profile_label(label) {
        return None;
    }
    let nanos = nanos_text.parse::<u128>().ok()?;
    Some(RawProfileRecord {
        label: label.to_owned(),
        nanos,
    })
}

fn is_raw_libkrun_profile_label(label: &str) -> bool {
    label.starts_with("libkrun_")
}

fn is_vm_worker_wait_detail_label(label: &str) -> bool {
    vm_worker_wait_detail_parent_mapping(label).is_some()
}

#[derive(Debug, Clone, Copy)]
struct VmWorkerWaitDetailMapping {
    parent_label: &'static str,
}

impl VmWorkerWaitDetailMapping {
    const fn accounted(parent_label: &'static str) -> Self {
        Self { parent_label }
    }
}

fn vm_worker_wait_detail_parent_mapping(label: &str) -> Option<VmWorkerWaitDetailMapping> {
    Some(match label {
        "vm_worker_config_read" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_config_read")
        }
        "vm_worker_identity_configure" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_identity_configure")
        }
        "vm_worker_enter_netns" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_enter_netns")
        }
        "vm_worker_passt_start" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_passt_start")
        }
        "vm_worker_prepare_root" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_prepare_root")
        }
        "vm_worker_guest_config_write" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_guest_config_write")
        }
        "vm_worker_libkrun_open" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_libkrun_open")
        }
        "vm_worker_libkrun_configure" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_libkrun_configure")
        }
        "vm_worker_libkrun_enter" => {
            VmWorkerWaitDetailMapping::accounted("helper_wait_vm_worker_child_libkrun_enter")
        }
        _ => return None,
    })
}

fn duration_from_nanos_saturating(nanos: u128) -> Duration {
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn host_profile_report_is_suppressed_when_disabled() {
        let profiler = CangHostProfiler::new_started_at(false, Instant::now());
        let mut output = Vec::new();

        profiler
            .write_report_with_total(&mut output, Duration::from_millis(1))
            .expect("disabled report should be writable");

        assert!(output.is_empty());
    }

    #[test]
    fn host_profile_report_uses_stable_labels_and_total_scope() {
        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());

        profiler.record_metadata("task_rootfs_backend", "btrfs-snapshot");
        profiler.record_metadata("image", "localhost/cang:latest");
        profiler.record_metadata("image_digest", "sha256:abc123");
        profiler.record_metadata("task_rootfs_cache_status", "hit");
        profiler.record_metadata("task_rootfs_cache_digest_key", "sha256-abc123");
        profiler.record_metadata(
            "task_rootfs_cache_path",
            "/tmp/cang/microvm/images/btrfs-snapshots/sha256-abc123",
        );
        profiler
            .measure_result("workspace_canonicalization", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("launch_plan_build", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("task_rootfs_materialization", || Ok(()))
            .expect("phase should pass");
        for label in [
            "task_rootfs_materialization:reset_task_dir",
            "task_rootfs_materialization:buildah_version",
            "task_rootfs_materialization:image_probe",
            "task_rootfs_materialization:cache_entry_read",
            "task_rootfs_materialization:cache_snapshot",
            "task_rootfs_materialization:buildah_materializer",
            "task_rootfs_materialization:cache_population",
        ] {
            profiler
                .measure_result(label, || Ok(()))
                .expect("rootfs profile detail should pass");
        }
        profiler
            .measure_result("persistent_disk_preparation", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("guest_init_resolution", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("launch_config_build", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("helper_command_build", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("helper_spawn_process", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("helper_wait_process", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("helper_session", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("task_state_cleanup", || Ok(()))
            .expect("phase should pass");

        let mut output = Vec::new();
        profiler
            .write_report_with_total(&mut output, Duration::from_millis(12))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");

        assert!(text.starts_with("cang host profile\n"));
        assert!(text.contains("task_rootfs_backend: btrfs-snapshot"));
        assert!(text.contains("image: localhost/cang:latest"));
        assert!(text.contains("image_digest: sha256:abc123"));
        assert!(text.contains("task_rootfs_cache_status: hit"));
        assert!(text.contains("task_rootfs_cache_digest_key: sha256-abc123"));
        assert!(text.contains(
            "task_rootfs_cache_path: /tmp/cang/microvm/images/btrfs-snapshots/sha256-abc123"
        ));
        assert!(!text.contains("cang-guest-init profile"));
        for label in [
            "workspace_canonicalization",
            "launch_plan_build",
            "task_rootfs_materialization",
            "task_rootfs_materialization:reset_task_dir",
            "task_rootfs_materialization:buildah_version",
            "task_rootfs_materialization:image_probe",
            "task_rootfs_materialization:cache_entry_read",
            "task_rootfs_materialization:cache_snapshot",
            "task_rootfs_materialization:buildah_materializer",
            "task_rootfs_materialization:cache_population",
            "persistent_disk_preparation",
            "guest_init_resolution",
            "launch_config_build",
            "helper_command_build",
            "helper_spawn_process",
            "helper_wait_process",
            "helper_session",
            "task_state_cleanup",
            "total_profiled_host_runtime",
        ] {
            assert!(text.contains(label), "missing profile label {label}");
        }
    }

    #[test]
    fn host_profile_total_uses_injected_session_start() {
        let started_at = Instant::now() - Duration::from_millis(123);
        let profiler = CangHostProfiler::new_started_at(true, started_at);
        let mut output = Vec::new();

        profiler
            .write_report(&mut output)
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");
        let total_ms = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("total_profiled_host_runtime: "))
            .and_then(|value| value.strip_suffix("ms"))
            .and_then(|value| value.parse::<f64>().ok())
            .expect("total runtime should be present as milliseconds");

        assert!(
            total_ms >= 123.0,
            "total runtime should include time before profiler construction: {total_ms}ms"
        );
    }

    #[test]
    fn host_profile_records_failed_phases_before_returning_error() {
        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());

        let err = profiler
            .measure_result::<()>("helper_session", || Err(anyhow!("fake failure")))
            .expect_err("phase should fail");

        assert_eq!(err.to_string(), "fake failure");
        let mut output = Vec::new();
        profiler
            .write_report_with_total(&mut output, Duration::from_millis(1))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");
        assert!(text.contains("helper_session"));
    }

    #[test]
    fn host_profile_finalize_records_result_metadata_before_returning_result() {
        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());

        let err = profiler
            .finalize_result::<()>(Err(anyhow!("fake failure")))
            .expect_err("finalized result should preserve original error");

        assert_eq!(err.to_string(), "fake failure");
        let mut output = Vec::new();
        profiler
            .write_report_with_total(&mut output, Duration::from_millis(1))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");
        assert!(text.contains("profile_result: error"));
    }

    #[test]
    fn host_profile_report_supports_child_scope_metadata_and_correlators() {
        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());

        profiler.record_metadata("profile_scope", "helper");
        profiler.record_metadata("profile_launch_config_path", "/tmp/cang-task/launch.conf");
        profiler.record_metadata("profile_task_state_dir", "/tmp/cang-task");
        profiler
            .measure_result("helper_config_read", || Ok(()))
            .expect("phase should pass");
        profiler
            .measure_result("helper_wait_vm_worker", || Ok(()))
            .expect("phase should pass");

        let mut output = Vec::new();
        profiler
            .write_report_with_total(&mut output, Duration::from_millis(10))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");

        assert!(text.starts_with("cang host profile\n"));
        assert!(text.contains("profile_scope: helper"));
        assert!(text.contains("profile_launch_config_path: /tmp/cang-task/launch.conf"));
        assert!(text.contains("profile_task_state_dir: /tmp/cang-task"));
        assert!(text.contains("helper_config_read"));
        assert!(text.contains("helper_wait_vm_worker"));
        assert!(text.contains("total_profiled_host_runtime"));
        assert!(!text.contains("cang-guest-init profile"));
    }

    #[test]
    fn host_profile_imports_vm_worker_wait_child_details() {
        let task_state_dir = unique_temp_dir("cang-profile-vm-worker-details");
        fs::create_dir_all(&task_state_dir).expect("temp task state dir should be created");
        let cleanup_dir = task_state_dir.clone();

        let mut child_profiler = CangHostProfiler::new_started_at(true, Instant::now());
        child_profiler.record_duration("vm_worker_config_read", Duration::from_millis(1));
        child_profiler.record_vm_worker_libkrun_configure(Duration::from_millis(2));
        child_profiler.record_duration("vm_worker_libkrun_session", Duration::from_millis(7));
        child_profiler.record_vm_worker_libkrun_enter(Duration::from_millis(5));
        child_profiler.record_duration("unrelated_child_phase", Duration::from_millis(3));
        child_profiler
            .write_vm_worker_wait_details(&task_state_dir)
            .expect("child profile artifact should write");

        let mut parent_profiler = CangHostProfiler::new_started_at(true, Instant::now());
        parent_profiler.record_vm_worker_wait_details(&task_state_dir, Duration::from_millis(12));

        let mut output = Vec::new();
        parent_profiler
            .write_report_with_total(&mut output, Duration::from_millis(11))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");

        assert!(text.contains("helper_wait_vm_worker_child_config_read: 1.000ms"));
        assert!(text.contains("helper_wait_vm_worker_child_libkrun_configure: 2.000ms"));
        assert!(text.contains("helper_wait_vm_worker_child_libkrun_enter: 5.000ms"));
        assert!(text.contains("helper_wait_vm_worker_child_unattributed: 4.000ms"));
        assert!(!text.contains("helper_wait_vm_worker_child_libkrun_session"));
        assert!(!text.contains("unrelated_child_phase"));

        fs::remove_dir_all(cleanup_dir).expect("temp task state dir should be removed");
    }

    #[test]
    fn host_profile_reads_libkrun_internal_profile_as_standalone_ns_rows() {
        let task_state_dir = unique_temp_dir("cang-profile-libkrun-details");
        fs::create_dir_all(&task_state_dir).expect("temp task state dir should be created");
        let cleanup_dir = task_state_dir.clone();

        fs::write(
            vm_worker_wait_detail_path(&task_state_dir),
            concat!(
                "vm_worker_config_read\t1000000\n",
                "libkrun_start_enter_event_manager_create\t2000000\n",
                "libkrun_start_enter_configure_block\t3000000\n",
                "libkrun_build_microvm_create_guest_memory\t4000000\n",
                "libkrun_start_enter_event_loop_runtime\t0\n",
                "libkrun_future_phase\t5000000\n",
            ),
        )
        .expect("profile artifact should write");

        let mut parent_profiler = CangHostProfiler::new_started_at(true, Instant::now());
        parent_profiler.record_vm_worker_wait_details(&task_state_dir, Duration::from_millis(20));

        let mut output = Vec::new();
        parent_profiler
            .write_report_with_total(&mut output, Duration::from_millis(21))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");

        assert!(text.contains("helper_wait_vm_worker_child_config_read: 1.000ms"));
        assert!(text.contains("helper_wait_vm_worker_child_unattributed: 19.000ms"));
        assert!(text.contains("libkrun profile\n"));
        assert!(text.contains("libkrun_start_enter_event_manager_create: 2000000ns (2.000ms)"));
        assert!(text.contains("libkrun_start_enter_configure_block: 3000000ns (3.000ms)"));
        assert!(text.contains("libkrun_build_microvm_create_guest_memory: 4000000ns (4.000ms)"));
        assert!(text.contains("libkrun_start_enter_event_loop_runtime: 0ns (0.000ms)"));
        assert!(text.contains("libkrun_future_phase: 5000000ns (5.000ms)"));
        assert!(!text.contains("helper_wait_vm_worker_child_libkrun_event_manager_create"));
        assert!(!text.contains("helper_wait_vm_worker_child_libkrun_configure_block"));

        fs::remove_dir_all(cleanup_dir).expect("temp task state dir should be removed");
    }

    #[test]
    fn vm_worker_wait_detail_rewrite_preserves_raw_libkrun_rows() {
        let task_state_dir = unique_temp_dir("cang-profile-preserve-libkrun-details");
        fs::create_dir_all(&task_state_dir).expect("temp task state dir should be created");
        let cleanup_dir = task_state_dir.clone();

        fs::write(
            vm_worker_wait_detail_path(&task_state_dir),
            "libkrun_start_enter_configure_block\t3000000\n",
        )
        .expect("profile artifact should write");

        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());
        profiler.record_duration("vm_worker_config_read", Duration::from_millis(1));
        profiler
            .write_vm_worker_wait_details(&task_state_dir)
            .expect("child detail artifact rewrite should preserve libkrun rows");

        let text = fs::read_to_string(vm_worker_wait_detail_path(&task_state_dir))
            .expect("profile artifact should be readable");

        assert!(text.contains("vm_worker_config_read\t1000000\n"));
        assert!(text.contains("libkrun_start_enter_configure_block\t3000000\n"));

        fs::remove_dir_all(cleanup_dir).expect("temp task state dir should be removed");
    }

    #[test]
    fn host_profile_ignores_missing_vm_worker_wait_child_details() {
        let task_state_dir = unique_temp_dir("cang-profile-missing-vm-worker-details");
        let mut profiler = CangHostProfiler::new_started_at(true, Instant::now());

        profiler.record_vm_worker_wait_details(&task_state_dir, Duration::from_millis(10));

        let mut output = Vec::new();
        profiler
            .write_report_with_total(&mut output, Duration::from_millis(11))
            .expect("report should write");
        let text = String::from_utf8(output).expect("report should be utf-8");

        assert!(!text.contains("helper_wait_vm_worker_child_unattributed"));
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
    }
}
