//! Host-side seccomp audit, synthesis, and enforcement support.

use crate::runtime::host_tools;
use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::{Path, PathBuf};

pub(crate) const AUDIT_START_MARKER_NAME: &str = "cang-seccomp-audit-start";
const AUDIT_START_MARKER_SYSCALL: &str = "memfd_create";
const LINEAGE_SYSCALLS: &[&str] = &["clone", "clone3", "fork", "vfork"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum SeccompMode {
    #[default]
    Off,
    Audit(AuditMode),
    Enforce {
        policy_path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuditMode {
    Full {
        trace_path: PathBuf,
    },
    Gap {
        baseline_policy_path: PathBuf,
        trace_path: PathBuf,
    },
    DefaultGap {
        trace_path: PathBuf,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct CompiledEnforcePolicy {
    policy_path: PathBuf,
    main_thread: seccompiler::BpfProgram,
}

impl AuditMode {
    pub(crate) fn trace_path(&self) -> &Path {
        match self {
            Self::Full { trace_path }
            | Self::Gap { trace_path, .. }
            | Self::DefaultGap { trace_path } => trace_path,
        }
    }

    pub(crate) fn baseline_policy_path(&self) -> Option<&Path> {
        match self {
            Self::Gap {
                baseline_policy_path,
                ..
            } => Some(baseline_policy_path),
            Self::Full { .. } => None,
            Self::DefaultGap { .. } => {
                panic!("unresolved default seccomp gap audit cannot be serialized")
            }
        }
    }
}

impl SeccompMode {
    pub(crate) fn as_config_value(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Audit(_) => "audit",
            Self::Enforce { .. } => "enforce",
        }
    }

    pub(crate) fn parse_config_value(
        mode: Option<&str>,
        audit_trace_path: Option<&str>,
        audit_baseline_policy_path: Option<&str>,
        enforce_policy_path: Option<&str>,
    ) -> Result<Self> {
        match mode.unwrap_or("off") {
            "off" => {
                if audit_trace_path.is_some()
                    || audit_baseline_policy_path.is_some()
                    || enforce_policy_path.is_some()
                {
                    bail!("cang launch config seccomp off mode rejects seccomp path fields");
                }
                Ok(Self::Off)
            }
            "audit" => {
                if enforce_policy_path.is_some() {
                    bail!("cang launch config audit mode rejects seccomp.enforce_policy_path");
                }
                let trace_path = PathBuf::from(audit_trace_path.ok_or_else(|| {
                    anyhow!("cang launch config seccomp.audit_trace_path is required")
                })?);
                Ok(Self::Audit(match audit_baseline_policy_path {
                    Some(path) => AuditMode::Gap {
                        baseline_policy_path: PathBuf::from(path),
                        trace_path,
                    },
                    None => AuditMode::Full { trace_path },
                }))
            }
            "enforce" => {
                if audit_trace_path.is_some() || audit_baseline_policy_path.is_some() {
                    bail!("cang launch config enforce mode rejects seccomp audit path fields");
                }
                Ok(Self::Enforce {
                    policy_path: PathBuf::from(enforce_policy_path.ok_or_else(|| {
                        anyhow!("cang launch config seccomp.enforce_policy_path is required")
                    })?),
                })
            }
            _ => bail!("cang launch config seccomp.mode is invalid"),
        }
    }

    pub(crate) fn audit_trace_path(&self) -> Option<&Path> {
        match self {
            Self::Audit(mode) => Some(mode.trace_path()),
            Self::Off | Self::Enforce { .. } => None,
        }
    }

    pub(crate) fn audit_baseline_policy_path(&self) -> Option<&Path> {
        match self {
            Self::Audit(mode) => mode.baseline_policy_path(),
            Self::Off | Self::Enforce { .. } => None,
        }
    }

    pub(crate) fn enforce_policy_path(&self) -> Option<&Path> {
        match self {
            Self::Enforce { policy_path } => Some(policy_path),
            Self::Off | Self::Audit(_) => None,
        }
    }
}

#[derive(Debug, Clone, Subcommand, PartialEq, Eq)]
pub(crate) enum SeccompCommand {
    #[command(
        name = "synthesize",
        about = "Synthesize a seccompiler allowlist policy from a cang seccomp audit trace"
    )]
    Synthesize {
        #[arg(long = "input", value_name = "TRACE_JSONL")]
        input: PathBuf,
        #[arg(long = "output", value_name = "POLICY_JSON")]
        output: PathBuf,
    },

    #[command(
        name = "extend",
        about = "Add missing audited syscalls to an existing seccompiler allowlist policy"
    )]
    Extend {
        #[arg(
            long = "policy",
            value_name = "BASELINE_POLICY_JSON",
            required_unless_present = "default_policy",
            conflicts_with = "default_policy"
        )]
        policy: Option<PathBuf>,
        #[arg(long = "default-policy", conflicts_with = "policy")]
        default_policy: bool,
        #[arg(long = "trace", value_name = "MISSING_TRACE_JSONL")]
        trace: PathBuf,
        #[arg(long = "output", value_name = "UPDATED_POLICY_JSON")]
        output: PathBuf,
    },
}

pub(crate) fn run_seccomp_command(command: SeccompCommand) -> Result<String> {
    run_seccomp_command_with_default_policy_path(command, host_tools::default_seccomp_policy_path)
}

pub(crate) fn run_seccomp_command_with_default_policy_path(
    command: SeccompCommand,
    default_policy_path: impl FnOnce() -> Option<PathBuf>,
) -> Result<String> {
    match command {
        SeccompCommand::Synthesize { input, output } => {
            synthesize_policy(&input, &output)?;
            Ok(format!(
                "wrote seccomp policy '{}' from '{}'\n",
                output.display(),
                input.display()
            ))
        }
        SeccompCommand::Extend {
            policy,
            default_policy,
            trace,
            output,
        } => {
            let policy =
                resolve_extend_baseline_policy(policy, default_policy, default_policy_path)?;
            extend_policy(&policy, &trace, &output)?;
            Ok(format!(
                "wrote extended seccomp policy '{}' from policy '{}' and trace '{}'\n",
                output.display(),
                policy.display(),
                trace.display()
            ))
        }
    }
}

