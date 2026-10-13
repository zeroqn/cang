use super::{
    MAX_ROOTLESS_INFO_STDERR, PodmanServiceEnv, PodmanServiceLock, command_plan_with_env,
    drain_nonblocking, terminate_and_reap, verify_rootless_info_with_env_and_timeout,
};
use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::components::podman::service::{
    PodmanServicePaths, docker_host_uri, socket_is_live, wait_for_socket,
};
use std::ffi::OsString;
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn verification_fixture(contents: &str) -> (tempfile::TempDir, PodmanServiceEnv) {
    let temp = tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let podman = executable_fixture(temp.path().join("podman"), contents);
    let env = PodmanServiceEnv {
        real_podman: Some(podman.display().to_string()),
        search_path: None,
        service_path: "/usr/bin:/bin".to_owned(),
        ssl_cert_file: None,
        nix_ssl_cert_file: None,
    };
    (temp, env)
}

fn verification_identity() -> DevIdentity {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    DevIdentity::new(uid, gid, PathBuf::from("/bin/sh"))
}

#[test]
fn rootless_info_verification_accepts_success() {
    let (_temp, env) = verification_fixture("#!/bin/sh\nexit 0\n");

    verify_rootless_info_with_env_and_timeout(
        &verification_identity(),
        &env,
        Duration::from_secs(1),
    )
    .unwrap();
}

#[test]
fn rootless_info_verification_preserves_failure_stderr() {
    let (_temp, env) = verification_fixture("#!/bin/sh\necho 'idmap setup failed' >&2\nexit 1\n");

    let err = verify_rootless_info_with_env_and_timeout(
        &verification_identity(),
        &env,
        Duration::from_secs(1),
    )
    .unwrap_err();

    assert!(err.to_string().contains("idmap setup failed"));
}

#[test]
fn rootless_info_verification_uses_dev_identity() {
    let identity = verification_identity();
    let script = format!(
        "#!/bin/sh\nuid=\ngid=\nwhile read -r key real effective saved fs; do\n    case \"$key\" in\n        Uid:) uid=$effective ;;\n        Gid:) gid=$effective ;;\n    esac\ndone < /proc/self/status\n[ \"$uid\" = \"{}\" ] && [ \"$gid\" = \"{}\" ]\n",
        identity.uid, identity.gid
    );
    let (_temp, env) = verification_fixture(&script);

    verify_rootless_info_with_env_and_timeout(&identity, &env, Duration::from_secs(1)).unwrap();
}

#[test]
fn rootless_info_verification_transitions_from_root_to_dev_identity() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let identity = DevIdentity::new(65534, 65534, PathBuf::from("/bin/sh"));
    let (_temp, env) = verification_fixture(
        "#!/bin/sh\nuid=\ngid=\nwhile read -r key real effective saved fs; do\n    case \"$key\" in\n        Uid:) uid=$effective ;;\n        Gid:) gid=$effective ;;\n    esac\ndone < /proc/self/status\n[ \"$uid\" = \"65534\" ] && [ \"$gid\" = \"65534\" ]\n",
    );

    verify_rootless_info_with_env_and_timeout(&identity, &env, Duration::from_secs(1)).unwrap();
}

