use anyhow::{Context, Result, anyhow};
use std::ffi::CString;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;

use crate::guest_init::components::env::{GuestPermission, GuestPermissions};
use crate::guest_init::components::home::identity::DevIdentity;

const CAP_NET_ADMIN: u32 = 12;
const CAP_NET_RAW: u32 = 13;
const CAP_BPF: u32 = 39;
const CAP_SYS_ADMIN: u32 = 21;
const CAP_SETUID: u32 = 7;
const CAP_SETGID: u32 = 6;
const CAP_DAC_OVERRIDE: u32 = 1;
const ROOTLESS_IDMAP_CAPABILITIES: [u32; 3] = [CAP_SETUID, CAP_SETGID, CAP_DAC_OVERRIDE];

const VIDEO_GID: libc::gid_t = 44;
const RENDER_GID: libc::gid_t = 107;
const DEV_SUPPLEMENTARY_GROUPS: &[libc::gid_t] = &[VIDEO_GID, RENDER_GID];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::guest_init) struct WorkloadCapabilities {
    values: [u32; 4],
    len: usize,
}

impl WorkloadCapabilities {
    pub(in crate::guest_init) fn as_slice(&self) -> &[u32] {
        &self.values[..self.len]
    }

    fn contains(&self, capability: u32) -> bool {
        self.as_slice().contains(&capability)
    }

    #[cfg(test)]
    fn is_empty(self) -> bool {
        self.len == 0
    }
}

pub(in crate::guest_init) fn workload_capability_plan(
    new_permissions: GuestPermissions,
) -> WorkloadCapabilities {
    let mut capabilities = WorkloadCapabilities::default();
    if new_permissions.contains(GuestPermission::NetAdmin) {
        capabilities.values[capabilities.len] = CAP_NET_ADMIN;
        capabilities.len += 1;
    }
    if new_permissions.contains(GuestPermission::NetRaw) {
        capabilities.values[capabilities.len] = CAP_NET_RAW;
        capabilities.len += 1;
    }
    if new_permissions.contains(GuestPermission::Bpf) {
        capabilities.values[capabilities.len] = CAP_BPF;
        capabilities.len += 1;
    }
    if new_permissions.contains(GuestPermission::SysAdmin) {
        capabilities.values[capabilities.len] = CAP_SYS_ADMIN;
        capabilities.len += 1;
    }
    capabilities
}

fn retains_bounding_capability(capability: u32, new_capabilities: WorkloadCapabilities) -> bool {
    ROOTLESS_IDMAP_CAPABILITIES.contains(&capability) || new_capabilities.contains(capability)
}

