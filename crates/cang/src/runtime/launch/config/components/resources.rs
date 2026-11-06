//! CPU and memory contributions to the serialized launch config.

use anyhow::{Context, Result, anyhow};
use std::fs;

use super::super::model::{BYTES_PER_GIB, HOST_MEMINFO, KIB, MAX_GIB_FOR_KRUN_RAM_MIB};

pub(crate) fn resolve_cpu_count() -> Result<u8> {
    let available = std::thread::available_parallelism()
        .context("failed to detect available CPUs for cang default")?
        .get();
    let count = if available <= 6 {
        available
    } else {
        available - 2
    };
    u8::try_from(count).context("host available CPU count is too large for libkrun vcpu config")
}

pub(crate) fn resolve_ram_mib(mem_gib: Option<u32>) -> Result<u32> {
    match mem_gib {
        Some(gib) => {
            let mib = mem_gib_to_mib(gib)?;
            check_over_commit(gib, mib)?;
            Ok(mib)
        }
        None => {
            let meminfo = fs::read_to_string(HOST_MEMINFO)
                .with_context(|| format!("failed to read host memory from {HOST_MEMINFO}"))?;
            default_ram_mib_from_meminfo(&meminfo)
        }
    }
}

/// Share of host RAM at which an explicit `--mem` is refused.
///
/// Guest RAM is one anonymous mapping of exactly this size, resident as soon as
/// the guest touches it, so a guest above this line cannot be backed without the
/// host swapping. Measured on a 20 GiB host: `--mem 22` (110%) with eighteen
/// rustc processes put the host at 45 MB/s swap-in and 47 MB/s swap-out with
/// 27-35% iowait, and the guest's own zram swap thrashing on top - a state that
/// only recovery from is killing the task.
const OVER_COMMIT_REFUSE_PERCENT: u64 = 90;

/// Share of host RAM at which an explicit `--mem` is allowed but warned about.
///
/// This is the same 80% the *default* already uses, so the warning only ever
/// fires for a launch that asked for more than cang would have chosen itself.
const OVER_COMMIT_WARN_PERCENT: u64 = 80;

/// What an explicit `--mem` asks of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverCommit {
    /// Inside the default's own budget.
    WithinDefault,
    /// Above the default, below the refusal line: allowed, with a warning.
    Warn { percent: u64 },
    /// Above the refusal line.
    Refuse { percent: u64, max_gib: u32 },
}

/// Classifies `guest_mib` against `host_bytes`, in whole percent of host RAM.
pub(crate) fn over_commit(guest_mib: u32, host_bytes: u64) -> OverCommit {
    if host_bytes == 0 {
        return OverCommit::WithinDefault;
    }
    let guest_bytes = u128::from(guest_mib) * 1024 * u128::from(KIB);
    let percent = (guest_bytes * 100 / u128::from(host_bytes)) as u64;
    if percent > OVER_COMMIT_REFUSE_PERCENT {
        let max_bytes = u128::from(host_bytes) * u128::from(OVER_COMMIT_REFUSE_PERCENT) / 100;
        let max_gib = u32::try_from(max_bytes / u128::from(BYTES_PER_GIB)).unwrap_or(u32::MAX);
        return OverCommit::Refuse { percent, max_gib };
    }
    if percent > OVER_COMMIT_WARN_PERCENT {
        return OverCommit::Warn { percent };
    }
    OverCommit::WithinDefault
}

/// Refuses an `--mem` the host cannot back, and warns above the default's line.
///
/// A host whose memory cannot be read is left alone: the guard exists to stop a
/// launch that cannot work, not to refuse launches on hosts it does not
/// understand.
fn check_over_commit(gib: u32, mib: u32) -> Result<()> {
    let Ok(meminfo) = fs::read_to_string(HOST_MEMINFO) else {
        return Ok(());
    };
    let Ok(host_bytes) = parse_meminfo_total_bytes(&meminfo) else {
        return Ok(());
    };
    match over_commit(mib, host_bytes) {
        OverCommit::WithinDefault => Ok(()),
        OverCommit::Warn { percent } => {
            tracing::warn!(
                mem_gib = gib,
                percent,
                "cang --mem claims {percent}% of host memory; the guest's RAM is resident as it \
                 is touched, so the host will swap and the guest will thrash. cang's default is \
                 80%, and anything above {OVER_COMMIT_REFUSE_PERCENT}% is refused."
            );
            Ok(())
        }
        OverCommit::Refuse { percent, max_gib } => Err(anyhow!(
            "cang --mem {gib} GiB is {percent}% of this host's memory, which cannot be backed \
             without the host swapping; use at most {max_gib} GiB ({}% of host RAM)",
            OVER_COMMIT_REFUSE_PERCENT
        )),
    }
}