pub(crate) fn resolve_default_seccomp_policy_path(
    default_policy_path: impl FnOnce() -> Option<PathBuf>,
    usage: &str,
    recovery_hint: &str,
) -> Result<PathBuf> {
    let policy_path = default_policy_path().ok_or_else(|| {
        anyhow!(
            "cang default seccomp policy is unavailable for {usage}; \
             install cang with share/cang/seccomp/default.json or {recovery_hint}"
        )
    })?;
    validate_seccomp_policy_file(&policy_path, "default cang seccomp policy").with_context(
        || {
            format!(
                "failed to load default cang seccomp policy '{}' for {usage}; {recovery_hint}",
                policy_path.display()
            )
        },
    )?;
    Ok(policy_path)
}

fn resolve_extend_baseline_policy(
    policy: Option<PathBuf>,
    default_policy: bool,
    default_policy_path: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf> {
    match (policy, default_policy) {
        (Some(policy), false) => Ok(policy),
        (None, true) => resolve_default_seccomp_policy_path(
            default_policy_path,
            "seccomp extend --default-policy",
            "pass --policy BASELINE_POLICY_JSON explicitly",
        ),
        (Some(_), true) => {
            bail!("seccomp extend requires exactly one of --policy or --default-policy")
        }
        (None, false) => {
            bail!("seccomp extend requires exactly one of --policy or --default-policy")
        }
    }
}

pub(crate) fn raw_strace_path(trace_path: &Path) -> PathBuf {
    let mut name = trace_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "cang-seccomp-trace".into());
    name.push(".strace");
    trace_path.with_file_name(name)
}

pub(crate) fn ptrace_failure_hint() -> &'static str {
    "seccomp audit mode uses strace/ptrace on the cang VM worker only; normal child tracing should work with `kernel.yama.ptrace_scope=1`, but hosts that disable ptrace entirely must allow ptrace for the audit run"
}

pub(crate) fn prepare_audit_trace_target(trace_path: &Path) -> Result<()> {
    if let Some(parent) = trace_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create seccomp trace dir '{}'", parent.display())
        })?;
    }
    let raw_path = raw_strace_path(trace_path);
    if let Some(parent) = raw_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create raw seccomp trace dir '{}'",
                parent.display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn emit_audit_start_marker() -> Result<()> {
    let name = std::ffi::CString::new(AUDIT_START_MARKER_NAME)
        .context("seccomp audit start marker name contains an interior NUL")?;
    // SAFETY: syscall is invoked with a valid NUL-terminated marker name and the
    // close-on-exec flag. A successful marker fd is immediately closed below.
    let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to emit seccomp audit start marker with memfd_create");
    }

    // SAFETY: fd came from a successful memfd_create call in this function.
    let rc = unsafe { libc::close(fd as libc::c_int) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
            .context("failed to close seccomp audit start marker memfd")
    }
}

