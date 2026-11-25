use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};

use crate::guest_init::components::env::{DEFAULT_SHELL, DEV_HOME, DEV_USER};
use crate::guest_init::components::home::identity::{DevIdentity, validate_host_identity};
use crate::guest_init::{command, process};

const PASSWD_PATH: &str = "/etc/passwd";
const GROUP_PATH: &str = "/etc/group";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::guest_init) enum AsDevOperation {
    RequireRoot,
    ReadMaterializedDevAccount,
    DeriveShellEnvironment,
    ExportShellEnvironment,
    AllowWorkloadKill,
    DropAndExec,
}

#[cfg(test)]
pub(in crate::guest_init) fn planned_as_dev_operations() -> Vec<AsDevOperation> {
    vec![
        AsDevOperation::RequireRoot,
        AsDevOperation::ReadMaterializedDevAccount,
        AsDevOperation::DeriveShellEnvironment,
        AsDevOperation::ExportShellEnvironment,
        // `as-dev` can be reached from a protected session process, so the command
        // it execs must start killable (see `process::drop_to_identity_and_exec`).
        AsDevOperation::AllowWorkloadKill,
        AsDevOperation::DropAndExec,
    ]
}

pub(in crate::guest_init) fn run(command: Vec<String>) -> Result<()> {
    if !process::is_root() {
        bail!("cang-as-dev must be run from the cang root shell");
    }

    let identity =
        resolve_materialized_dev_identity(&command, Path::new(PASSWD_PATH), Path::new(GROUP_PATH))?;
    let shell_env = crate::guest_init::components::shell::env::derive(&identity, false, None);
    crate::guest_init::components::shell::env::export(&shell_env);
    let permissions = std::env::var("CANG_PERMISSIONS")
        .ok()
        .map_or(Ok(Default::default()), |value| value.parse())?;
    process::drop_to_identity_and_exec(&identity, permissions, &command)
}

fn resolve_materialized_dev_identity(
    command: &[String],
    passwd_path: &Path,
    group_path: &Path,
) -> Result<DevIdentity> {
    let account = read_materialized_dev_account(passwd_path, group_path)?;
    let shell = resolve_shell(command);
    Ok(DevIdentity::new(account.uid, account.gid, shell))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializedDevAccount {
    uid: u32,
    gid: u32,
}

fn read_materialized_dev_account(
    passwd_path: &Path,
    group_path: &Path,
) -> Result<MaterializedDevAccount> {
    let passwd = std::fs::read_to_string(passwd_path)
        .with_context(|| format!("failed to read {} for cang-as-dev", passwd_path.display()))?;
    let group = std::fs::read_to_string(group_path)
        .with_context(|| format!("failed to read {} for cang-as-dev", group_path.display()))?;
    parse_materialized_dev_account(&passwd, &group)
}

fn parse_materialized_dev_account(passwd: &str, group: &str) -> Result<MaterializedDevAccount> {
    let (uid, passwd_gid, home) = parse_dev_passwd(passwd)?;
    let group_gid = parse_dev_group(group)?;
    if passwd_gid != group_gid {
        bail!(
            "materialized dev gid mismatch between /etc/passwd ({passwd_gid}) and /etc/group ({group_gid})"
        );
    }
    if home != DEV_HOME {
        bail!("materialized dev home must be {DEV_HOME}, got {home}");
    }
    validate_host_identity(uid, passwd_gid)?;
    Ok(MaterializedDevAccount {
        uid,
        gid: passwd_gid,
    })
}

fn parse_dev_passwd(passwd: &str) -> Result<(u32, u32, String)> {
    let line = single_dev_line(passwd, "/etc/passwd")?;
    let fields = line.split(':').collect::<Vec<_>>();
    if fields.len() != 7 {
        bail!("materialized dev /etc/passwd entry must have 7 fields");
    }
    let uid = parse_u32_field(fields[2], "dev uid in /etc/passwd")?;
    let gid = parse_u32_field(fields[3], "dev gid in /etc/passwd")?;
    Ok((uid, gid, fields[5].to_owned()))
}

fn parse_dev_group(group: &str) -> Result<u32> {
    let line = single_dev_line(group, "/etc/group")?;
    let fields = line.split(':').collect::<Vec<_>>();
    if fields.len() != 4 {
        bail!("materialized dev /etc/group entry must have 4 fields");
    }
    parse_u32_field(fields[2], "dev gid in /etc/group")
}

fn single_dev_line<'a>(text: &'a str, path: &str) -> Result<&'a str> {
    let mut lines = text.lines().filter(|line| {
        line.split_once(':')
            .map(|(name, _)| name == DEV_USER)
            .unwrap_or(false)
    });
    let line = lines
        .next()
        .ok_or_else(|| anyhow!("materialized dev entry is missing from {path}"))?;
    if lines.next().is_some() {
        bail!("materialized dev entry appears more than once in {path}");
    }
    Ok(line)
}

