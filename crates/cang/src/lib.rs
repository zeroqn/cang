use anyhow::Result;
use clap::Parser;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Instant;

mod cli;
mod config;
mod logging;
mod naming;
mod runtime;
mod state;
mod task_rootfs;
mod va_driver;

use cli::{Cli, CliAction, RuntimeOptions};

const DEFAULT_IMAGE: &str = "localhost/cang:latest";
const DEFAULT_FALLBACK_IMAGE: &str = "ghcr.io/zeroqn/cang:latest";

pub fn entrypoint() -> ExitCode {
    // Link cang's `dlopen` interposer in (see `va_driver`).
    va_driver::install();
    let host_session_started_at = Instant::now();
    let args = std::env::args_os().collect::<Vec<_>>();
    if is_internal_invocation(&args) {
        return match run_internal(args.into_iter().skip(2).collect()) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("cang internal: {err:#}");
                ExitCode::from(1)
            }
        };
    }

    match Cli::parse().into_action() {
        CliAction::Run(options) => run_with_logging(
            options,
            runtime::RuntimeProfileScope::from_started_at(host_session_started_at),
        ),
        CliAction::ContainerStore { command, options } => {
            if let Err(err) = logging::init_tracing(&options.log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_container_store_command(command, options) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::DecodeLaunchConf { path } => {
            match runtime::launch::config::LaunchConfig::decode_file_for_debug(&path) {
                Ok(decoded) => {
                    print!("{decoded}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Images {
            command,
            log_settings,
        } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_image_command(image_cache_command_from_cli(command)) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Exec {
            task_id,
            command,
            waypipe,
            log_settings,
        } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::session::run_exec_command(task_id, command, waypipe) {
                Ok(code) => code,
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Ps { log_settings } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_task_control_command(
                runtime::session::task_control::TaskControlCommand::Ps,
            ) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Attach {
            task_id,
            log_settings,
        } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_task_control_command(
                runtime::session::task_control::TaskControlCommand::Attach { task_id },
            ) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Kill {
            task_id,
            log_settings,
        } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_task_control_command(
                runtime::session::task_control::TaskControlCommand::Kill { task_id },
            ) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
        CliAction::Seccomp {
            command,
            log_settings,
        } => {
            if let Err(err) = logging::init_tracing(&log_settings) {
                eprintln!("cang: {err:#}");
                return ExitCode::from(1);
            }
            match runtime::run_seccomp_command(command) {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("cang: {err:#}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn image_cache_command_from_cli(
    command: cli::ImagesCommand,
) -> runtime::session::rootfs::image_source::ImageCacheCommand {
    match command {
        cli::ImagesCommand::Sync { reference } => {
            runtime::session::rootfs::image_source::ImageCacheCommand::Sync { reference }
        }
        cli::ImagesCommand::List => runtime::session::rootfs::image_source::ImageCacheCommand::List,
        cli::ImagesCommand::Remove { target, dry_run } => {
            runtime::session::rootfs::image_source::ImageCacheCommand::Remove { target, dry_run }
        }
    }
}

fn run_with_logging(
    options: RuntimeOptions,
    profile_scope: runtime::RuntimeProfileScope,
) -> ExitCode {
    if let Err(err) = logging::init_tracing(&options.log_settings) {
        eprintln!("cang: {err:#}");
        return ExitCode::from(1);
    }

    match run(options, profile_scope) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("cang: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run(options: RuntimeOptions, profile_scope: runtime::RuntimeProfileScope) -> Result<ExitCode> {
    runtime::run(options, profile_scope)
}

fn is_internal_invocation(args: &[OsString]) -> bool {
    args.get(1).and_then(|arg| arg.to_str()) == Some("internal")
}

fn run_internal(args: Vec<OsString>) -> Result<ExitCode> {
    runtime::run_internal(args)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::cli::Cli;

    #[test]
    fn command_help_renders() {
        Cli::command().debug_assert();
    }

    #[test]
    fn top_level_cli_does_not_accept_microvm_subcommand() {
        let err = Cli::try_parse_from(["cang", "microvm"]).expect_err("microvm is not a command");

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}