pub(crate) fn finalize_audit_trace_with_baseline(
    trace_path: &Path,
    baseline_policy_path: Option<&Path>,
) -> Result<()> {
    let raw_path = raw_strace_path(trace_path);
    if let Some(parent) = trace_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create seccomp trace dir '{}'", parent.display())
        })?;
    }
    let baseline_syscalls = baseline_policy_path
        .map(allowed_syscalls_from_policy)
        .transpose()?;
    let temp_path = finalized_trace_temp_path(trace_path);
    match write_finalized_audit_trace(&raw_path, &temp_path, baseline_syscalls.as_ref()) {
        Ok(()) => fs::rename(&temp_path, trace_path).with_context(|| {
            format!(
                "failed to publish finalized seccomp trace '{}' from '{}'",
                trace_path.display(),
                temp_path.display()
            )
        }),
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

#[derive(Debug, Clone)]
struct AuditTraceScope {
    marker_pid: u32,
    marker_fd_to_skip_close: Option<i32>,
    lineage_pids: BTreeSet<u32>,
}

fn write_finalized_audit_trace(
    raw_path: &Path,
    output_path: &Path,
    baseline_syscalls: Option<&BTreeSet<String>>,
) -> Result<()> {
    let scope = audit_trace_scope(raw_path)?;
    let raw = fs::File::open(raw_path)
        .with_context(|| format!("failed to open raw strace log '{}'", raw_path.display()))?;
    let mut out = fs::File::create(output_path)
        .with_context(|| format!("failed to create seccomp trace '{}'", output_path.display()))?;
    let mut marker_seen = false;
    let mut marker_fd_to_skip_close = scope.marker_fd_to_skip_close;
    for line in BufReader::new(raw).lines() {
        let line = line.with_context(|| format!("failed to read '{}'", raw_path.display()))?;
        if !marker_seen {
            if is_audit_start_marker_line(&line) {
                marker_seen = true;
            }
            continue;
        }

        let Some(line_pid) = strace_line_pid(&line) else {
            continue;
        };
        if !scope.lineage_pids.contains(&line_pid) {
            continue;
        }
        if line_pid == scope.marker_pid
            && marker_fd_to_skip_close.is_some_and(|fd| is_close_of_fd_line(&line, fd))
        {
            marker_fd_to_skip_close = None;
            continue;
        }
        let Some(syscall) = syscall_from_strace_line(&line) else {
            continue;
        };
        if baseline_syscalls.is_some_and(|syscalls| syscalls.contains(&syscall)) {
            continue;
        }
        let record = TraceRecord { syscall, raw: line };
        serde_json::to_writer(&mut out, &record)
            .context("failed to encode seccomp audit trace record")?;
        out.write_all(b"\n")
            .context("failed to write seccomp audit trace record")?;
    }
    out.flush()
        .context("failed to flush seccomp audit trace records")?;
    out.sync_all()
        .context("failed to sync seccomp audit trace records")?;
    Ok(())
}

fn audit_trace_scope(raw_path: &Path) -> Result<AuditTraceScope> {
    let raw = fs::File::open(raw_path)
        .with_context(|| format!("failed to open raw strace log '{}'", raw_path.display()))?;
    let mut marker_pid = None;
    let mut marker_fd_to_skip_close = None;
    let mut lineage_edges = Vec::new();
    for line in BufReader::new(raw).lines() {
        let line = line.with_context(|| format!("failed to read '{}'", raw_path.display()))?;
        let line_pid = strace_line_pid(&line);
        if marker_pid.is_none() {
            if is_audit_start_marker_line(&line) {
                let pid = line_pid.ok_or_else(|| {
                    anyhow!(
                        "raw seccomp strace log '{}' start marker '{}' did not contain traced PID; refusing to publish unscoped JSONL trace",
                        raw_path.display(),
                        AUDIT_START_MARKER_NAME
                    )
                })?;
                marker_pid = Some(pid);
                marker_fd_to_skip_close = audit_start_marker_fd(&line);
            }
            continue;
        }

        let Some(parent_pid) = line_pid else {
            continue;
        };
        if let Some(child_pid) = lineage_child_pid_from_strace_line(&line) {
            lineage_edges.push((parent_pid, child_pid));
        }
    }
    let Some(marker_pid) = marker_pid else {
        bail!(
            "raw seccomp strace log '{}' did not contain audit start marker '{}'; refusing to publish unscoped JSONL trace",
            raw_path.display(),
            AUDIT_START_MARKER_NAME
        );
    };
    let mut lineage_pids = BTreeSet::from([marker_pid]);
    loop {
        let mut added = false;
        for (parent_pid, child_pid) in &lineage_edges {
            if lineage_pids.contains(parent_pid) && lineage_pids.insert(*child_pid) {
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    Ok(AuditTraceScope {
        marker_pid,
        marker_fd_to_skip_close,
        lineage_pids,
    })
}

fn finalized_trace_temp_path(trace_path: &Path) -> PathBuf {
    let mut name = trace_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "cang-seccomp-trace".into());
    name.push(format!(".tmp-{}", std::process::id()));
    trace_path.with_file_name(name)
}

pub(crate) fn synthesize_policy(input: &Path, output: &Path) -> Result<()> {
    let syscalls = syscalls_from_trace(input)?;
    if syscalls.is_empty() {
        bail!(
            "seccomp trace '{}' did not contain any syscalls",
            input.display()
        );
    }
    let policy = SeccompilerPolicy {
        main_thread: ThreadPolicy {
            mismatch_action: "trap".to_owned(),
            match_action: "allow".to_owned(),
            filter: syscalls
                .into_iter()
                .map(|syscall| SyscallRule { syscall })
                .collect(),
        },
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create seccomp policy dir '{}'", parent.display())
        })?;
    }
    let mut file = fs::File::create(output)
        .with_context(|| format!("failed to create seccomp policy '{}'", output.display()))?;
    serde_json::to_writer_pretty(&mut file, &policy).context("failed to write seccomp policy")?;
    file.write_all(b"\n")
        .context("failed to finish seccomp policy")?;
    Ok(())
}

pub(crate) fn strace_exclusion_filter_from_policy(policy_path: &Path) -> Result<Option<String>> {
    let mut syscalls = allowed_syscalls_from_policy(policy_path)?;
    if syscalls.is_empty() {
        bail!(
            "seccomp gap audit baseline policy '{}' has an empty main_thread.filter allowlist",
            policy_path.display()
        );
    }
    syscalls.remove(AUDIT_START_MARKER_SYSCALL);
    for syscall in LINEAGE_SYSCALLS {
        syscalls.remove(*syscall);
    }
    if syscalls.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "trace=!{}",
        syscalls.into_iter().collect::<Vec<_>>().join(",")
    )))
}

pub(crate) fn allowed_syscalls_from_policy(policy_path: &Path) -> Result<BTreeSet<String>> {
    let text = fs::read_to_string(policy_path).with_context(|| {
        format!(
            "failed to read seccomp baseline policy '{}'",
            policy_path.display()
        )
    })?;
    let policy = serde_json::from_str::<serde_json::Value>(&text).with_context(|| {
        format!(
            "failed to parse seccomp baseline policy '{}'",
            policy_path.display()
        )
    })?;
    allowed_syscalls_from_policy_value(&policy).with_context(|| {
        format!(
            "failed to inspect seccomp baseline policy '{}'",
            policy_path.display()
        )
    })
}

fn allowed_syscalls_from_policy_value(policy: &serde_json::Value) -> Result<BTreeSet<String>> {
    let filter = policy
        .get("main_thread")
        .and_then(|value| value.get("filter"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("seccomp policy main_thread.filter must be an array"))?;
    let mut syscalls = BTreeSet::new();
    for (index, rule) in filter.iter().enumerate() {
        let syscall = rule
            .get("syscall")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow!("seccomp policy main_thread.filter[{index}].syscall must be a string")
            })?;
        if !valid_syscall_name(syscall) {
            bail!(
                "invalid syscall name '{}' in seccomp policy main_thread.filter[{index}]",
                syscall
            );
        }
        syscalls.insert(syscall.to_owned());
    }
    Ok(syscalls)
}

fn main_thread_action(policy: &serde_json::Value, key: &str) -> Result<String> {
    policy
        .get("main_thread")
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("seccomp policy main_thread.{key} must be a string"))
}