fn restrict_capability_bounding_set(new_capabilities: WorkloadCapabilities) -> io::Result<()> {
    const CAP_SETPCAP: u32 = 8;
    for capability in (0..=40).filter(|capability| *capability != CAP_SETPCAP) {
        if retains_bounding_capability(capability, new_capabilities) {
            continue;
        }
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if !retains_bounding_capability(CAP_SETPCAP, new_capabilities)
        && unsafe { libc::prctl(libc::PR_CAPBSET_DROP, CAP_SETPCAP, 0, 0, 0) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::guest_init) enum CredentialOperation {
    SupplementaryGroups(&'static [libc::gid_t]),
    PrimaryGid(libc::gid_t),
    Uid(libc::uid_t),
}

impl CredentialOperation {
    fn error_context(self) -> String {
        match self {
            Self::SupplementaryGroups(_) => "failed to set dev supplementary groups".to_owned(),
            Self::PrimaryGid(gid) => format!("failed to set gid {gid}"),
            Self::Uid(uid) => format!("failed to set uid {uid}"),
        }
    }
}

pub(in crate::guest_init) fn credential_plan(identity: &DevIdentity) -> [CredentialOperation; 3] {
    [
        CredentialOperation::SupplementaryGroups(DEV_SUPPLEMENTARY_GROUPS),
        CredentialOperation::PrimaryGid(identity.gid),
        CredentialOperation::Uid(identity.uid),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialTransitionOperation {
    RestrictCapabilityBoundingSet(WorkloadCapabilities),
    Credential(CredentialOperation),
}

fn credential_transition_plan(
    identity: &DevIdentity,
    new_permissions: GuestPermissions,
) -> Vec<CredentialTransitionOperation> {
    let capabilities = workload_capability_plan(new_permissions);
    let mut operations =
        vec![CredentialTransitionOperation::RestrictCapabilityBoundingSet(capabilities)];
    operations.extend(
        credential_plan(identity)
            .into_iter()
            .map(CredentialTransitionOperation::Credential),
    );
    operations
}

pub(in crate::guest_init) fn apply_dev_credentials(
    identity: &DevIdentity,
    new_permissions: GuestPermissions,
) -> io::Result<()> {
    for operation in credential_transition_plan(identity, new_permissions) {
        let result = match operation {
            CredentialTransitionOperation::RestrictCapabilityBoundingSet(capabilities) => {
                restrict_capability_bounding_set(capabilities)
            }
            CredentialTransitionOperation::Credential(credential) => {
                let rc = match credential {
                    CredentialOperation::SupplementaryGroups(groups) => unsafe {
                        libc::setgroups(groups.len(), groups.as_ptr())
                    },
                    CredentialOperation::PrimaryGid(gid) => unsafe { libc::setgid(gid) },
                    CredentialOperation::Uid(uid) => unsafe { libc::setuid(uid) },
                };
                if rc == 0 {
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::Error::last_os_error().kind(),
                        format!(
                            "{}: {}",
                            credential.error_context(),
                            io::Error::last_os_error()
                        ),
                    ))
                }
            }
        };
        result?;
    }
    Ok(())
}

pub(in crate::guest_init) fn uid() -> u32 {
    unsafe { libc::getuid() }
}

pub(in crate::guest_init) fn gid() -> u32 {
    unsafe { libc::getgid() }
}

pub(in crate::guest_init) fn is_root() -> bool {
    uid() == 0
}

/// Turn this process into the workload: killable again, and off the cores the
/// guest reserves for itself.
///
/// Every path that becomes a workload calls this - the raw `enter` exec, the
/// managed session's PTY child, `as-dev`, and the exec listener's child - because
/// a live arm found that a seam which applied one half and missed the other
/// shipped a default launch with no CPU bound at all (`nproc 26`, no idle core,
/// versus `nproc 24` and two idle cores on the `--root` launch; note 09 §1).
/// Keeping both halves in one call is what makes the seam list auditable.
pub(in crate::guest_init) fn become_workload() {
    allow_workload_kill();
    crate::guest_init::runtime::cpu_bound::confine_workload();
}

pub(in crate::guest_init) fn exec_command(command: &[String]) -> Result<()> {
    if command.is_empty() {
        return Err(anyhow!("cannot exec an empty command"));
    }
    become_workload();
    execvp(command)
}

pub(in crate::guest_init) fn drop_to_identity_and_exec(
    identity: &DevIdentity,
    permissions: GuestPermissions,
    command: &[String],
) -> Result<()> {
    if command.is_empty() {
        return Err(anyhow!("cannot exec an empty command"));
    }

    // The caller may be a process the guest protects - `as-dev` is reached from a
    // session context whose score is -1000, and the managed session's child is forked
    // from the supervisor - so the command it becomes must stay killable, or the
    // kernel ends up with no victim and panics (`oom_kill.c:1159-1166`). Both bounds
    // go on before the credentials drop, while the score write is still guaranteed
    // to be allowed.
    become_workload();

    apply_dev_credentials(identity, permissions)?;

    execvp(command)
}

pub(in crate::guest_init) fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

const GUEST_NOFILE_FLOOR: libc::rlim_t = 524_288;

/// System-wide open-descriptor ceiling. The guest kernel derives it from guest
/// RAM at boot, which at small `--mem` values lands below [`GUEST_NOFILE_FLOOR`].
const FILE_MAX_PATH: &str = "/proc/sys/fs/file-max";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NofileLimits {
    soft: libc::rlim_t,
    hard: libc::rlim_t,
}

/// The guest has two descriptor ceilings that must stay consistent:
/// `RLIMIT_NOFILE` caps one process, while `fs.file-max` caps the whole guest.
/// A lower system ceiling makes a process fail with `ENFILE` before it can
/// reach its own `EMFILE`.
trait NofileCeilingBackend {
    fn get_nofile_limits(&mut self) -> io::Result<NofileLimits>;
    fn set_nofile_limits(&mut self, limits: NofileLimits) -> io::Result<()>;
    fn get_file_max(&mut self) -> io::Result<libc::rlim_t>;
    fn set_file_max(&mut self, value: libc::rlim_t) -> io::Result<()>;
}

#[derive(Debug, Default)]
struct LibcNofileCeilingBackend;

impl NofileCeilingBackend for LibcNofileCeilingBackend {
    fn get_nofile_limits(&mut self) -> io::Result<NofileLimits> {
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) };
        if rc == 0 {
            Ok(NofileLimits {
                soft: limits.rlim_cur,
                hard: limits.rlim_max,
            })
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn set_nofile_limits(&mut self, limits: NofileLimits) -> io::Result<()> {
        let raw_limits = libc::rlimit {
            rlim_cur: limits.soft,
            rlim_max: limits.hard,
        };
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw_limits) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn get_file_max(&mut self) -> io::Result<libc::rlim_t> {
        let text = std::fs::read_to_string(FILE_MAX_PATH)?;
        text.trim().parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid value in {FILE_MAX_PATH}"),
            )
        })
    }

    fn set_file_max(&mut self, value: libc::rlim_t) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(FILE_MAX_PATH)?;
        file.write_all(format!("{value}\n").as_bytes())
    }
}

