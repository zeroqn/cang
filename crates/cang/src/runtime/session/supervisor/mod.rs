use anyhow::Result;
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

pub(crate) mod command;
pub(crate) mod entry;
pub(crate) mod guest_death;
pub(crate) mod identity;
pub(crate) mod managed_exit_marker;
pub(crate) mod managed_ready;
pub(crate) mod readiness_pipe;
pub(crate) mod render_server;
pub(crate) mod rlimits;
pub(crate) mod sigwinch;
pub(crate) mod vm_child;

use crate::runtime::launch::config::LaunchConfig;
use crate::runtime::session::attach::AttachInputPolicy;
use crate::runtime::session::profile::CangHostProfiler;
use crate::runtime::session::task_control::ActiveTaskSpec;

pub(crate) const LIBKRUN_ENTER_HELPER_ARG: &str = "libkrun-network-enter";
pub(crate) const LIBKRUN_VM_WORKER_ARG: &str = "libkrun-vm-worker-enter";
pub(crate) const RENDER_SERVER_BOOTSTRAP_ARG: &str = "render-server-bootstrap";

pub(crate) fn is_supervisor_internal_arg(arg: &str) -> bool {
    matches!(
        arg,
        LIBKRUN_ENTER_HELPER_ARG | LIBKRUN_VM_WORKER_ARG | RENDER_SERVER_BOOTSTRAP_ARG
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildStatus {
    Exited(i32),
    Signaled,
    Detached,
}

impl ChildStatus {
    pub(crate) fn exited(code: i32) -> Self {
        Self::Exited(code)
    }

    pub(crate) fn signaled() -> Self {
        Self::Signaled
    }

    pub(crate) fn detached() -> Self {
        Self::Detached
    }

    pub(crate) fn exit_code(self) -> ExitCode {
        match self {
            Self::Exited(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            Self::Signaled => ExitCode::from(1),
            Self::Detached => ExitCode::SUCCESS,
        }
    }
}

pub(crate) trait Supervisor {
    fn run(
        &self,
        config: &LaunchConfig,
        task_state_dir: &Path,
        profiler: &mut CangHostProfiler,
        active_task: &ActiveTaskSpec,
        daemon_initial_attach: bool,
        attach_input_policy: AttachInputPolicy,
    ) -> Result<ChildStatus>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostSupervisor;

impl Supervisor for HostSupervisor {
    fn run(
        &self,
        config: &LaunchConfig,
        task_state_dir: &Path,
        profiler: &mut CangHostProfiler,
        active_task: &ActiveTaskSpec,
        daemon_initial_attach: bool,
        attach_input_policy: AttachInputPolicy,
    ) -> Result<ChildStatus> {
        let _waypipe_broker = active_task
            .waypipe
            .as_ref()
            .map(|waypipe| {
                let data_socket = config
                    .waypipe
                    .as_ref()
                    .expect("active Waypipe task must have launch config")
                    .socket
                    .clone();
                crate::runtime::session::waypipe_broker::WaypipeBroker::start(
                    data_socket,
                    waypipe.control_socket.clone(),
                    waypipe.initial_target.clone(),
                )
            })
            .transpose()?;
        let config_path = task_state_dir.join("launch.conf");
        config.write_to(&config_path)?;
        command::run_helper_process(
            config,
            &config_path,
            profiler,
            active_task,
            daemon_initial_attach,
            attach_input_policy,
        )
    }
}

pub(crate) fn run_internal(args: Vec<OsString>) -> Result<()> {
    entry::run_internal(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::launch::config::NetworkMode;
    use crate::runtime::session::supervisor::identity::KeepIdLauncher;
    use std::path::PathBuf;

    #[test]
    fn child_status_maps_to_parent_exit_code() {
        assert_eq!(ChildStatus::exited(0).exit_code(), ExitCode::from(0));
        assert_eq!(ChildStatus::exited(42).exit_code(), ExitCode::from(42));
        assert_eq!(ChildStatus::exited(300).exit_code(), ExitCode::from(1));
        assert_eq!(ChildStatus::signaled().exit_code(), ExitCode::from(1));
    }

    #[test]
    fn internal_rejects_wrong_argument_shape() {
        let err =
            run_internal(vec!["libkrun-network-enter".into()]).expect_err("missing config path");
        assert!(format!("{err:#}").contains("expected internal"));

        let err = run_internal(vec!["btrfs-rootfs".into(), "/tmp/x".into()])
            .expect_err("wrong subcommand");
        assert!(format!("{err:#}").contains("unknown cang internal command"));
    }

    #[test]
    fn supervisor_internal_arg_recognizes_helper_vm_worker_and_render_server_bootstrap() {
        assert!(is_supervisor_internal_arg(LIBKRUN_ENTER_HELPER_ARG));
        assert!(is_supervisor_internal_arg(LIBKRUN_VM_WORKER_ARG));
        assert!(is_supervisor_internal_arg(RENDER_SERVER_BOOTSTRAP_ARG));
        assert!(!is_supervisor_internal_arg("btrfs-rootfs"));
    }

    #[test]
    fn helper_filesystem_identity_parser_uses_latest_required_env_values() {
        let mut config = minimal_launch_config();
        config.guest_config_env = vec![
            ("CANG_HOST_UID".to_owned(), "image".to_owned()),
            ("CANG_HOST_GID".to_owned(), "image".to_owned()),
            ("CANG_HOST_UID".to_owned(), "1000".to_owned()),
            ("CANG_HOST_GID".to_owned(), "993".to_owned()),
        ];

        assert_eq!(
            identity::required_guest_config_u32(&config, "CANG_HOST_UID").unwrap(),
            1000
        );
        assert_eq!(
            identity::required_guest_config_u32(&config, "CANG_HOST_GID").unwrap(),
            993
        );
    }

    #[test]
    fn helper_filesystem_identity_parser_rejects_missing_or_invalid_values() {
        let mut config = minimal_launch_config();
        assert!(
            format!(
                "{:#}",
                identity::required_guest_config_u32(&config, "CANG_HOST_UID").unwrap_err()
            )
            .contains("missing")
        );
        config.guest_config_env = vec![("CANG_HOST_UID".to_owned(), "not-a-uid".to_owned())];
        assert!(
            format!(
                "{:#}",
                identity::required_guest_config_u32(&config, "CANG_HOST_UID").unwrap_err()
            )
            .contains("not a u32")
        );
    }

    fn minimal_launch_config() -> LaunchConfig {
        LaunchConfig {
            task_rootfs: PathBuf::from("/tmp/rootfs"),
            hostname: "cang-test".to_owned(),
            mounts: Vec::new(),
            host_nix_overlay: None,
            guest_init_override: None,
            disks: Vec::new(),
            ram_mib: 1024,
            vcpus: 1,
            log_level: crate::logging::LogLevel::Info,
            network_mode: NetworkMode::Tsi,
            gpu_mode: crate::runtime::vm::gpu::GpuMode::Off,
            zero_copy_shm: false,
            new_perms: crate::runtime::launch::config::GuestPermissions::default(),
            publish: Vec::new(),
            workdir: "/workspace".to_owned(),
            exec_path: "/bin/sh".to_owned(),
            argv: Vec::new(),
            env: Vec::new(),
            guest_config_env: Vec::new(),
            passt_fd: None,
            pulse_bridge: None,
            waypipe: None,
            exec: None,
            managed_session: None,
            seccomp: Default::default(),
            landlock: Default::default(),
        }
    }

    #[test]
    fn libkrun_helper_launches_through_keep_id_unshare() {
        let launcher = KeepIdLauncher::from_parts(
            1000,
            993,
            "dev",
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
        )
        .unwrap();
        let spec = command::build_helper_command_with_launcher(
            Path::new("/nix/store/hash-cang/bin/cang"),
            Path::new("/tmp/cang-task/launch.conf"),
            crate::logging::LogLevel::Debug,
            false,
            None,
            None,
            &launcher,
        );

        assert_eq!(spec.program, OsString::from("unshare"));
        assert_eq!(
            spec.env,
            vec![(
                OsString::from("CANG_INTERNAL_LOG_LEVEL"),
                OsString::from("debug")
            )]
        );
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.contains(&"--user".to_owned()));
        assert!(args.contains(&"--mount".to_owned()));
        assert!(args.contains(&"--keep-caps".to_owned()));
        assert!(args.windows(2).any(|pair| pair == ["--setuid", "0"]));
        assert!(args.windows(2).any(|pair| pair == ["--setgid", "0"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--map-users", "1000:1000:1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--map-groups", "993:993:1"])
        );
        assert_eq!(
            &args[args.len() - 4..],
            [
                "/nix/store/hash-cang/bin/cang",
                "internal",
                "libkrun-network-enter",
                "/tmp/cang-task/launch.conf",
            ]
        );
    }

    #[test]
    fn libkrun_helper_sets_host_profile_env_only_when_enabled() {
        let launcher = KeepIdLauncher::from_parts(
            1000,
            993,
            "dev",
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
        )
        .unwrap();

        let disabled = command::build_helper_command_with_launcher(
            Path::new("/nix/store/hash-cang/bin/cang"),
            Path::new("/tmp/cang-task/launch.conf"),
            crate::logging::LogLevel::Debug,
            false,
            None,
            None,
            &launcher,
        );
        assert!(
            !disabled
                .env
                .iter()
                .any(|(key, _)| key == &OsString::from("CANG_HOST_PROFILE"))
        );
        assert!(
            !disabled
                .env
                .iter()
                .any(|(key, _)| key == &OsString::from("CANG_GUEST_PROFILE"))
        );

        let enabled = command::build_helper_command_with_launcher(
            Path::new("/nix/store/hash-cang/bin/cang"),
            Path::new("/tmp/cang-task/launch.conf"),
            crate::logging::LogLevel::Debug,
            true,
            None,
            None,
            &launcher,
        );
        assert!(enabled.env.contains(&(
            OsString::from("CANG_INTERNAL_LOG_LEVEL"),
            OsString::from("debug")
        )));
        assert!(
            enabled
                .env
                .contains(&(OsString::from("CANG_HOST_PROFILE"), OsString::from("1")))
        );
        assert!(
            !enabled
                .env
                .iter()
                .any(|(key, _)| key == &OsString::from("CANG_GUEST_PROFILE"))
        );
    }

    #[test]
    fn managed_libkrun_helper_includes_readiness_fd_env_when_requested() {
        let launcher = KeepIdLauncher::from_parts(
            1000,
            993,
            "dev",
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
            crate::runtime::session::supervisor::identity::SubIdRange::new(100_000, 65_536)
                .unwrap(),
        )
        .unwrap();

        let spec = command::build_helper_command_with_launcher(
            Path::new("/nix/store/hash-cang/bin/cang"),
            Path::new("/tmp/cang-task/launch.conf"),
            crate::logging::LogLevel::Info,
            false,
            Some(42),
            None,
            &launcher,
        );

        assert!(spec.env.contains(&(
            OsString::from(crate::runtime::session::supervisor::readiness_pipe::READY_FD_ENV),
            OsString::from("42")
        )));
    }
}
