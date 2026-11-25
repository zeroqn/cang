use super::{UTF8_LOCALE_DEFAULT, append_utf8_locale_defaults, derive};
use crate::guest_init::components::home::identity::DevIdentity;
use std::ffi::OsString;
use std::path::PathBuf;

#[test]
fn internal_runtime_derives_parent_shell_environment_before_podman_prep() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, true, None);
    assert!(
        shell_env
            .vars
            .contains(&("HOME".to_owned(), "/home/dev".to_owned()))
    );
    assert!(
        shell_env
            .vars
            .contains(&("USER".to_owned(), "dev".to_owned()))
    );
    assert!(
        shell_env
            .vars
            .contains(&("LOGNAME".to_owned(), "dev".to_owned()))
    );
    assert!(
        shell_env
            .vars
            .contains(&("XDG_RUNTIME_DIR".to_owned(), "/run/user/1234".to_owned()))
    );
    assert!(
        shell_env
            .vars
            .iter()
            .any(|(key, value)| key == "PATH" && value.starts_with("/run/cang/wrappers/bin:"))
    );
    assert!(shell_env.vars.contains(&(
        "DOCKER_HOST".to_owned(),
        "unix:///run/user/1234/podman/podman.sock".to_owned()
    )));
    assert_eq!(shell_env.tmpdir, PathBuf::from("/home/dev/.cache/tmp"));
}

#[test]
fn internal_shell_environment_omits_docker_host_without_container_storage() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, false, None);

    assert!(!shell_env.vars.iter().any(|(key, _)| key == "DOCKER_HOST"));
    assert!(
        !shell_env
            .vars
            .iter()
            .any(|(key, _)| key == "XDG_RUNTIME_DIR")
    );
}

#[test]
fn internal_shell_environment_exports_fontconfig_path() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, false, None);

    assert!(shell_env.vars.contains(&(
        "FONTCONFIG_PATH".to_owned(),
        "/usr/lib/cang-fontconfig/etc/fonts".to_owned()
    )));
}

#[test]
fn internal_shell_environment_exports_pulse_server_when_configured() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, false, Some("tcp:192.0.2.10:4713"));

    assert!(
        shell_env
            .vars
            .contains(&("PULSE_SERVER".to_owned(), "tcp:192.0.2.10:4713".to_owned()))
    );
}

#[test]
fn internal_shell_environment_omits_pulse_server_without_configuration() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, false, None);

    assert!(!shell_env.vars.iter().any(|(key, _)| key == "PULSE_SERVER"));
}

#[test]
fn internal_shell_environment_defaults_missing_utf8_locale_vars() {
    let mut vars = Vec::new();

    append_utf8_locale_defaults(&mut vars, |_| None);

    assert!(vars.contains(&("LANG".to_owned(), UTF8_LOCALE_DEFAULT.to_owned())));
    assert!(vars.contains(&("LC_CTYPE".to_owned(), UTF8_LOCALE_DEFAULT.to_owned())));
    assert!(!vars.iter().any(|(key, _)| key == "LC_ALL"));
}

#[test]
fn internal_shell_environment_defaults_empty_utf8_locale_vars() {
    let mut vars = Vec::new();

    append_utf8_locale_defaults(&mut vars, |key| match key {
        "LANG" | "LC_CTYPE" => Some(OsString::new()),
        _ => None,
    });

    assert!(vars.contains(&("LANG".to_owned(), UTF8_LOCALE_DEFAULT.to_owned())));
    assert!(vars.contains(&("LC_CTYPE".to_owned(), UTF8_LOCALE_DEFAULT.to_owned())));
    assert!(!vars.iter().any(|(key, _)| key == "LC_ALL"));
}

#[test]
fn internal_shell_environment_preserves_explicit_locale_vars() {
    let mut vars = Vec::new();

    append_utf8_locale_defaults(&mut vars, |key| match key {
        "LANG" => Some(OsString::from("ja_JP.UTF-8")),
        "LC_CTYPE" => Some(OsString::from("en_US.UTF-8")),
        "LC_ALL" => Some(OsString::from("C")),
        _ => None,
    });

    assert!(vars.is_empty());
}

/// The build job count is derived from the same reserve the affinity mask
/// applies, so a shell does not spawn more compilers than the cores it was left.
#[test]
fn the_shell_environment_carries_the_bounded_build_job_count() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/nix/store/fish/bin/fish"));
    let shell_env = derive(&identity, false, None);

    let cpus = crate::guest_init::runtime::cpu_bound::guest_cpu_count();
    let expected = crate::guest_init::runtime::cpu_bound::workload_job_count(cpus);
    let jobs = shell_env
        .vars
        .iter()
        .find(|(key, _)| key == "CARGO_BUILD_JOBS");

    match expected {
        // A one-core guest reserves nothing, so forcing a value would only
        // overwrite what its own `nproc` already gives.
        None => assert_eq!(jobs, None, "a one-core guest exports no job count"),
        Some(jobs_expected) => assert_eq!(
            jobs,
            Some(&("CARGO_BUILD_JOBS".to_owned(), jobs_expected.to_string())),
            "the export is the count of the cores the workload keeps"
        ),
    }
}

/// A user's own choice wins: the guest derives the default only when it has one.
#[test]
fn an_explicit_build_job_count_is_not_overwritten() {
    use std::ffi::OsString;

    assert_eq!(
        super::bounded_job_count_default(Some(OsString::from("4")), 26),
        None
    );
    assert_eq!(
        super::bounded_job_count_default(None, 26),
        Some("24".to_owned())
    );
    assert_eq!(
        super::bounded_job_count_default(None, 2),
        Some("1".to_owned())
    );
    // One vCPU reserves nothing, so there is no default to force.
    assert_eq!(super::bounded_job_count_default(None, 1), None);
}