pub(in crate::guest_init) fn ensure_nofile_floor() -> Result<()> {
    let mut backend = LibcNofileCeilingBackend;
    ensure_nofile_floor_with(&mut backend)
}

fn ensure_nofile_floor_with(backend: &mut impl NofileCeilingBackend) -> Result<()> {
    let current = backend
        .get_nofile_limits()
        .context("failed to read guest RLIMIT_NOFILE before launching the guest shell")?;
    let requested = plan_nofile_floor(current)?;
    if requested != current {
        backend.set_nofile_limits(requested).with_context(|| {
            format!(
                "failed to raise guest RLIMIT_NOFILE from soft={} hard={} to soft={} hard={} before launching the guest shell",
                current.soft, current.hard, requested.soft, requested.hard
            )
        })?;
    }
    ensure_file_max_floor(backend, requested.hard)
}

/// The system-wide ceiling must never bind before a single process reaches its
/// own hard limit, which is the largest descriptor count one process may hold.
/// Values the kernel already reports above the hard limit are left alone.
fn ensure_file_max_floor(
    backend: &mut impl NofileCeilingBackend,
    hard_limit: libc::rlim_t,
) -> Result<()> {
    let current = backend.get_file_max().context(
        "failed to read guest fs.file-max before matching it to the guest RLIMIT_NOFILE",
    )?;
    let requested = plan_file_max_floor(current, hard_limit);
    if requested == current {
        return Ok(());
    }
    backend.set_file_max(requested).with_context(|| {
        format!(
            "failed to raise guest fs.file-max from {current} to {requested} so it covers the guest RLIMIT_NOFILE hard limit before launching the guest shell"
        )
    })
}

fn plan_file_max_floor(current: libc::rlim_t, hard_limit: libc::rlim_t) -> libc::rlim_t {
    current.max(hard_limit)
}

fn plan_nofile_floor(current: NofileLimits) -> Result<NofileLimits> {
    if current.soft > current.hard {
        return Err(anyhow!(
            "guest RLIMIT_NOFILE is invalid: soft limit {} is greater than hard limit {}",
            current.soft,
            current.hard
        ));
    }
    Ok(NofileLimits {
        soft: current.soft.max(GUEST_NOFILE_FLOOR),
        hard: current.hard.max(GUEST_NOFILE_FLOOR),
    })
}