#[test]
fn rootless_info_verification_times_out_and_reaps_child() {
    let (_temp, env) = verification_fixture("#!/bin/sh\nwhile :; do :; done\n");
    let started = Instant::now();

    let err = verify_rootless_info_with_env_and_timeout(
        &verification_identity(),
        &env,
        Duration::from_millis(20),
    )
    .unwrap_err();

    assert!(err.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn rootless_info_verification_does_not_wait_for_inherited_stderr() {
    let (_temp, env) = verification_fixture("#!/bin/sh\nsleep 2 &\necho failed >&2\nexit 1\n");
    let started = Instant::now();

    let err = verify_rootless_info_with_env_and_timeout(
        &verification_identity(),
        &env,
        Duration::from_secs(1),
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed"));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn rootless_info_verification_handles_timeout_exit_races() {
    let (_temp, env) = verification_fixture("#!/bin/sh\nexit 0\n");

    for _ in 0..100 {
        let result = verify_rootless_info_with_env_and_timeout(
            &verification_identity(),
            &env,
            Duration::ZERO,
        );
        if let Err(err) = result {
            assert!(err.to_string().contains("timed out"), "{err:#}");
        }
    }
}
#[test]
fn rootless_info_stderr_capture_is_bounded() {
    let input = vec![b'x'; MAX_ROOTLESS_INFO_STDERR * 2];
    let mut reader = Cursor::new(input);
    let mut output = Vec::new();

    drain_nonblocking(&mut reader, &mut output).unwrap();

    assert_eq!(output.len(), MAX_ROOTLESS_INFO_STDERR);
}

#[test]
fn child_cleanup_handles_an_already_exited_child() {
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    while child.try_wait().unwrap().is_none() {
        std::thread::yield_now();
    }

    terminate_and_reap(&mut child).unwrap();

    assert!(child.try_wait().unwrap().is_some());
}

#[test]
fn podman_service_paths_are_uid_derived() {
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/bin/fish"));
    let paths = PodmanServicePaths::for_identity(&identity);

    assert_eq!(paths.runtime_dir, PathBuf::from("/run/user/1234"));
    assert_eq!(
        paths.socket_path,
        PathBuf::from("/run/user/1234/podman/podman.sock")
    );
    assert_eq!(paths.socket_uri, "unix:///run/user/1234/podman/podman.sock");
    assert_eq!(docker_host_uri(&identity), paths.socket_uri);
}

#[test]
fn podman_service_command_uses_real_binary_and_rootless_env() {
    let temp = tempdir().unwrap();
    let podman = executable_fixture(temp.path().join("real-podman"), "#!/bin/sh\nexit 0\n");
    let injected_env = PodmanServiceEnv {
        real_podman: Some(podman.display().to_string()),
        search_path: Some(OsString::from(
            "/nix/store/wrappers/bin:/nix/store/podman/bin",
        )),
        service_path: "/nix/store/wrappers/bin:/nix/store/podman/bin".to_owned(),
        ssl_cert_file: None,
        nix_ssl_cert_file: None,
    };

    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/bin/fish"));
    let paths = PodmanServicePaths::for_identity(&identity);
    let plan = command_plan_with_env(&identity, &paths, &injected_env).unwrap();

    assert_eq!(plan.program, podman);
    assert_eq!(
        plan.args,
        vec![
            "system",
            "service",
            "--time=0",
            "unix:///run/user/1234/podman/podman.sock"
        ]
    );
    assert_env(&plan.env, "USER", "dev");
    assert_env(&plan.env, "HOME", "/home/dev");
    assert_env(&plan.env, "SHELL", "/bin/fish");
    assert_env(&plan.env, "XDG_RUNTIME_DIR", "/run/user/1234");
    assert_env(&plan.env, "XDG_CONFIG_HOME", "/home/dev/.config");
    assert_env(&plan.env, "XDG_DATA_HOME", "/home/dev/.local/share");
    assert_env(&plan.env, "XDG_STATE_HOME", "/home/dev/.local/state");
    assert_env(&plan.env, "XDG_CACHE_HOME", "/home/dev/.cache");
    assert_env(&plan.env, "TMPDIR", "/home/dev/.cache/tmp");
    assert!(
        plan.env
            .iter()
            .any(|(key, value)| key == "PATH" && value.starts_with("/run/cang/wrappers/bin:"))
    );
}

#[test]
fn podman_service_command_discovers_real_binary_from_injected_path() {
    let temp = tempdir().unwrap();
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let podman = executable_fixture(bin_dir.join("podman"), "#!/bin/sh\nexit 0\n");
    let injected_env = PodmanServiceEnv {
        real_podman: None,
        search_path: Some(OsString::from(bin_dir.as_os_str())),
        service_path: "/usr/bin".to_owned(),
        ssl_cert_file: None,
        nix_ssl_cert_file: None,
    };

    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/bin/fish"));
    let paths = PodmanServicePaths::for_identity(&identity);
    let plan = command_plan_with_env(&identity, &paths, &injected_env).unwrap();

    assert_eq!(plan.program, podman);
    assert_env(&plan.env, "PATH", "/run/cang/wrappers/bin:/usr/bin");
}

#[test]
fn podman_service_refuses_cang_compat_wrapper() {
    for (name, body) in [
        (
            "prep-wait",
            "#!/bin/sh\ncang-guest-init internal podman wait\nexec /real/podman \"$@\"\n",
        ),
        (
            "service-wait",
            "#!/bin/sh\ncang-guest-init internal podman service-wait\nexec /real/podman \"$@\"\n",
        ),
    ] {
        let temp = tempdir().unwrap();
        let wrapper = executable_fixture(temp.path().join("podman"), body);
        let injected_env = PodmanServiceEnv {
            real_podman: Some(wrapper.display().to_string()),
            search_path: None,
            service_path: String::new(),
            ssl_cert_file: None,
            nix_ssl_cert_file: None,
        };

        let identity = DevIdentity::new(1234, 1235, PathBuf::from("/bin/fish"));
        let paths = PodmanServicePaths::for_identity(&identity);
        let err = command_plan_with_env(&identity, &paths, &injected_env).unwrap_err();

        assert!(
            err.to_string().contains("compatibility wrapper"),
            "{name} wrapper should be rejected"
        );
    }
}

#[test]
fn podman_socket_probe_detects_live_unix_listener() {
    let temp = tempdir().unwrap();
    let socket = temp.path().join("podman.sock");
    let _listener = UnixListener::bind(&socket).unwrap();

    assert!(socket_is_live(&socket));
    wait_for_socket(&socket, Duration::from_millis(10)).unwrap();
}

#[test]
fn podman_socket_wait_times_out_for_missing_socket() {
    let temp = tempdir().unwrap();
    let socket = temp.path().join("missing.sock");
    let err = wait_for_socket(&socket, Duration::from_millis(0)).unwrap_err();

    assert!(
        err.to_string()
            .contains("timed out waiting for Podman API socket")
    );
    assert!(err.to_string().contains("missing.sock"));
}

#[test]
fn podman_service_lock_times_out_instead_of_blocking_forever() {
    let temp = tempdir().unwrap();
    let identity = DevIdentity::new(1234, 1235, PathBuf::from("/bin/fish"));
    let paths = PodmanServicePaths::from_runtime_dir(temp.path().join("runtime"));
    fs::create_dir_all(&paths.socket_dir).unwrap();

    let _held = PodmanServiceLock::acquire_until(
        &identity,
        &paths,
        std::time::Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let err = match PodmanServiceLock::acquire_until(
        &identity,
        &paths,
        std::time::Instant::now() + Duration::from_millis(0),
    ) {
        Ok(_) => panic!("second lock acquisition should time out"),
        Err(err) => err,
    };

    assert!(
        err.to_string()
            .contains("timed out waiting for Podman API service lock")
    );
}

fn executable_fixture(path: PathBuf, contents: &str) -> PathBuf {
    crate::guest_init::components::test_support::write_executable_stub(&path, contents);
    path
}

fn assert_env(env: &[(String, String)], key: &str, value: &str) {
    assert!(
        env.iter().any(
            |(candidate_key, candidate_value)| candidate_key == key && candidate_value == value
        ),
        "missing {key}={value} in {env:?}"
    );
}
