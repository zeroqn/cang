use std::env;
use std::path::PathBuf;

use crate::guest_init::components::env::DEV_USER;
use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::components::rootless::idmap::WRAPPER_BIN_DIR;

const UTF8_LOCALE_DEFAULT: &str = "C.UTF-8";
const UTF8_LOCALE_ENV_NAMES: [&str; 2] = ["LANG", "LC_CTYPE"];
const FONTCONFIG_PATH: &str = "/usr/lib/cang-fontconfig/etc/fonts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) struct ShellEnvironment {
    pub(in crate::guest_init) vars: Vec<(String, String)>,
    pub(in crate::guest_init) tmpdir: PathBuf,
    pub(in crate::guest_init) runtime_dir: Option<PathBuf>,
}

pub(in crate::guest_init) fn derive(
    identity: &DevIdentity,
    containers_storage: bool,
    pulse_server: Option<&str>,
) -> ShellEnvironment {
    let home = identity.home.display().to_string();
    let tmpdir = identity.home.join(".cache/tmp");
    let runtime_dir =
        containers_storage.then(|| PathBuf::from(format!("/run/user/{}", identity.uid)));
    let mut vars = vec![
        ("USER".to_owned(), DEV_USER.to_owned()),
        ("LOGNAME".to_owned(), DEV_USER.to_owned()),
        ("HOME".to_owned(), home.clone()),
        ("SHELL".to_owned(), identity.shell.display().to_string()),
        ("XDG_CONFIG_HOME".to_owned(), format!("{home}/.config")),
        ("XDG_DATA_HOME".to_owned(), format!("{home}/.local/share")),
        ("XDG_STATE_HOME".to_owned(), format!("{home}/.local/state")),
        ("XDG_CACHE_HOME".to_owned(), format!("{home}/.cache")),
        ("TMPDIR".to_owned(), tmpdir.display().to_string()),
        ("FONTCONFIG_PATH".to_owned(), FONTCONFIG_PATH.to_owned()),
    ];
    if let Some(runtime_dir) = &runtime_dir {
        vars.push((
            "XDG_RUNTIME_DIR".to_owned(),
            runtime_dir.display().to_string(),
        ));
        vars.push((
            "DOCKER_HOST".to_owned(),
            crate::guest_init::components::podman::service::docker_host_uri(identity),
        ));
    }
    if containers_storage {
        let path = env::var("PATH").unwrap_or_default();
        vars.push(("PATH".to_owned(), format!("{WRAPPER_BIN_DIR}:{path}")));
    }
    if let Some(pulse_server) = pulse_server {
        vars.push(("PULSE_SERVER".to_owned(), pulse_server.to_owned()));
    }
    if let Some(jobs) = crate::guest_init::components::responsive::enabled()
        .then(|| {
            bounded_job_count_default(
                env::var_os("CARGO_BUILD_JOBS"),
                crate::guest_init::runtime::cpu_bound::guest_cpu_count(),
            )
        })
        .flatten()
    {
        vars.push(("CARGO_BUILD_JOBS".to_owned(), jobs));
    }
    append_utf8_locale_defaults(&mut vars, |key| env::var_os(key));
    ShellEnvironment {
        vars,
        tmpdir,
        runtime_dir,
    }
}

/// The build job count the guest gives a shell: one compiler per core the
/// workload was left, so a build spawns no more compilers than the reserving mask
/// allows - which is also what keeps its peak anonymous memory down.
///
/// `None` when the user already set it (the shell's own configuration is sourced
/// after this and wins) or when the guest is too small to reserve a core, where
/// the guest's own `nproc` is already the right answer.
fn bounded_job_count_default(existing: Option<std::ffi::OsString>, cpus: usize) -> Option<String> {
    if existing.is_some() {
        return None;
    }
    crate::guest_init::runtime::cpu_bound::workload_job_count(cpus).map(|jobs| jobs.to_string())
}

fn append_utf8_locale_defaults<F>(vars: &mut Vec<(String, String)>, lookup: F)
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    for key in UTF8_LOCALE_ENV_NAMES {
        let should_default = match lookup(key) {
            Some(value) => value.is_empty(),
            None => true,
        };
        if should_default {
            vars.push((key.to_owned(), UTF8_LOCALE_DEFAULT.to_owned()));
        }
    }
}

pub(in crate::guest_init) fn export(env_contract: &ShellEnvironment) {
    for (key, value) in &env_contract.vars {
        // SAFETY: guest-init builds and exports the shell environment during
        // single-threaded bootstrap immediately before replacing the process.
        unsafe { env::set_var(key, value) };
    }
}

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
