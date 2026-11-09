use anyhow::{Context, Result, anyhow, bail};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::runtime::launch::config::LaunchConfig;

pub(crate) const UNSHARE_PROGRAM: &str = "unshare";

const SUBUID_PATH: &str = "/etc/subuid";
const SUBGID_PATH: &str = "/etc/subgid";
const NEWUIDMAP_PROGRAM: &str = "newuidmap";
const NEWGIDMAP_PROGRAM: &str = "newgidmap";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdMapRange {
    pub(crate) inner_id: u32,
    pub(crate) outer_id: u32,
    pub(crate) count: u32,
}

impl IdMapRange {
    fn to_unshare_arg(self) -> OsString {
        OsString::from(format!(
            "{}:{}:{}",
            self.inner_id, self.outer_id, self.count
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeepIdMap {
    pub(crate) uid_ranges: Vec<IdMapRange>,
    pub(crate) gid_ranges: Vec<IdMapRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubIdRange {
    start: u32,
    count: u32,
}

impl SubIdRange {
    pub(crate) fn new(start: u32, count: u32) -> Result<Self> {
        if count == 0 {
            bail!("subordinate ID range must not be empty");
        }
        checked_end(start, count)
            .with_context(|| format!("subordinate ID range {start}:{count} overflows u32"))?;
        Ok(Self { start, count })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeepIdLauncher {
    host_uid: u32,
    host_gid: u32,
    user_label: String,
    map: KeepIdMap,
}

impl KeepIdLauncher {
    pub(crate) fn from_current_system() -> Result<Self> {
        ensure_program_available(UNSHARE_PROGRAM).with_context(|| {
            format!("cang keep-id libkrun helper requires util-linux `{UNSHARE_PROGRAM}` on PATH")
        })?;
        ensure_program_available(NEWUIDMAP_PROGRAM).with_context(|| {
            format!(
                "cang keep-id libkrun helper requires `{NEWUIDMAP_PROGRAM}` on PATH for rootless UID maps"
            )
        })?;
        ensure_program_available(NEWGIDMAP_PROGRAM).with_context(|| {
            format!(
                "cang keep-id libkrun helper requires `{NEWGIDMAP_PROGRAM}` on PATH for rootless GID maps"
            )
        })?;

        let host_uid = current_uid();
        let host_gid = current_gid();
        let user_label = current_user_label(host_uid);
        let subuid = read_subid_range(SUBUID_PATH, &user_label, host_uid).with_context(|| {
            format!("failed to resolve subordinate UID range for {user_label}/{host_uid}")
        })?;
        let subgid = read_subid_range(SUBGID_PATH, &user_label, host_uid).with_context(|| {
            format!("failed to resolve subordinate GID range for {user_label}/{host_uid}")
        })?;
        Self::from_parts(host_uid, host_gid, user_label, subuid, subgid)
    }

    pub(crate) fn from_parts(
        host_uid: u32,
        host_gid: u32,
        user_label: impl Into<String>,
        subuid: SubIdRange,
        subgid: SubIdRange,
    ) -> Result<Self> {
        let user_label = user_label.into();
        Ok(Self {
            host_uid,
            host_gid,
            map: KeepIdMap {
                uid_ranges: keep_id_ranges(host_uid, subuid, "UID", &user_label)?,
                gid_ranges: keep_id_ranges(host_gid, subgid, "GID", &user_label)?,
            },
            user_label,
        })
    }

    pub(crate) fn program(&self) -> OsString {
        OsString::from(UNSHARE_PROGRAM)
    }

    /// The `unshare` flags that create the keep-id namespace, without a command.
    ///
    /// Shared with the maintenance commands, which re-execute an ordinary CLI
    /// invocation inside the same namespace rather than an internal worker.
    pub(crate) fn namespace_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--user"),
            OsString::from("--mount"),
            OsString::from("--fork"),
            OsString::from("--kill-child"),
            OsString::from("--propagation"),
            OsString::from("private"),
        ];
        push_map_args(&mut args, "--map-users", &self.map.uid_ranges);
        push_map_args(&mut args, "--map-groups", &self.map.gid_ranges);
        args.extend([
            OsString::from("--setuid"),
            OsString::from("0"),
            OsString::from("--setgid"),
            OsString::from("0"),
            OsString::from("--keep-caps"),
        ]);
        args
    }

    pub(crate) fn args(
        &self,
        executable: &Path,
        internal_arg: &str,
        config_path: &Path,
    ) -> Vec<OsString> {
        let mut args = self.namespace_args();
        args.extend([
            executable.as_os_str().to_os_string(),
            OsString::from("internal"),
            OsString::from(internal_arg),
            config_path.as_os_str().to_os_string(),
        ]);
        args
    }

    pub(crate) fn diagnostic_summary(&self) -> String {
        format!(
            "keep-id user namespace for {} uid={} gid={} uid_map={} gid_map={}",
            self.user_label,
            self.host_uid,
            self.host_gid,
            format_ranges(&self.map.uid_ranges),
            format_ranges(&self.map.gid_ranges)
        )
    }
}

fn push_map_args(args: &mut Vec<OsString>, flag: &str, ranges: &[IdMapRange]) {
    for range in ranges {
        args.push(OsString::from(flag));
        args.push(range.to_unshare_arg());
    }
}

fn format_ranges(ranges: &[IdMapRange]) -> String {
    ranges
        .iter()
        .map(|range| format!("{}:{}:{}", range.inner_id, range.outer_id, range.count))
        .collect::<Vec<_>>()
        .join(",")
}

fn keep_id_ranges(
    host_id: u32,
    subid: SubIdRange,
    kind: &str,
    user_label: &str,
) -> Result<Vec<IdMapRange>> {
    if host_id > subid.count {
        bail!(
            "insufficient subordinate {kind} range for {user_label}: host {kind} {host_id} requires at least {host_id} subordinate IDs below the keep-id entry, but only {} are available",
            subid.count
        );
    }

    let mut ranges = Vec::with_capacity(3);
    if host_id > 0 {
        ranges.push(IdMapRange {
            inner_id: 0,
            outer_id: subid.start,
            count: host_id,
        });
    }
    ranges.push(IdMapRange {
        inner_id: host_id,
        outer_id: host_id,
        count: 1,
    });

    let upper_count = subid.count - host_id;
    if upper_count > 0 {
        let upper_inner = host_id.checked_add(1).ok_or_else(|| {
            anyhow!("keep-id {kind} map inner range overflows above host ID {host_id}")
        })?;
        let upper_outer = subid.start.checked_add(host_id).ok_or_else(|| {
            anyhow!(
                "keep-id {kind} map subordinate range overflows: {} + {host_id}",
                subid.start
            )
        })?;
        checked_end(upper_outer, upper_count).with_context(|| {
            format!("keep-id {kind} upper map {upper_outer}:{upper_count} overflows u32")
        })?;
        ranges.push(IdMapRange {
            inner_id: upper_inner,
            outer_id: upper_outer,
            count: upper_count,
        });
    }
    assert_non_overlapping(&ranges, kind)?;
    Ok(ranges)
}

fn assert_non_overlapping(ranges: &[IdMapRange], kind: &str) -> Result<()> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|range| range.inner_id);
    let mut previous_end = None;
    for range in sorted {
        let end = checked_end(range.inner_id, range.count)
            .with_context(|| format!("keep-id {kind} inner range overflows"))?;
        if let Some(previous) = previous_end
            && u64::from(range.inner_id) < previous
        {
            bail!("keep-id {kind} map contains overlapping inner ranges");
        }
        previous_end = Some(end);
    }
    Ok(())
}

fn checked_end(start: u32, count: u32) -> Option<u64> {
    u64::from(start)
        .checked_add(u64::from(count))
        .filter(|end| *end <= u64::from(u32::MAX) + 1)
}

fn read_subid_range(path: &str, user_label: &str, uid: u32) -> Result<SubIdRange> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read {path}; configure rootless subordinate IDs"))?;
    parse_subid_range(&contents, user_label, uid).ok_or_else(|| {
        anyhow!(
            "no usable subordinate ID entry for {user_label}/{uid} in {path}; add an entry such as `{user_label}:100000:65536`"
        )
    })
}

fn parse_subid_range(contents: &str, user_label: &str, uid: u32) -> Option<SubIdRange> {
    let numeric = uid.to_string();
    contents.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut fields = line.split(':');
        let owner = fields.next()?;
        let start = fields.next()?.parse::<u32>().ok()?;
        let count = fields.next()?.parse::<u32>().ok()?;
        if fields.next().is_some() || (owner != user_label && owner != numeric) {
            return None;
        }
        SubIdRange::new(start, count).ok()
    })
}

