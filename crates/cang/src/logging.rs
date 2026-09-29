use anyhow::{Context, Result};
use clap::ValueEnum;
use tracing_subscriber::EnvFilter;

pub(crate) const INTERNAL_LOG_LEVEL_ENV: &str = "CANG_INTERNAL_LOG_LEVEL";
const RUST_LOG_ENV: &str = "RUST_LOG";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    pub(crate) fn libkrun_level(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }

    /// The numeric level handed to libkrun's own logger (`krun_init_log`),
    /// floored at `warn`.
    ///
    /// libkrun's diagnostics go through `log` and env_logger, not through
    /// cang's tracing subscriber, and only at the level cang passes here. cang's
    /// default is `off`, which would swallow the fork's warnings: `--zero-copy-shm`
    /// falling back to the copy path because `/dev/udmabuf` is unusable, a GPU
    /// backend that failed to come up, a guest handle about to be mis-routed.
    /// Those describe a degraded or failing launch, so they are never silenced -
    /// cang's own tracing still honours `--log-level` exactly.
    pub(crate) fn libkrun_log_level(self) -> u32 {
        self.libkrun_level().max(Self::Warn.libkrun_level())
    }

    pub(crate) fn enables_debug(self) -> bool {
        self >= Self::Debug
    }

    pub(crate) fn parse_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogSettings {
    pub(crate) level: LogLevel,
    filter: Option<String>,
}

impl LogSettings {
    pub(crate) fn from_process_env(cli_level: Option<LogLevel>, debug_flag: bool) -> Self {
        Self::resolve(
            cli_level,
            debug_flag,
            std::env::var(RUST_LOG_ENV).ok().as_deref(),
        )
    }

    pub(crate) fn for_internal_helper(level: LogLevel) -> Self {
        Self {
            level,
            filter: Some(level_filter(level)),
        }
    }

    pub(crate) fn resolve(
        cli_or_env_level: Option<LogLevel>,
        debug_flag: bool,
        rust_log: Option<&str>,
    ) -> Self {
        if let Some(level) = cli_or_env_level {
            return Self {
                level,
                filter: Some(level_filter(level)),
            };
        }
        if debug_flag {
            return Self {
                level: LogLevel::Debug,
                filter: Some(level_filter(LogLevel::Debug)),
            };
        }

        let rust_log = rust_log.map(str::trim).filter(|value| !value.is_empty());
        let level = rust_log
            .and_then(parse_scalar_rust_log)
            .unwrap_or(LogLevel::Off);
        Self {
            level,
            filter: rust_log.map(ToOwned::to_owned),
        }
    }

    fn env_filter(&self) -> Result<EnvFilter> {
        match &self.filter {
            Some(filter) => EnvFilter::try_new(filter)
                .with_context(|| format!("failed to parse cang log filter '{filter}'")),
            None => Ok(EnvFilter::default()),
        }
    }
}

pub(crate) fn init_tracing(settings: &LogSettings) -> Result<()> {
    let filter = settings.env_filter()?;
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(())
}

pub(crate) fn helper_pre_config_debug_enabled() -> bool {
    std::env::var(INTERNAL_LOG_LEVEL_ENV)
        .ok()
        .and_then(|value| LogLevel::parse_name(&value))
        .is_some_and(LogLevel::enables_debug)
}

fn level_filter(level: LogLevel) -> String {
    match level {
        LogLevel::Off => "off".to_owned(),
        other => other.as_str().to_owned(),
    }
}

fn parse_scalar_rust_log(value: &str) -> Option<LogLevel> {
    let trimmed = value.trim();
    if trimmed.contains(',') || trimmed.contains('=') || trimmed.contains('/') {
        return None;
    }
    LogLevel::parse_name(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libkrun_log_level_is_floored_at_warn() {
        // Off/Error/Warn all reach libkrun's logger as warn, so its
        // "running without it" diagnostics cannot be silenced; info and above
        // pass through unchanged.
        assert_eq!(LogLevel::Off.libkrun_log_level(), 2);
        assert_eq!(LogLevel::Error.libkrun_log_level(), 2);
        assert_eq!(LogLevel::Warn.libkrun_log_level(), 2);
        assert_eq!(LogLevel::Info.libkrun_log_level(), 3);
        assert_eq!(LogLevel::Debug.libkrun_log_level(), 4);
        assert_eq!(LogLevel::Trace.libkrun_log_level(), 5);
    }

    #[test]
    fn log_level_maps_to_libkrun_numeric_contract() {
        assert_eq!(LogLevel::Off.libkrun_level(), 0);
        assert_eq!(LogLevel::Error.libkrun_level(), 1);
        assert_eq!(LogLevel::Warn.libkrun_level(), 2);
        assert_eq!(LogLevel::Info.libkrun_level(), 3);
        assert_eq!(LogLevel::Debug.libkrun_level(), 4);
        assert_eq!(LogLevel::Trace.libkrun_level(), 5);
    }

    #[test]
    fn effective_level_precedence_matches_cli_contract() {
        assert_eq!(
            LogSettings::resolve(Some(LogLevel::Info), true, Some("trace")).level,
            LogLevel::Info
        );
        assert_eq!(
            LogSettings::resolve(None, true, Some("trace")).level,
            LogLevel::Debug
        );
        assert_eq!(
            LogSettings::resolve(None, false, Some("trace")).level,
            LogLevel::Trace
        );
        assert_eq!(
            LogSettings::resolve(None, false, Some("cang=debug")).level,
            LogLevel::Off
        );
        assert_eq!(LogSettings::resolve(None, false, None).level, LogLevel::Off);
    }

    #[test]
    fn scalar_rust_log_is_not_guessed_from_compound_filters() {
        assert_eq!(parse_scalar_rust_log("debug"), Some(LogLevel::Debug));
        assert_eq!(parse_scalar_rust_log("warn"), Some(LogLevel::Warn));
        assert_eq!(parse_scalar_rust_log("cang=debug"), None);
        assert_eq!(parse_scalar_rust_log("debug,libkrun=trace"), None);
    }
}
