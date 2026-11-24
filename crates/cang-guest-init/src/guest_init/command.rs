use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub(in crate::guest_init) fn require_on_path(name: &str) -> Result<PathBuf> {
    find_on_path(name).ok_or_else(|| anyhow!("required tool '{name}' is not available on PATH"))
}

pub(in crate::guest_init) fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

pub(in crate::guest_init) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(in crate::guest_init) fn status_ok(program: &str, args: &[&str]) -> Result<bool> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    Ok(status.success())
}

pub(in crate::guest_init) fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{program} exited with status {status}"))
    }
}

pub(in crate::guest_init) fn output_trimmed(
    program: &str,
    args: &[&str],
) -> Result<Option<String>> {
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!text.is_empty()).then_some(text))
}

pub(in crate::guest_init) fn spawn_background_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Child> {
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))
}