fn ensure_program_available(program: &str) -> Result<()> {
    find_program_in_path(program)
        .map(|_| ())
        .ok_or_else(|| anyhow!("{program} is not installed or not available on PATH"))
}

fn find_program_in_path(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn current_user_label(uid: u32) -> String {
    env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| env::var("LOGNAME").ok().filter(|value| !value.is_empty()))
        .or_else(|| username_from_passwd(uid))
        .unwrap_or_else(|| uid.to_string())
}

fn username_from_passwd(uid: u32) -> Option<String> {
    let contents = fs::read_to_string("/etc/passwd").ok()?;
    contents.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        fields.next()?;
        let parsed_uid = fields.next()?.parse::<u32>().ok()?;
        (parsed_uid == uid).then(|| name.to_owned())
    })
}

fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn current_gid() -> u32 {
    unsafe { libc::getgid() }
}

pub(crate) fn configure_helper_filesystem_identity_for_launch(config: &LaunchConfig) -> Result<()> {
    configure_helper_filesystem_identity(config)
}

fn configure_helper_filesystem_identity(config: &LaunchConfig) -> Result<()> {
    let host_uid = required_guest_config_u32(config, "CANG_HOST_UID")?;
    let host_gid = required_guest_config_u32(config, "CANG_HOST_GID")?;
    set_filesystem_gid(host_gid)?;
    set_filesystem_uid(host_uid)?;
    tracing::debug!(
        host_uid,
        host_gid,
        "cang internal: keep-id helper filesystem identity configured"
    );
    Ok(())
}