/// `/proc/<pid>/oom_score_adj` value that puts a task outside the OOM
/// killer's reach (`OOM_SCORE_ADJ_MIN` in the kernel).
const PROTECTED_OOM_SCORE_ADJ: i32 = -1000;

/// OOM score left on the workload subtree, which stays the guest's victim.
const WORKLOAD_OOM_SCORE_ADJ: i32 = 0;

const OOM_SCORE_ADJ_PATH: &str = "/proc/self/oom_score_adj";

/// Takes a process the guest cannot afford to lose out of the OOM killer's reach.
///
/// Two classes qualify. The managed-session supervisor, because losing it ends
/// the microVM rather than the workload: libkrun's init execs the image
/// entrypoint as a child of the real PID 1 and reboots the guest when that
/// child exits, so an OOM kill here is indistinguishable from the task finishing.
/// And the detached `internal` workers that own state the session then depends
/// on - the zram device, the `/nix` overlay seed, the Podman store, the Pulse
/// bridge - because a worker killed mid-way leaves that state half-built for the
/// life of the guest.
///
/// Nothing in the workload subtree is covered: the shell, `cargo` and `rustc`
/// keep score [`WORKLOAD_OOM_SCORE_ADJ`] and stay the kernel's victim (see
/// [`allow_workload_kill`]), or a guest that is out of memory has nobody left to
/// kill and panics instead of reclaiming. A worker therefore calls this itself,
/// as its first act, rather than the spawner writing the child's `/proc` entry:
/// the score then holds from the worker's own first instruction with no window
/// between `fork` and write. A failed adjustment is reported and ignored, because
/// a guest that cannot raise its own score still works, only less safely.
pub(in crate::guest_init) fn protect_from_oom(what: &str) {
    if let Err(err) = protect_from_oom_at(OOM_SCORE_ADJ_PATH, what) {
        eprintln!("cang-guest-init: {err:#}");
    }
}

fn protect_from_oom_at(path: &str, what: &str) -> Result<()> {
    set_oom_score_adj(path, PROTECTED_OOM_SCORE_ADJ)
        .with_context(|| format!("{what} is not protected from the OOM killer"))
}

/// Restores the default OOM score on a workload process.
///
/// Called in the forked child before it drops privileges: a child inherits the
/// protected score, and an inherited [`PROTECTED_OOM_SCORE_ADJ`] would leave
/// a memory-hungry workload as unkillable as the supervisor, which ends in the
/// kernel's "no killable processes" panic.
pub(in crate::guest_init) fn allow_workload_kill() {
    if let Err(err) = set_oom_score_adj(OOM_SCORE_ADJ_PATH, WORKLOAD_OOM_SCORE_ADJ) {
        eprintln!("cang-guest-init: guest workload OOM score not restored: {err:#}");
    }
}

fn set_oom_score_adj(path: &str, score: i32) -> Result<()> {
    std::fs::write(path, format!("{score}\n"))
        .with_context(|| format!("failed to write {score} to {path}"))
}