pub(crate) fn extend_policy(policy: &Path, trace: &Path, output: &Path) -> Result<()> {
    if policy == output {
        bail!(
            "refusing to overwrite baseline seccomp policy '{}'",
            policy.display()
        );
    }
    let text = fs::read_to_string(policy).with_context(|| {
        format!(
            "failed to read baseline seccomp policy '{}'",
            policy.display()
        )
    })?;
    let policy_value = serde_json::from_str::<serde_json::Value>(&text).with_context(|| {
        format!(
            "failed to parse baseline seccomp policy '{}'",
            policy.display()
        )
    })?;
    let mut syscalls = allowed_syscalls_from_policy_value(&policy_value).with_context(|| {
        format!(
            "failed to inspect baseline seccomp policy '{}'",
            policy.display()
        )
    })?;
    syscalls.extend(syscalls_from_trace(trace)?);

    let policy = SeccompilerPolicy {
        main_thread: ThreadPolicy {
            mismatch_action: main_thread_action(&policy_value, "mismatch_action")?,
            match_action: main_thread_action(&policy_value, "match_action")?,
            filter: syscalls
                .into_iter()
                .map(|syscall| SyscallRule { syscall })
                .collect(),
        },
    };

    let mut bytes = Vec::new();
    serde_json::to_writer_pretty(&mut bytes, &policy)
        .context("failed to encode extended seccomp policy")?;
    bytes.push(b'\n');
    validate_seccomp_policy_bytes(&bytes, output, "extended seccomp policy")?;

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create seccomp policy dir '{}'", parent.display())
        })?;
    }
    let mut file = fs::File::create(output).with_context(|| {
        format!(
            "failed to create extended seccomp policy '{}'",
            output.display()
        )
    })?;
    file.write_all(&bytes)
        .context("failed to write extended seccomp policy")?;
    Ok(())
}

pub(crate) fn validate_seccomp_policy_file(policy_path: &Path, description: &str) -> Result<()> {
    let bytes = fs::read(policy_path)
        .with_context(|| format!("failed to read {description} '{}'", policy_path.display()))?;
    validate_seccomp_policy_bytes(&bytes, policy_path, description)
}

fn validate_seccomp_policy_bytes(
    policy_bytes: &[u8],
    policy_path: &Path,
    description: &str,
) -> Result<()> {
    let arch = std::env::consts::ARCH.try_into().map_err(|_| {
        anyhow!(
            "seccomp does not support host architecture {}",
            std::env::consts::ARCH
        )
    })?;
    seccompiler::compile_from_json(Cursor::new(policy_bytes), arch).with_context(|| {
        format!(
            "{description} '{}' is not valid for seccompiler",
            policy_path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn compile_enforce_policy(policy_path: &Path) -> Result<CompiledEnforcePolicy> {
    let policy = fs::File::open(policy_path)
        .with_context(|| format!("failed to open seccomp policy '{}'", policy_path.display()))?;
    let arch = std::env::consts::ARCH.try_into().map_err(|_| {
        anyhow!(
            "seccomp does not support host architecture {}",
            std::env::consts::ARCH
        )
    })?;
    let mut filters = seccompiler::compile_from_json(policy, arch).with_context(|| {
        format!(
            "failed to compile seccomp policy '{}'",
            policy_path.display()
        )
    })?;
    let main_thread = filters
        .remove("main_thread")
        .ok_or_else(|| anyhow!("seccomp policy must contain a main_thread filter"))?;
    Ok(CompiledEnforcePolicy {
        policy_path: policy_path.to_path_buf(),
        main_thread,
    })
}

pub(crate) fn apply_compiled_enforce_policy(policy: &CompiledEnforcePolicy) -> Result<()> {
    set_no_new_privs()?;
    seccompiler::apply_filter(&policy.main_thread).with_context(|| {
        format!(
            "failed to install seccomp policy '{}'",
            policy.policy_path.display()
        )
    })
}

fn set_no_new_privs() -> Result<()> {
    // SAFETY: prctl is called with PR_SET_NO_NEW_PRIVS and constant integer
    // arguments before loading the seccomp filter in the current VM worker.
    let rc = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("failed to set PR_SET_NO_NEW_PRIVS")
    }
}

fn syscalls_from_trace(input: &Path) -> Result<BTreeSet<String>> {
    let file = fs::File::open(input)
        .with_context(|| format!("failed to open seccomp trace '{}'", input.display()))?;
    let mut syscalls = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("failed to read '{}'", input.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with('{') {
            let record = serde_json::from_str::<TraceRecord>(&line).with_context(|| {
                format!(
                    "invalid seccomp JSONL trace record in '{}'",
                    input.display()
                )
            })?;
            if !valid_syscall_name(&record.syscall) {
                bail!(
                    "invalid syscall name '{}' in seccomp trace '{}'",
                    record.syscall,
                    input.display()
                );
            }
            syscalls.insert(record.syscall);
        } else if let Some(syscall) = syscall_from_strace_line(&line) {
            syscalls.insert(syscall);
        }
    }
    Ok(syscalls)
}

fn syscall_from_strace_line(line: &str) -> Option<String> {
    let line = strip_pid_prefix(line.trim());
    if line.is_empty() || line.starts_with("+++") || line.starts_with("---") {
        return None;
    }
    if let Some(rest) = line.strip_prefix("<... ") {
        let syscall = rest.split_whitespace().next()?.trim();
        return valid_syscall_name(syscall).then(|| syscall.to_owned());
    }
    let open_paren = line.find('(')?;
    let syscall = line[..open_paren].trim();
    valid_syscall_name(syscall).then(|| syscall.to_owned())
}

fn lineage_child_pid_from_strace_line(line: &str) -> Option<u32> {
    let syscall = syscall_from_strace_line(line)?;
    if !LINEAGE_SYSCALLS.contains(&syscall.as_str()) {
        return None;
    }
    let (_, result) = strip_pid_prefix(line.trim()).rsplit_once(" = ")?;
    let pid = result.split_whitespace().next()?.parse::<i64>().ok()?;
    u32::try_from(pid).ok().filter(|pid| *pid > 0)
}

fn is_audit_start_marker_line(line: &str) -> bool {
    let line = strip_pid_prefix(line.trim());
    let marker_argument = format!("\"{AUDIT_START_MARKER_NAME}\"");
    line.starts_with(AUDIT_START_MARKER_SYSCALL)
        && line.contains(&marker_argument)
        && syscall_from_strace_line(line).as_deref() == Some(AUDIT_START_MARKER_SYSCALL)
}

fn audit_start_marker_fd(line: &str) -> Option<i32> {
    if !is_audit_start_marker_line(line) {
        return None;
    }
    let (_, result) = strip_pid_prefix(line.trim()).rsplit_once(" = ")?;
    result.split_whitespace().next()?.parse().ok()
}

fn is_close_of_fd_line(line: &str, fd: i32) -> bool {
    let line = strip_pid_prefix(line.trim());
    let Some(rest) = line.strip_prefix("close(") else {
        return false;
    };
    rest.strip_prefix(&fd.to_string())
        .is_some_and(|rest| rest.starts_with(')'))
}

fn strace_line_pid(line: &str) -> Option<u32> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("[pid ") {
        let (pid, _) = rest.split_once(']')?;
        return pid.trim().parse().ok();
    }

    let (pid, _) = line.split_once(char::is_whitespace)?;
    if !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()) {
        pid.parse().ok()
    } else {
        None
    }
}

fn strip_pid_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("[pid ") {
        let Some((_, after)) = rest.split_once(']') else {
            return line;
        };
        return after.trim_start();
    }

    let Some((pid, after)) = line.split_once(char::is_whitespace) else {
        return line;
    };
    if !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()) {
        after.trim_start()
    } else {
        line
    }
}