pub(crate) fn configure_vm_worker_filesystem_identity() -> Result<()> {
    set_filesystem_gid(0)?;
    set_filesystem_uid(0)?;
    tracing::debug!("cang internal VM worker: namespace-root filesystem identity restored");
    Ok(())
}

pub(crate) trait FilesystemIdentityScope {
    fn with_namespace_root<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T>;
}

pub(crate) struct RealFilesystemIdentityScope;

impl FilesystemIdentityScope for RealFilesystemIdentityScope {
    fn with_namespace_root<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        with_filesystem_identity(
            FilesystemIdentity { uid: 0, gid: 0 },
            &RealFilesystemIdentityOps,
            operation,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilesystemIdentity {
    uid: u32,
    gid: u32,
}

trait FilesystemIdentityOps {
    fn current_uid(&self) -> Result<u32>;
    fn current_gid(&self) -> Result<u32>;
    fn set_uid(&self, uid: u32) -> Result<()>;
    fn set_gid(&self, gid: u32) -> Result<()>;
}

struct RealFilesystemIdentityOps;

impl FilesystemIdentityOps for RealFilesystemIdentityOps {
    fn current_uid(&self) -> Result<u32> {
        current_filesystem_uid()
    }

    fn current_gid(&self) -> Result<u32> {
        current_filesystem_gid()
    }

    fn set_uid(&self, uid: u32) -> Result<()> {
        set_filesystem_uid(uid)
    }

    fn set_gid(&self, gid: u32) -> Result<()> {
        set_filesystem_gid(gid)
    }
}

fn with_filesystem_identity<T>(
    target: FilesystemIdentity,
    ops: &impl FilesystemIdentityOps,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let previous = current_filesystem_identity(ops)?;
    if let Err(err) = set_filesystem_identity(ops, target) {
        let operation_result = Err(err.context(format!(
            "failed to enter filesystem identity {}:{}",
            target.uid, target.gid
        )));
        return combine_identity_results(
            operation_result,
            restore_filesystem_identity(ops, previous),
        );
    }

    let operation_result = operation();
    let restore_result = restore_filesystem_identity(ops, previous);
    combine_identity_results(operation_result, restore_result)
}

fn current_filesystem_identity(ops: &impl FilesystemIdentityOps) -> Result<FilesystemIdentity> {
    Ok(FilesystemIdentity {
        uid: ops.current_uid()?,
        gid: ops.current_gid()?,
    })
}

fn set_filesystem_identity(
    ops: &impl FilesystemIdentityOps,
    identity: FilesystemIdentity,
) -> Result<()> {
    ops.set_gid(identity.gid)?;
    ops.set_uid(identity.uid)?;
    Ok(())
}

fn restore_filesystem_identity(
    ops: &impl FilesystemIdentityOps,
    previous: FilesystemIdentity,
) -> Result<()> {
    set_filesystem_identity(ops, previous).with_context(|| {
        format!(
            "failed to restore cang helper filesystem identity to {}:{}",
            previous.uid, previous.gid
        )
    })
}

fn combine_identity_results<T>(
    operation_result: Result<T>,
    restore_result: Result<()>,
) -> Result<T> {
    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(restore_err)) => Err(restore_err),
        (Err(operation_err), Err(restore_err)) => {
            Err(anyhow!("{operation_err:#}; additionally, {restore_err:#}"))
        }
    }
}