fn execvp(command: &[String]) -> Result<()> {
    let c_strings = command
        .iter()
        .map(|arg| CString::new(arg.as_str()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut argv = c_strings
        .iter()
        .map(|arg| arg.as_ptr())
        .collect::<Vec<*const libc::c_char>>();
    argv.push(std::ptr::null());

    unsafe {
        libc::execvp(c_strings[0].as_ptr(), argv.as_ptr());
    }
    Err(std::io::Error::last_os_error()).with_context(|| format!("failed to exec {}", command[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// System ceiling the kernel reports on a guest whose RAM derives a
    /// `fs.file-max` above every hard limit these tests use.
    const FAKE_FILE_MAX: libc::rlim_t = 1_048_576;

    #[derive(Debug)]
    struct FakeNofileBackend {
        current: io::Result<NofileLimits>,
        set_error: Option<i32>,
        set_calls: Vec<NofileLimits>,
        file_max: io::Result<libc::rlim_t>,
        set_file_max_error: Option<i32>,
        file_max_set_calls: Vec<libc::rlim_t>,
    }

    impl FakeNofileBackend {
        fn with_limits(soft: libc::rlim_t, hard: libc::rlim_t) -> Self {
            Self {
                current: Ok(NofileLimits { soft, hard }),
                set_error: None,
                set_calls: Vec::new(),
                file_max: Ok(FAKE_FILE_MAX),
                set_file_max_error: None,
                file_max_set_calls: Vec::new(),
            }
        }

        fn with_get_error(errno: i32) -> Self {
            Self {
                current: Err(io::Error::from_raw_os_error(errno)),
                ..Self::with_limits(0, 0)
            }
        }

        fn with_file_max(mut self, value: libc::rlim_t) -> Self {
            self.file_max = Ok(value);
            self
        }

        fn with_file_max_error(mut self, errno: i32) -> Self {
            self.file_max = Err(io::Error::from_raw_os_error(errno));
            self
        }

        fn with_set_file_max_error(mut self, errno: i32) -> Self {
            self.set_file_max_error = Some(errno);
            self
        }
    }

    impl NofileCeilingBackend for FakeNofileBackend {
        fn get_nofile_limits(&mut self) -> io::Result<NofileLimits> {
            self.current.as_ref().map(|limits| *limits).map_err(|err| {
                io::Error::from_raw_os_error(err.raw_os_error().unwrap_or(libc::EIO))
            })
        }

        fn set_nofile_limits(&mut self, limits: NofileLimits) -> io::Result<()> {
            self.set_calls.push(limits);
            self.set_error
                .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
        }

        fn get_file_max(&mut self) -> io::Result<libc::rlim_t> {
            self.file_max.as_ref().map(|value| *value).map_err(|err| {
                io::Error::from_raw_os_error(err.raw_os_error().unwrap_or(libc::EIO))
            })
        }

        fn set_file_max(&mut self, value: libc::rlim_t) -> io::Result<()> {
            self.file_max_set_calls.push(value);
            if let Some(errno) = self.set_file_max_error {
                return Err(io::Error::from_raw_os_error(errno));
            }
            self.file_max = Ok(value);
            Ok(())
        }
    }

    #[test]
    fn selected_workload_capabilities_map_exactly() {
        assert_eq!(
            workload_capability_plan("sys-admin".parse().expect("permissions should parse"),)
                .as_slice(),
            [CAP_SYS_ADMIN]
        );
        assert_eq!(
            workload_capability_plan(
                "net-admin,net-raw,bpf,sys-admin"
                    .parse()
                    .expect("permissions should parse"),
            )
            .as_slice(),
            [CAP_NET_ADMIN, CAP_NET_RAW, CAP_BPF, CAP_SYS_ADMIN]
        );
        assert_eq!(
            workload_capability_plan("bpf".parse().expect("permission should parse")).as_slice(),
            [CAP_BPF]
        );
    }

    #[test]
    fn unselected_workload_capabilities_are_empty() {
        assert!(workload_capability_plan(Default::default()).is_empty());
    }

    #[test]
    fn rootless_idmap_capabilities_are_retained_only_in_the_bounding_set() {
        let new_capabilities = workload_capability_plan(Default::default());

        assert!(retains_bounding_capability(CAP_SETUID, new_capabilities));
        assert!(retains_bounding_capability(CAP_SETGID, new_capabilities));
        assert!(retains_bounding_capability(
            CAP_DAC_OVERRIDE,
            new_capabilities
        ));
        assert!(!new_capabilities.contains(CAP_SETUID));
        assert!(!new_capabilities.contains(CAP_SETGID));
        assert!(!new_capabilities.contains(CAP_DAC_OVERRIDE));
        assert!(!retains_bounding_capability(
            CAP_SYS_ADMIN,
            new_capabilities
        ));
    }

    #[test]
    fn new_permissions_extend_the_rootless_idmap_bounding_set() {
        let new_permissions = "net-raw,sys-admin"
            .parse()
            .expect("permissions should parse");
        let new_capabilities = workload_capability_plan(new_permissions);

        assert!(retains_bounding_capability(CAP_SETUID, new_capabilities));
        assert!(retains_bounding_capability(CAP_SETGID, new_capabilities));
        assert!(retains_bounding_capability(
            CAP_DAC_OVERRIDE,
            new_capabilities
        ));
        assert!(retains_bounding_capability(CAP_NET_RAW, new_capabilities));
        assert!(retains_bounding_capability(CAP_SYS_ADMIN, new_capabilities));
        assert!(new_capabilities.contains(CAP_NET_RAW));
        assert!(new_capabilities.contains(CAP_SYS_ADMIN));

        let bpf_capabilities =
            workload_capability_plan("bpf".parse().expect("bpf permission should parse"));
        assert!(!retains_bounding_capability(
            CAP_SYS_ADMIN,
            bpf_capabilities
        ));
    }

    #[test]
    fn dev_supplementary_groups_include_wayland_device_groups() {
        assert_eq!(DEV_SUPPLEMENTARY_GROUPS, &[VIDEO_GID, RENDER_GID]);
    }

    #[test]
    fn credential_transition_keeps_new_permissions_out_of_active_workload_sets() {
        let identity = DevIdentity::new(1000, 1000, "/bin/sh".into());
        let permissions = "net-raw".parse().expect("net-raw permission should parse");
        let capabilities = workload_capability_plan(permissions);

        assert_eq!(
            credential_transition_plan(&identity, permissions),
            [
                CredentialTransitionOperation::RestrictCapabilityBoundingSet(capabilities),
                CredentialTransitionOperation::Credential(
                    CredentialOperation::SupplementaryGroups(DEV_SUPPLEMENTARY_GROUPS),
                ),
                CredentialTransitionOperation::Credential(CredentialOperation::PrimaryGid(1000)),
                CredentialTransitionOperation::Credential(CredentialOperation::Uid(1000)),
            ]
        );
    }

    #[test]
    fn dev_credential_plan_preserves_privilege_drop_order() {
        let identity = DevIdentity::new(1000, 1000, "/bin/sh".into());

        assert_eq!(
            credential_plan(&identity),
            [
                CredentialOperation::SupplementaryGroups(DEV_SUPPLEMENTARY_GROUPS),
                CredentialOperation::PrimaryGid(identity.gid),
                CredentialOperation::Uid(identity.uid),
            ]
        );
    }

    #[test]
    fn dev_credential_operations_preserve_syscall_error_context() {
        let identity = DevIdentity::new(1000, 1000, "/bin/sh".into());

        assert_eq!(
            credential_plan(&identity).map(CredentialOperation::error_context),
            [
                "failed to set dev supplementary groups".to_owned(),
                "failed to set gid 1000".to_owned(),
                "failed to set uid 1000".to_owned(),
            ]
        );
    }

    #[test]
    fn nofile_floor_preserves_higher_limits_without_setrlimit() {
        let mut backend = FakeNofileBackend::with_limits(600_000, 700_000);

        ensure_nofile_floor_with(&mut backend).expect("limits above floor should pass");

        assert!(backend.set_calls.is_empty());
        assert!(
            backend.file_max_set_calls.is_empty(),
            "a system ceiling above the hard limit must be left alone"
        );
    }

    #[test]
    fn nofile_floor_raises_system_file_max_to_the_hard_limit() {
        let mut backend = FakeNofileBackend::with_limits(1024, 4096).with_file_max(401_676);

        ensure_nofile_floor_with(&mut backend).expect("below-floor ceilings should be raised");

        assert_eq!(backend.file_max_set_calls, [GUEST_NOFILE_FLOOR]);
    }

    #[test]
    fn nofile_floor_tracks_a_hard_limit_above_the_floor() {
        let mut backend = FakeNofileBackend::with_limits(600_000, 700_000).with_file_max(401_676);

        ensure_nofile_floor_with(&mut backend)
            .expect("system ceiling should follow the hard limit");

        assert!(
            backend.set_calls.is_empty(),
            "an unchanged RLIMIT_NOFILE must not be set again"
        );
        assert_eq!(backend.file_max_set_calls, [700_000]);
    }

    #[test]
    fn nofile_floor_raises_file_max_even_when_rlimit_needs_no_change() {
        let mut backend = FakeNofileBackend::with_limits(GUEST_NOFILE_FLOOR, GUEST_NOFILE_FLOOR)
            .with_file_max(401_676);

        ensure_nofile_floor_with(&mut backend).expect("the system ceiling should still be raised");

        assert!(backend.set_calls.is_empty());
        assert_eq!(backend.file_max_set_calls, [GUEST_NOFILE_FLOOR]);
    }

    #[test]
    fn nofile_floor_reports_file_max_failures() {
        let mut backend =
            FakeNofileBackend::with_limits(1024, 4096).with_file_max_error(libc::ENOENT);
        let err =
            ensure_nofile_floor_with(&mut backend).expect_err("unreadable file-max should fail");
        assert!(
            format!("{err:#}").contains("failed to read guest fs.file-max"),
            "unexpected error: {err:#}"
        );

        let mut backend = FakeNofileBackend::with_limits(1024, 4096)
            .with_file_max(401_676)
            .with_set_file_max_error(libc::EROFS);
        let err =
            ensure_nofile_floor_with(&mut backend).expect_err("unwritable file-max should fail");
        assert!(
            format!("{err:#}").contains("failed to raise guest fs.file-max"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn plan_file_max_floor_never_lowers_the_kernel_value() {
        assert_eq!(
            plan_file_max_floor(401_676, GUEST_NOFILE_FLOOR),
            GUEST_NOFILE_FLOOR
        );
        assert_eq!(plan_file_max_floor(1_048_576, 4096), 1_048_576);
        assert_eq!(plan_file_max_floor(4096, 4096), 4096);
    }

    #[test]
    fn nofile_floor_raises_both_soft_and_hard_when_below_floor() {
        let mut backend = FakeNofileBackend::with_limits(1024, 4096);

        ensure_nofile_floor_with(&mut backend).expect("below-floor limits should be raised");

        assert_eq!(
            backend.set_calls,
            [NofileLimits {
                soft: GUEST_NOFILE_FLOOR,
                hard: GUEST_NOFILE_FLOOR,
            }]
        );
    }

    #[test]
    fn nofile_floor_raises_only_soft_when_hard_is_already_high_enough() {
        let mut backend = FakeNofileBackend::with_limits(1024, 700_000);

        ensure_nofile_floor_with(&mut backend).expect("soft limit should be raised to floor");

        assert_eq!(
            backend.set_calls,
            [NofileLimits {
                soft: GUEST_NOFILE_FLOOR,
                hard: 700_000,
            }]
        );
    }

    #[test]
    fn nofile_floor_rejects_invalid_limits() {
        let err = plan_nofile_floor(NofileLimits {
            soft: 4096,
            hard: 1024,
        })
        .expect_err("soft above hard should fail");

        assert!(
            format!("{err:#}").contains("guest RLIMIT_NOFILE is invalid"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn nofile_floor_reports_getrlimit_failure() {
        let mut backend = FakeNofileBackend::with_get_error(libc::EPERM);

        let err =
            ensure_nofile_floor_with(&mut backend).expect_err("getrlimit failure should surface");

        assert!(format!("{err:#}").contains("failed to read guest RLIMIT_NOFILE"));
    }

    #[test]
    fn nofile_floor_reports_setrlimit_failure() {
        let mut backend = FakeNofileBackend::with_limits(1024, 4096);
        backend.set_error = Some(libc::EPERM);

        let err =
            ensure_nofile_floor_with(&mut backend).expect_err("setrlimit failure should surface");

        assert!(format!("{err:#}").contains("failed to raise guest RLIMIT_NOFILE"));
    }

    #[test]
    fn oom_protection_writes_the_unkillable_score() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oom_score_adj");

        set_oom_score_adj(path.to_str().unwrap(), PROTECTED_OOM_SCORE_ADJ).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "-1000\n");
    }

    /// Every protected process writes the same score, whether it is the session
    /// supervisor or a detached worker: one definition of "the guest cannot lose
    /// this".
    #[test]
    fn a_detached_worker_protects_itself_with_the_same_score_as_the_supervisor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_path = dir.path().join("supervisor");
        let worker_path = dir.path().join("worker");

        protect_from_oom_at(agent_path.to_str().unwrap(), "guest session supervisor").unwrap();
        protect_from_oom_at(worker_path.to_str().unwrap(), "guest swap worker").unwrap();

        assert_eq!(std::fs::read_to_string(&agent_path).unwrap(), "-1000\n");
        assert_eq!(std::fs::read_to_string(&worker_path).unwrap(), "-1000\n");
    }

    /// The guard reports rather than fails: a guest that cannot raise its own
    /// score still boots, and the message names which piece is unprotected so a
    /// guest console says what protection was lost.
    #[test]
    fn a_worker_that_cannot_be_protected_reports_itself_by_name() {
        let err = protect_from_oom_at("/nonexistent-oom/oom_score_adj", "guest swap worker")
            .expect_err("a missing path must fail");

        let message = format!("{err:#}");
        assert!(
            message.contains("guest swap worker is not protected"),
            "unexpected error: {message}"
        );
    }

    /// One call carries both halves of "this process is the workload", and a seam
    /// that uses it cannot miss one. The live arm proved *that* matters: the
    /// default launch shipped with the killable half and not the CPU half, `nproc
    /// 26` and no idle core (note 09 §1).
    #[test]
    fn becoming_the_workload_applies_both_bounds() {
        let cpus = crate::guest_init::runtime::cpu_bound::guest_cpu_count();
        let size = std::mem::size_of::<libc::cpu_set_t>();
        let mut before: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: pid 0 is this process and the set is correctly sized.
        unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut before), 0) };

        // The score half writes `/proc/self/oom_score_adj` for real; 0 is always
        // permitted, so this needs no privilege.
        become_workload();

        // On a one-vCPU host `workload_cpu_range` is `None` and only the score half
        // applies, so the mask assertion is skipped rather than faked.
        if let Some((start, end)) = crate::guest_init::runtime::cpu_bound::workload_cpu_range(cpus)
        {
            let mut after: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            unsafe { assert_eq!(libc::sched_getaffinity(0, size, &mut after), 0) };
            for cpu in 0..libc::CPU_SETSIZE as usize {
                // SAFETY: `CPU_ISSET` only reads the set.
                let in_mask = unsafe { libc::CPU_ISSET(cpu, &after) };
                assert_eq!(in_mask, cpu >= start && cpu < end, "cpu {cpu}");
            }
            // SAFETY: restore, so the rest of the suite is not confined.
            unsafe { assert_eq!(libc::sched_setaffinity(0, size, &before), 0) };
        }

        let score = std::fs::read_to_string("/proc/self/oom_score_adj").expect("score");
        assert_eq!(
            score.trim(),
            WORKLOAD_OOM_SCORE_ADJ.to_string(),
            "the workload is killable again"
        );
    }

    #[test]
    fn workload_restore_writes_the_default_score() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oom_score_adj");

        set_oom_score_adj(path.to_str().unwrap(), WORKLOAD_OOM_SCORE_ADJ).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0\n");
    }

    #[test]
    fn workload_score_stays_killable_next_to_the_supervisor() {
        // The supervisor is exempt from the OOM killer, so the workload has to
        // remain the more killable of the two or the kernel runs out of
        // victims and panics instead of reclaiming.
        const { assert!(WORKLOAD_OOM_SCORE_ADJ > PROTECTED_OOM_SCORE_ADJ) };
    }

    #[test]
    fn oom_score_adjust_surfaces_write_failures() {
        let err = set_oom_score_adj("/nonexistent-oom/oom_score_adj", PROTECTED_OOM_SCORE_ADJ)
            .expect_err("a missing path must fail");

        assert!(format!("{err:#}").contains("oom_score_adj"));
    }
}