fn valid_syscall_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TraceRecord {
    syscall: String,
    raw: String,
}

#[derive(Debug, Clone, Serialize)]
struct SeccompilerPolicy {
    main_thread: ThreadPolicy,
}

#[derive(Debug, Clone, Serialize)]
struct ThreadPolicy {
    mismatch_action: String,
    match_action: String,
    filter: Vec<SyscallRule>,
}

#[derive(Debug, Clone, Serialize)]
struct SyscallRule {
    syscall: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_strace_lines() {
        assert_eq!(
            syscall_from_strace_line("[pid 123] openat(AT_FDCWD, \"/tmp\", O_RDONLY) = 3"),
            Some("openat".to_owned())
        );
        assert_eq!(
            syscall_from_strace_line(
                "40860 execve(\"/nix/store/bin/cang\", [\"cang\"], 0x1234) = 0"
            ),
            Some("execve".to_owned())
        );
        assert_eq!(
            syscall_from_strace_line("<... read resumed> \"\", 8192) = 0"),
            Some("read".to_owned())
        );
        assert_eq!(syscall_from_strace_line("+++ exited with 0 +++"), None);
    }

    #[test]
    fn parses_strace_pid_prefixes() {
        assert_eq!(
            strace_line_pid("[pid 123] openat(AT_FDCWD, \"/tmp\", O_RDONLY) = 3"),
            Some(123)
        );
        assert_eq!(
            strace_line_pid("40860 execve(\"/nix/store/bin/cang\", [\"cang\"], 0x1234) = 0"),
            Some(40860)
        );
        assert_eq!(
            strace_line_pid("openat(AT_FDCWD, \"/tmp\", O_RDONLY) = 3"),
            None
        );
        assert_eq!(strace_line_pid("+++ exited with 0 +++"), None);
    }

    #[test]
    fn parses_lineage_child_pid_from_strace_lines() {
        assert_eq!(
            lineage_child_pid_from_strace_line("4106 clone3({flags=CLONE_VM}, 88) = 4108"),
            Some(4108)
        );
        assert_eq!(
            lineage_child_pid_from_strace_line(
                "4106 <... clone3 resumed>{flags=CLONE_VM}, 88) = 4109"
            ),
            Some(4109)
        );
        assert_eq!(
            lineage_child_pid_from_strace_line("[pid 4108] clone(NULL) = 4136"),
            Some(4136)
        );
        assert_eq!(
            lineage_child_pid_from_strace_line("[pid 4108] fork() = 4137"),
            Some(4137)
        );
        assert_eq!(
            lineage_child_pid_from_strace_line("[pid 4108] vfork() = 4138"),
            Some(4138)
        );
        assert_eq!(
            lineage_child_pid_from_strace_line("4106 clone3({flags=CLONE_VM}, 88) = -1 EPERM"),
            None
        );
        assert_eq!(
            lineage_child_pid_from_strace_line("4106 read(0, \"\", 1) = 0"),
            None
        );
    }

    #[test]
    fn detects_audit_start_marker_with_pid_prefix() {
        assert!(is_audit_start_marker_line(
            "[pid 123] memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 5"
        ));
        assert!(is_audit_start_marker_line(
            "123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 5"
        ));
        assert!(!is_audit_start_marker_line(
            "[pid 123] memfd_create(\"other\", MFD_CLOEXEC) = 5"
        ));
        assert!(!is_audit_start_marker_line(
            "[pid 123] memfd_create(\"cang-seccomp-audit-start-extra\", MFD_CLOEXEC) = 5"
        ));
        assert!(!is_audit_start_marker_line(
            "[pid 123] openat(AT_FDCWD, \"cang-seccomp-audit-start\", O_RDONLY) = 5"
        ));
    }

    #[test]
    fn prepares_audit_trace_and_raw_trace_parent_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("nested").join("trace.jsonl");

        prepare_audit_trace_target(&trace).expect("prepare trace target");