pub(crate) fn required_guest_config_u32(config: &LaunchConfig, key: &str) -> Result<u32> {
    let value = config
        .guest_config_env
        .iter()
        .rev()
        .find_map(|(env_key, env_value)| (env_key == key).then_some(env_value))
        .ok_or_else(|| anyhow!("cang launch config is missing required {key}"))?;
    value
        .parse::<u32>()
        .with_context(|| format!("cang launch config {key} value '{value}' is not a u32"))
}

fn set_filesystem_uid(uid: u32) -> Result<()> {
    let uid = uid as libc::uid_t;
    // SAFETY: setfsuid changes only the current process filesystem credential.
    unsafe { libc::setfsuid(uid) };
    let current = current_filesystem_uid()?;
    if current != uid as u32 {
        bail!("failed to set cang helper filesystem UID to {uid}; current fsuid is {current}");
    }
    Ok(())
}

fn set_filesystem_gid(gid: u32) -> Result<()> {
    let gid = gid as libc::gid_t;
    // SAFETY: setfsgid changes only the current process filesystem credential.
    unsafe { libc::setfsgid(gid) };
    let current = current_filesystem_gid()?;
    if current != gid as u32 {
        bail!("failed to set cang helper filesystem GID to {gid}; current fsgid is {current}");
    }
    Ok(())
}

fn current_filesystem_uid() -> Result<u32> {
    // SAFETY: uid_t::MAX is treated by Linux as an invalid fsuid probe and returns the current fsuid.
    let current = unsafe { libc::setfsuid(libc::uid_t::MAX) };
    if current < 0 {
        bail!("failed to read cang helper filesystem UID; current fsuid is {current}");
    }
    Ok(current as u32)
}