fn parse_u32_field(value: &str, name: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .with_context(|| format!("invalid numeric {name}"))
}

fn resolve_shell(command: &[String]) -> PathBuf {
    let shell = command.first().map(String::as_str).unwrap_or(DEFAULT_SHELL);
    if shell.contains('/') {
        PathBuf::from(shell)
    } else {
        command::find_on_path(shell).unwrap_or_else(|| PathBuf::from(shell))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_as_dev_avoids_full_enter_bootstrap() {
        let operations = planned_as_dev_operations();

        assert_eq!(operations.first(), Some(&AsDevOperation::RequireRoot));
        assert!(operations.contains(&AsDevOperation::ReadMaterializedDevAccount));
        assert!(operations.contains(&AsDevOperation::DropAndExec));
        assert_eq!(
            operations
                .iter()
                .position(|operation| operation == &AsDevOperation::AllowWorkloadKill),
            Some(operations.len() - 2),
            "the workload must become killable before it execs"
        );
        assert_eq!(operations.len(), 6);
    }

    #[test]
    fn materialized_dev_identity_comes_from_passwd_and_group() {
        let account = parse_materialized_dev_account(
            "root:x:0:0:root:/root:/bin/sh\ndev:x:1000:1001:dev user:/home/dev:/nix/store/fish/bin/fish\n",
            "root:x:0:\ndev:x:1001:\n",
        )
        .expect("materialized account should parse");

        assert_eq!(account.uid, 1000);
        assert_eq!(account.gid, 1001);
    }

    #[test]
    fn materialized_dev_identity_rejects_missing_entries() {
        let err =
            parse_materialized_dev_account("root:x:0:0:root:/root:/bin/sh\n", "dev:x:1001:\n")
                .expect_err("missing passwd dev entry should fail");
        assert!(format!("{err:#}").contains("missing from /etc/passwd"));

        let err = parse_materialized_dev_account(
            "dev:x:1000:1001:dev user:/home/dev:/bin/fish\n",
            "root:x:0:\n",
        )
        .expect_err("missing group dev entry should fail");
        assert!(format!("{err:#}").contains("missing from /etc/group"));
    }

    #[test]
    fn materialized_dev_identity_rejects_root_or_mismatched_identity() {
        let err = parse_materialized_dev_account(
            "dev:x:0:1001:dev user:/home/dev:/bin/fish\n",
            "dev:x:1001:\n",
        )
        .expect_err("root uid should fail");
        assert!(format!("{err:#}").contains("non-root dev user"));

        let err = parse_materialized_dev_account(
            "dev:x:1000:1001:dev user:/home/dev:/bin/fish\n",
            "dev:x:1002:\n",
        )
        .expect_err("gid mismatch should fail");
        assert!(format!("{err:#}").contains("gid mismatch"));
    }

    #[test]
    fn as_dev_environment_contract_does_not_enable_container_storage_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let passwd = dir.path().join("passwd");
        let group = dir.path().join("group");
        std::fs::write(&passwd, "dev:x:2000:2001:dev user:/home/dev:/bin/fish\n").expect("passwd");
        std::fs::write(&group, "dev:x:2001:\n").expect("group");

        let identity = resolve_materialized_dev_identity(&["fish".to_owned()], &passwd, &group)
            .expect("identity should resolve");
        let shell_env = crate::guest_init::components::shell::env::derive(&identity, false, None);

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
        assert!(!shell_env.vars.iter().any(|(key, _)| key == "DOCKER_HOST"));
        assert!(
            !shell_env
                .vars
                .iter()
                .any(|(key, _)| key == "XDG_RUNTIME_DIR")
        );
        assert!(!shell_env.vars.iter().any(|(key, _)| key == "PATH"));
    }

    #[test]
    fn as_dev_shell_comes_from_requested_command_not_host_id_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let passwd = dir.path().join("passwd");
        let group = dir.path().join("group");
        std::fs::write(
            &passwd,
            "dev:x:2000:2001:dev user:/home/dev:/ignored/shell\n",
        )
        .expect("passwd");
        std::fs::write(&group, "dev:x:2001:\n").expect("group");

        let identity = resolve_materialized_dev_identity(&["bash".to_owned()], &passwd, &group)
            .expect("identity should resolve");

        assert_eq!(identity.uid, 2000);
        assert_eq!(identity.gid, 2001);
        assert_eq!(identity.home, PathBuf::from(DEV_HOME));
        assert!(identity.shell.ends_with("bash"));
    }
}