        assert!(trace.parent().expect("trace parent").is_dir());
        assert!(
            raw_strace_path(&trace)
                .parent()
                .expect("raw parent")
                .is_dir()
        );
    }

    #[test]
    fn finalizes_raw_strace_to_jsonl_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(
            &raw,
            "122 landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) = 6\n\
             123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             123 close(7) = 0\n\
             124 read(0, \"other tid\", 9) = 9\n\
             123 read(0, \"\", 1) = 0\n\
             123 write(1, \"x\", 1) = 1\n\
             122 umount2(\"/tmp/cang/root\", 0) = 0\n\
             123 exit_group(0) = ?\n",
        )
        .expect("write raw trace");

        finalize_audit_trace_with_baseline(&trace, None).expect("finalize trace");

        let syscalls = syscalls_from_trace(&trace).expect("read finalized trace");
        assert_eq!(
            syscalls,
            BTreeSet::from([
                "exit_group".to_owned(),
                "read".to_owned(),
                "write".to_owned()
            ])
        );
        assert!(!finalized_trace_temp_path(&trace).exists());
    }

    #[test]
    fn finalizes_marker_descendant_lineage_syscalls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(
            &raw,
            "122 landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) = 6\n\
             123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             123 close(7) = 0\n\
             123 clone3({flags=CLONE_VM}, 88) = 124\n\
             124 read(0, \"child\", 5) = 5\n\
             124 clone(NULL) = 125\n\
             125 write(1, \"grandchild\", 10) = 10\n\
             126 openat(AT_FDCWD, \"/unrelated\", O_RDONLY) = 8\n\
             122 umount2(\"/tmp/cang/root\", 0) = 0\n",
        )
        .expect("write raw trace");

        finalize_audit_trace_with_baseline(&trace, None).expect("finalize trace");

        let syscalls = syscalls_from_trace(&trace).expect("read finalized trace");
        assert_eq!(
            syscalls,
            BTreeSet::from([
                "clone".to_owned(),
                "clone3".to_owned(),
                "read".to_owned(),
                "write".to_owned()
            ])
        );
    }

    #[test]
    fn finalizes_fork_vfork_lineage_and_excludes_non_lineage_process_creation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(
            &raw,
            "123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             123 close(7) = 0\n\
             130 clone3({flags=CLONE_VM}, 88) = 131\n\
             131 read(0, \"non-lineage clone child\", 23) = 23\n\
             132 fork() = 133\n\
             133 openat(AT_FDCWD, \"/non-lineage-fork-child\", O_RDONLY) = 8\n\
             123 fork() = 124\n\
             124 vfork() = 125\n\
             125 write(1, \"lineage\", 7) = 7\n",
        )
        .expect("write raw trace");

        finalize_audit_trace_with_baseline(&trace, None).expect("finalize trace");

        let syscalls = syscalls_from_trace(&trace).expect("read finalized trace");
        assert_eq!(
            syscalls,
            BTreeSet::from(["fork".to_owned(), "vfork".to_owned(), "write".to_owned()])
        );
        let trace_text = fs::read_to_string(&trace).expect("read finalized trace text");
        assert!(!trace_text.contains("non-lineage"));
    }

    #[test]
    fn finalization_includes_child_lines_seen_before_clone_return() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(
            &raw,
            "123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             124 ioctl(3, KVM_RUN, 0) = 0\n\
             123 <... clone3 resumed>{flags=CLONE_VM}, 88) = 124\n\
             122 openat(AT_FDCWD, \"/unrelated\", O_RDONLY) = 8\n",
        )
        .expect("write raw trace");

        finalize_audit_trace_with_baseline(&trace, None).expect("finalize trace");

        let syscalls = syscalls_from_trace(&trace).expect("read finalized trace");
        assert_eq!(
            syscalls,
            BTreeSet::from(["clone3".to_owned(), "ioctl".to_owned()])
        );
    }

    #[test]
    fn gap_finalization_suppresses_baseline_lineage_syscalls_but_keeps_descendants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("denied.jsonl");
        let raw = raw_strace_path(&trace);
        let policy = dir.path().join("policy.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                  { "syscall": "memfd_create" },
                  { "syscall": "clone3" },
                  { "syscall": "clone" }
                ]
              }
            }"#,
        )
        .expect("write policy");
        fs::write(
            &raw,
            "123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             123 close(7) = 0\n\
             123 clone3({flags=CLONE_VM}, 88) = 124\n\
             124 clone(NULL) = 125\n\
             125 ioctl(3, KVM_RUN, 0) = 0\n\
             126 openat(AT_FDCWD, \"/unrelated\", O_RDONLY) = 8\n",
        )
        .expect("write raw trace");

        finalize_audit_trace_with_baseline(&trace, Some(&policy)).expect("finalize gap trace");

        let syscalls = syscalls_from_trace(&trace).expect("read finalized trace");
        assert_eq!(syscalls, BTreeSet::from(["ioctl".to_owned()]));
    }

    #[test]
    fn finalization_fails_closed_when_start_marker_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(&trace, "old finalized trace\n").expect("write old trace");
        fs::write(&raw, "123 read(0, \"\", 1) = 0\n").expect("write raw trace");

        let err = finalize_audit_trace_with_baseline(&trace, None)
            .expect_err("missing marker should fail");

        assert!(format!("{err:#}").contains("did not contain audit start marker"));
        assert_eq!(
            fs::read_to_string(&trace).expect("old trace should remain"),
            "old finalized trace\n"
        );
        assert!(!finalized_trace_temp_path(&trace).exists());
    }

    #[test]
    fn finalization_fails_closed_when_start_marker_pid_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(&trace, "old finalized trace\n").expect("write old trace");
        fs::write(
            &raw,
            "memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\
             123 close(7) = 0\n\
             123 read(0, \"\", 1) = 0\n",
        )
        .expect("write raw trace");

        let err = finalize_audit_trace_with_baseline(&trace, None)
            .expect_err("missing marker pid should fail");

        assert!(format!("{err:#}").contains("did not contain traced PID"));
        assert_eq!(
            fs::read_to_string(&trace).expect("old trace should remain"),
            "old finalized trace\n"
        );
        assert!(!finalized_trace_temp_path(&trace).exists());
    }

    #[test]
    fn finalization_failure_preserves_existing_trace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("trace.jsonl");
        let raw = raw_strace_path(&trace);
        fs::write(&trace, "old finalized trace\n").expect("write old trace");
        fs::write(
            &raw,
            b"123 memfd_create(\"cang-seccomp-audit-start\", MFD_CLOEXEC) = 7\n\xff\n",
        )
        .expect("write bad raw trace");

        let err = finalize_audit_trace_with_baseline(&trace, None)
            .expect_err("bad raw trace should fail");

        assert!(format!("{err:#}").contains("failed to read"));
        assert_eq!(
            fs::read_to_string(&trace).expect("old trace should remain"),
            "old finalized trace\n"
        );
        assert!(!finalized_trace_temp_path(&trace).exists());
    }

    #[test]
    fn packaged_default_seccomp_policy_compiles() {
        let policy = include_bytes!("../../assets/seccomp/default.json");
        let arch = std::env::consts::ARCH.try_into().expect("supported arch");
        let filters = seccompiler::compile_from_json(Cursor::new(policy), arch)
            .expect("default seccomp policy should compile");
        assert!(filters.contains_key("main_thread"));
    }

    #[test]
    fn packaged_render_server_seccomp_policy_compiles() {
        let policy = include_bytes!("../../assets/seccomp/render-server.json");
        let arch = std::env::consts::ARCH.try_into().expect("supported arch");
        let filters = seccompiler::compile_from_json(Cursor::new(policy), arch)
            .expect("render-server seccomp policy should compile");
        assert!(filters.contains_key("main_thread"));
    }

    #[test]
    fn packaged_default_seccomp_policy_excludes_post_vm_cleanup_syscalls() {
        let policy: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../assets/seccomp/default.json"))
                .expect("default seccomp policy should parse");
        let syscalls =
            allowed_syscalls_from_policy_value(&policy).expect("default policy should inspect");

        assert!(!syscalls.contains("umount2"));
    }

    #[test]
    fn packaged_default_seccomp_policy_allows_gpu_drm_worker_syscalls() {
        let policy: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../assets/seccomp/default.json"))
                .expect("default seccomp policy should parse");
        let syscalls =
            allowed_syscalls_from_policy_value(&policy).expect("default policy should inspect");

        for syscall in [
            "readlink",
            "uname",
            "sched_setaffinity",
            "setpriority",
            "sched_setscheduler",
            "gettid",
            "sysinfo",
        ] {
            assert!(
                syscalls.contains(syscall),
                "{syscall} is required by the gpu worker thread during --gpu=drm init when landlock=off"
            );
        }
    }

    #[test]
    fn packaged_default_seccomp_policy_allows_sigaltstack() {
        let policy: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../assets/seccomp/default.json"))
                .expect("default seccomp policy should parse");
        let syscalls =
            allowed_syscalls_from_policy_value(&policy).expect("default policy should inspect");

        assert!(
            syscalls.contains("sigaltstack"),
            "libkrun installs an alternate signal stack while building the VMM; without sigaltstack the enforced default policy kills the VM worker with SIGSYS"
        );
    }

    #[test]
    fn compile_enforce_policy_reads_policy_before_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy_path = dir.path().join("default.json");
        fs::write(
            &policy_path,
            include_bytes!("../../assets/seccomp/default.json"),
        )
        .expect("write default policy");

        let policy = compile_enforce_policy(&policy_path).expect("policy should compile");

        assert_eq!(policy.policy_path, policy_path);
        assert!(!policy.main_thread.is_empty());
    }

    #[test]
    fn synthesizes_deterministic_seccompiler_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("trace.jsonl");
        let output = dir.path().join("policy.json");
        fs::write(
            &input,
            "{\"syscall\":\"write\",\"raw\":\"write(1, \\\"x\\\", 1) = 1\"}\nread(0, \"\",\n",
        )
        .expect("write trace");

        synthesize_policy(&input, &output).expect("synthesize");

        let policy = fs::read_to_string(&output).expect("read policy");
        assert!(policy.contains("\"mismatch_action\": \"trap\""));
        assert!(policy.find("\"read\"").unwrap() < policy.find("\"write\"").unwrap());
        let file = fs::File::open(output).expect("open policy");
        let arch = std::env::consts::ARCH.try_into().expect("supported arch");
        let filters = seccompiler::compile_from_json(file, arch).expect("policy should compile");
        assert!(filters.contains_key("main_thread"));
    }

    #[test]
    fn synthesize_rejects_malformed_jsonl_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("trace.jsonl");
        let output = dir.path().join("policy.json");
        fs::write(&input, "{\"syscall\":\"read\",\"raw\":\"unterminated}\n").expect("write trace");

        let err = synthesize_policy(&input, &output).expect_err("malformed JSONL should fail");

        assert!(format!("{err:#}").contains("invalid seccomp JSONL trace record"));
    }

    #[test]
    fn extracts_allowed_syscalls_from_baseline_policy() {
        let policy = serde_json::json!({
            "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                    { "syscall": "write" },
                    { "syscall": "read" }
                ]
            }
        });

        let syscalls =
            allowed_syscalls_from_policy_value(&policy).expect("allowed syscalls should parse");

        assert_eq!(
            syscalls,
            BTreeSet::from(["read".to_owned(), "write".to_owned()])
        );
    }

    #[test]
    fn rejects_invalid_baseline_policy_filter_entries() {
        let missing_filter = serde_json::json!({
            "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow"
            }
        });
        let bad_name = serde_json::json!({
            "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                    { "syscall": "BadName" }
                ]
            }
        });

        let missing_err = allowed_syscalls_from_policy_value(&missing_filter)
            .expect_err("missing filter should fail");
        let bad_name_err =
            allowed_syscalls_from_policy_value(&bad_name).expect_err("bad syscall should fail");

        assert!(format!("{missing_err:#}").contains("main_thread.filter must be an array"));
        assert!(format!("{bad_name_err:#}").contains("invalid syscall name"));
    }

    #[test]
    fn gap_audit_rejects_empty_baseline_allowlist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("empty-policy.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": []
              }
            }"#,
        )
        .expect("write policy");

        let err =
            strace_exclusion_filter_from_policy(&policy).expect_err("empty allowlist should fail");

        assert!(format!("{err:#}").contains("empty main_thread.filter allowlist"));
    }

    #[test]
    fn builds_deterministic_gap_audit_strace_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("policy.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                  { "syscall": "write" },
                  { "syscall": "read" }
                ]
              }
            }"#,
        )
        .expect("write policy");

        let filter = strace_exclusion_filter_from_policy(&policy).expect("gap filter");

        assert_eq!(filter, Some("trace=!read,write".to_owned()));
    }

    #[test]
    fn gap_audit_strace_filter_keeps_marker_syscall_visible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("policy.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                  { "syscall": "write" },
                  { "syscall": "memfd_create" },
                  { "syscall": "clone3" },
                  { "syscall": "clone" },
                  { "syscall": "fork" },
                  { "syscall": "vfork" },
                  { "syscall": "read" }
                ]
              }
            }"#,
        )
        .expect("write policy");

        let filter = strace_exclusion_filter_from_policy(&policy).expect("gap filter");

        assert_eq!(filter, Some("trace=!read,write".to_owned()));
    }

    #[test]
    fn gap_audit_omits_filter_when_only_marker_and_lineage_syscalls_were_allowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("policy.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                  { "syscall": "memfd_create" },
                  { "syscall": "clone3" },
                  { "syscall": "clone" },
                  { "syscall": "fork" },
                  { "syscall": "vfork" }
                ]
              }
            }"#,
        )
        .expect("write policy");

        let filter = strace_exclusion_filter_from_policy(&policy).expect("gap filter");

        assert_eq!(filter, None);
    }

    #[test]
    fn extend_policy_adds_missing_syscalls_without_mutating_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("policy.json");
        let trace = dir.path().join("denied.jsonl");
        let output = dir.path().join("updated.json");
        fs::write(
            &policy,
            r#"{
              "main_thread": {
                "mismatch_action": "trap",
                "match_action": "allow",
                "filter": [
                  { "syscall": "read" }
                ]
              }
            }"#,
        )
        .expect("write policy");
        let original = fs::read_to_string(&policy).expect("read original policy");
        fs::write(
            &trace,
            "{\"syscall\":\"write\",\"raw\":\"write(1, \\\"x\\\", 1) = 1\"}\n{\"syscall\":\"openat\",\"raw\":\"openat(AT_FDCWD, \\\"/x\\\", O_RDONLY) = 3\"}\n{\"syscall\":\"access\",\"raw\":\"access(\\\"/x\\\", F_OK) = 0\"}\n{\"syscall\":\"accept\",\"raw\":\"accept(3, NULL, NULL) = 4\"}\n{\"syscall\":\"read\",\"raw\":\"read(0, \\\"\\\", 1) = 0\"}\n",
        )
        .expect("write trace");

        extend_policy(&policy, &trace, &output).expect("extend policy");

        assert_eq!(
            fs::read_to_string(&policy).expect("baseline still readable"),
            original
        );
        let updated = fs::read_to_string(&output).expect("read updated policy");
        assert!(
            updated.find("\"mismatch_action\"").unwrap()
                < updated.find("\"match_action\"").unwrap()
        );
        assert!(updated.find("\"match_action\"").unwrap() < updated.find("\"filter\"").unwrap());
        assert!(updated.find("\"accept\"").unwrap() < updated.find("\"access\"").unwrap());
        assert!(updated.find("\"access\"").unwrap() < updated.find("\"openat\"").unwrap());
        assert!(updated.find("\"openat\"").unwrap() < updated.find("\"read\"").unwrap());
        assert!(updated.find("\"read\"").unwrap() < updated.find("\"write\"").unwrap());
        let file = fs::File::open(output).expect("open updated policy");
        let arch = std::env::consts::ARCH.try_into().expect("supported arch");
        let filters = seccompiler::compile_from_json(file, arch).expect("policy should compile");
        assert!(filters.contains_key("main_thread"));
    }

    #[test]
    fn packaged_default_policy_allows_profiled_passt_startup_syscalls() {
        let policy: serde_json::Value =
            serde_json::from_str(include_str!("../../assets/seccomp/default.json"))
                .expect("default policy should parse");

        let syscalls =
            allowed_syscalls_from_policy_value(&policy).expect("default policy should inspect");

        assert!(syscalls.contains("getsockopt"));
        assert!(syscalls.contains("mkdir"));
    }

    #[test]
    fn extend_command_default_policy_resolves_and_extends_packaged_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("default.json");
        let trace = dir.path().join("missing.jsonl");
        let output = dir.path().join("updated.json");
        fs::write(&policy, include_bytes!("../../assets/seccomp/default.json"))
            .expect("write default policy");
        fs::write(&trace, r#"read(0, "", 1) = 0\n"#).expect("write trace");

        let message = run_seccomp_command_with_default_policy_path(
            SeccompCommand::Extend {
                policy: None,
                default_policy: true,
                trace: trace.clone(),
                output: output.clone(),
            },
            || Some(policy.clone()),
        )
        .expect("default policy extend command should run");

        assert!(message.contains(&policy.display().to_string()));
        assert!(output.is_file());
        validate_seccomp_policy_file(&output, "updated policy").expect("updated policy validates");
    }

    #[test]
    fn extend_command_default_policy_fails_closed_before_extending_invalid_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("invalid.json");
        let trace = dir.path().join("missing.jsonl");
        let output = dir.path().join("updated.json");
        fs::write(&policy, b"not json").expect("write invalid default policy");
        fs::write(&trace, r#"read(0, "", 1) = 0\n"#).expect("write trace");

        let err = run_seccomp_command_with_default_policy_path(
            SeccompCommand::Extend {
                policy: None,
                default_policy: true,
                trace,
                output: output.clone(),
            },
            || Some(policy),
        )
        .expect_err("invalid default policy should fail closed");

        let message = format!("{err:#}");
        assert!(message.contains("failed to load default cang seccomp policy"));
        assert!(message.contains("--policy BASELINE_POLICY_JSON"));
        assert!(!output.exists());
    }

    #[test]
    fn extend_command_rejects_ambiguous_internal_baseline_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trace = dir.path().join("missing.jsonl");
        let output = dir.path().join("updated.json");

        let err = run_seccomp_command_with_default_policy_path(
            SeccompCommand::Extend {
                policy: Some(dir.path().join("baseline.json")),
                default_policy: true,
                trace,
                output,
            },
            || panic!("ambiguous command should not resolve default policy"),
        )
        .expect_err("ambiguous internal extend command should fail");

        assert!(format!("{err:#}").contains("exactly one of --policy or --default-policy"));
    }

    #[test]
    fn extend_policy_refuses_to_overwrite_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = dir.path().join("policy.json");
        let trace = dir.path().join("trace.jsonl");
        fs::write(&policy, "{}").expect("write policy");
        fs::write(&trace, "").expect("write trace");

        let err = extend_policy(&policy, &trace, &policy).expect_err("same output should fail");

        assert!(format!("{err:#}").contains("refusing to overwrite baseline"));
    }
}