pub(crate) fn default_ram_mib_from_meminfo(meminfo: &str) -> Result<u32> {
    let host_bytes = parse_meminfo_total_bytes(meminfo)?;
    let default_gib = default_mem_gib_from_host_bytes(host_bytes)?;
    mem_gib_to_mib(default_gib)
}

pub(crate) fn mem_gib_to_mib(gib: u32) -> Result<u32> {
    validate_mem_gib(gib)?;
    gib.checked_mul(1024)
        .ok_or_else(|| anyhow!("cang --mem is too large for libkrun ram_mib"))
}

pub(crate) fn validate_mem_gib(gib: u32) -> Result<()> {
    if gib == 0 {
        anyhow::bail!("cang --mem must be at least 1 GiB");
    }
    if gib > MAX_GIB_FOR_KRUN_RAM_MIB {
        anyhow::bail!("cang --mem must be at most {MAX_GIB_FOR_KRUN_RAM_MIB} GiB");
    }
    Ok(())
}

pub(crate) fn default_mem_gib_from_host_bytes(host_bytes: u64) -> Result<u32> {
    let eighty_percent_bytes = (host_bytes / 5)
        .saturating_mul(4)
        .saturating_add((host_bytes % 5).saturating_mul(4) / 5);
    let default_gib = eighty_percent_bytes / BYTES_PER_GIB;

    if default_gib == 0 {
        anyhow::bail!("host memory is too small to derive a cang --mem default of at least 1 GiB");
    }

    let default_gib =
        u32::try_from(default_gib).context("host memory is too large to fit cang --mem default")?;
    validate_mem_gib(default_gib)?;
    Ok(default_gib)
}

pub(crate) fn parse_meminfo_total_bytes(meminfo: &str) -> Result<u64> {
    let mem_total_line = meminfo
        .lines()
        .find(|line| line.starts_with("MemTotal:"))
        .ok_or_else(|| {
            anyhow!("host memory detection failed: MemTotal missing from {HOST_MEMINFO}")
        })?;

    let mut fields = mem_total_line.split_whitespace();
    let _label = fields.next();
    let value = fields
        .next()
        .ok_or_else(|| anyhow!("host memory detection failed: MemTotal value missing"))?;
    let unit = fields
        .next()
        .ok_or_else(|| anyhow!("host memory detection failed: MemTotal unit missing"))?;

    if unit != "kB" {
        anyhow::bail!("host memory detection failed: expected MemTotal in kB, got {unit}");
    }

    let kib = value
        .parse::<u64>()
        .with_context(|| format!("host memory detection failed: invalid MemTotal value {value}"))?;
    kib.checked_mul(KIB)
        .ok_or_else(|| anyhow!("host memory detection failed: MemTotal overflows bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWENTY_GIB: u64 = 20 * 1024 * 1024 * 1024;

    /// An explicit `--mem` above the refusal line is a launch that cannot work:
    /// the guest's RAM is resident as the guest touches it, so the host swaps.
    /// The measured case was `--mem 22` on a 20 GiB host.
    #[test]
    fn refuses_an_explicit_mem_above_ninety_percent_of_host_ram() {
        assert_eq!(
            over_commit(22 * 1024, TWENTY_GIB),
            OverCommit::Refuse {
                percent: 110,
                max_gib: 18
            }
        );
    }

    /// Between the default's 80% and the refusal line the launch is allowed and
    /// warned about, so a deliberate over-provision is possible but loud.
    #[test]
    fn warns_between_the_default_budget_and_the_refusal_line() {
        assert_eq!(
            over_commit(18 * 1024, TWENTY_GIB),
            OverCommit::Warn { percent: 90 }
        );
        assert_eq!(
            over_commit(17 * 1024, TWENTY_GIB),
            OverCommit::Warn { percent: 85 }
        );
    }

    /// At or below the default's own 80% an explicit `--mem` is exactly what
    /// cang would have chosen, so it is neither refused nor warned about.
    #[test]
    fn accepts_an_explicit_mem_within_the_default_budget() {
        assert_eq!(
            over_commit(16 * 1024, TWENTY_GIB),
            OverCommit::WithinDefault
        );
        assert_eq!(over_commit(8 * 1024, TWENTY_GIB), OverCommit::WithinDefault);
    }

    /// A host whose memory cannot be read must not refuse launches: the guard
    /// exists to stop a launch that cannot work, not to break unknown hosts.
    #[test]
    fn unknown_host_memory_never_refuses() {
        assert_eq!(over_commit(64 * 1024, 0), OverCommit::WithinDefault);
    }
}