fn current_filesystem_gid() -> Result<u32> {
    // SAFETY: gid_t::MAX is treated by Linux as an invalid fsgid probe and returns the current fsgid.
    let current = unsafe { libc::setfsgid(libc::gid_t::MAX) };
    if current < 0 {
        bail!("failed to read cang helper filesystem GID; current fsgid is {current}");
    }
    Ok(current as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn subid() -> SubIdRange {
        SubIdRange::new(100_000, 65_536).unwrap()
    }

    #[test]
    fn uid_1000_map_matches_podman_keep_id_shape() {
        let ranges = keep_id_ranges(1000, subid(), "UID", "dev").unwrap();
        assert_eq!(
            ranges,
            vec![
                IdMapRange {
                    inner_id: 0,
                    outer_id: 100_000,
                    count: 1000,
                },
                IdMapRange {
                    inner_id: 1000,
                    outer_id: 1000,
                    count: 1,
                },
                IdMapRange {
                    inner_id: 1001,
                    outer_id: 101_000,
                    count: 64_536,
                },
            ]
        );
    }

    #[test]
    fn gid_993_map_has_exact_remaining_length() {
        let ranges = keep_id_ranges(993, subid(), "GID", "dev").unwrap();
        assert_eq!(
            ranges,
            vec![
                IdMapRange {
                    inner_id: 0,
                    outer_id: 100_000,
                    count: 993,
                },
                IdMapRange {
                    inner_id: 993,
                    outer_id: 993,
                    count: 1,
                },
                IdMapRange {
                    inner_id: 994,
                    outer_id: 100_993,
                    count: 64_543,
                },
            ]
        );
    }

    #[test]
    fn zero_host_id_skips_zero_length_lower_range() {
        let ranges = keep_id_ranges(0, subid(), "UID", "root").unwrap();
        assert_eq!(
            ranges,
            vec![
                IdMapRange {
                    inner_id: 0,
                    outer_id: 0,
                    count: 1,
                },
                IdMapRange {
                    inner_id: 1,
                    outer_id: 100_000,
                    count: 65_536,
                },
            ]
        );
    }

    #[test]
    fn insufficient_subordinate_range_fails_hard() {
        let err = keep_id_ranges(1000, SubIdRange::new(100_000, 999).unwrap(), "UID", "dev")
            .expect_err("short ranges must fail");
        assert!(format!("{err:#}").contains("insufficient subordinate UID range"));
    }

    #[test]
    fn subid_parser_matches_user_or_numeric_id() {
        let contents = "other:1:2\n1000:200000:10\ndev:100000:65536\n";
        assert_eq!(
            parse_subid_range(contents, "dev", 1000).unwrap(),
            SubIdRange::new(200_000, 10).unwrap()
        );
        assert_eq!(
            parse_subid_range("dev:100000:65536\n", "dev", 1000).unwrap(),
            subid()
        );
    }

    #[test]
    fn filesystem_identity_scope_restores_after_success() {
        let ops = RecordingFilesystemIdentityOps::new(1000, 993);

        let result = with_filesystem_identity(FilesystemIdentity { uid: 0, gid: 0 }, &ops, || {
            assert_eq!(ops.current_uid().unwrap(), 0);
            assert_eq!(ops.current_gid().unwrap(), 0);
            Ok("repaired")
        })
        .unwrap();

        assert_eq!(result, "repaired");
        assert_eq!(ops.current_uid().unwrap(), 1000);
        assert_eq!(ops.current_gid().unwrap(), 993);
        assert_eq!(
            ops.calls(),
            vec!["set_gid:0", "set_uid:0", "set_gid:993", "set_uid:1000"]
        );
    }

    #[test]
    fn filesystem_identity_scope_restores_after_operation_error() {
        let ops = RecordingFilesystemIdentityOps::new(1000, 993);

        let err = with_filesystem_identity(FilesystemIdentity { uid: 0, gid: 0 }, &ops, || {
            assert_eq!(ops.current_uid().unwrap(), 0);
            assert_eq!(ops.current_gid().unwrap(), 0);
            Err::<(), _>(anyhow!("repair failed"))
        })
        .unwrap_err();

        assert!(format!("{err:#}").contains("repair failed"));
        assert_eq!(ops.current_uid().unwrap(), 1000);
        assert_eq!(ops.current_gid().unwrap(), 993);
        assert_eq!(
            ops.calls(),
            vec!["set_gid:0", "set_uid:0", "set_gid:993", "set_uid:1000"]
        );
    }

    #[derive(Debug)]
    struct RecordingFilesystemIdentityOps {
        uid: Cell<u32>,
        gid: Cell<u32>,
        calls: RefCell<Vec<String>>,
    }

    impl RecordingFilesystemIdentityOps {
        fn new(uid: u32, gid: u32) -> Self {
            Self {
                uid: Cell::new(uid),
                gid: Cell::new(gid),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl FilesystemIdentityOps for RecordingFilesystemIdentityOps {
        fn current_uid(&self) -> Result<u32> {
            Ok(self.uid.get())
        }

        fn current_gid(&self) -> Result<u32> {
            Ok(self.gid.get())
        }

        fn set_uid(&self, uid: u32) -> Result<()> {
            self.calls.borrow_mut().push(format!("set_uid:{uid}"));
            self.uid.set(uid);
            Ok(())
        }

        fn set_gid(&self, gid: u32) -> Result<()> {
            self.calls.borrow_mut().push(format!("set_gid:{gid}"));
            self.gid.set(gid);
            Ok(())
        }
    }

    #[test]
    fn launcher_args_request_namespace_root_with_keep_id_maps() {
        let launcher = KeepIdLauncher::from_parts(1000, 993, "dev", subid(), subid()).unwrap();
        let args = launcher.args(
            Path::new("/nix/store/hash-cang/bin/cang"),
            "libkrun-network-enter",
            Path::new("/tmp/cang-task/launch.conf"),
        );
        let strings = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(strings.contains(&"--setuid".to_owned()));
        assert!(strings.contains(&"--setgid".to_owned()));
        assert!(strings.contains(&"--keep-caps".to_owned()));
        assert!(strings.windows(2).any(|pair| pair == ["--setuid", "0"]));
        assert!(strings.windows(2).any(|pair| pair == ["--setgid", "0"]));
        assert!(
            strings
                .windows(2)
                .any(|pair| pair == ["--map-users", "1000:1000:1"])
        );
        assert!(
            strings
                .windows(2)
                .any(|pair| pair == ["--map-groups", "993:993:1"])
        );
        assert_eq!(strings.last().unwrap(), "/tmp/cang-task/launch.conf");
    }
}
