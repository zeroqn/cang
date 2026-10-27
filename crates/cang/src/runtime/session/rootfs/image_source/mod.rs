use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::runtime::host_tools::{RuntimeTool, runtime_tool_program};
use crate::runtime::launch::plan::ImageSelection;
use crate::runtime::session::profile::{
    DisabledRootfsMaterializationRecorder, RootfsMaterializationRecorder,
};
use crate::runtime::session::rootfs::task::{
    BtrfsRootfsCommands, UnsharedBtrfsRootfsCommands, snapshot_mounted_rootfs,
};
use crate::{DEFAULT_FALLBACK_IMAGE, DEFAULT_IMAGE};

mod commands;
#[cfg(test)]
pub(crate) use commands::ImageCacheCommandOutput;
pub(crate) use commands::{ImageCacheCommand, run_image_cache_command};

const BTRFS_IMAGE_CACHE_DIR: &str = "btrfs-snapshots";
const CACHE_ROOTFS_DIR: &str = "rootfs";
const CACHE_METADATA_FILE: &str = "metadata";
const GUEST_INIT_BASENAME: &str = "cang-guest-init";
const INTERNAL_BTRFS_ROOTFS_COMMAND: &str = "btrfs-rootfs";
const OCI_PROCESS_CONFIG_TEMPLATE: &str = r#"{{range $index, $value := .OCIv1.Config.Env}}{{printf "oci_env.%d=%x\n" $index $value}}{{end}}{{range $index, $value := .OCIv1.Config.Cmd}}{{printf "oci_cmd.%d=%x\n" $index $value}}{{end}}{{range $index, $value := .OCIv1.Config.Entrypoint}}{{printf "oci_entrypoint.%d=%x\n" $index $value}}{{end}}{{if .OCIv1.Config.WorkingDir}}{{printf "oci_workdir=%x\n" .OCIv1.Config.WorkingDir}}{{end}}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildahPullPolicy {
    Never,
    Missing,
    Always,
}

impl BuildahPullPolicy {
    fn as_buildah_value(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Missing => "missing",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageSourceAttempt {
    pub(crate) reference: String,
    pub(crate) pull_policy: BuildahPullPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageSourceRootfs {
    pub(crate) selected_reference: String,
    pub(crate) image_digest: Option<String>,
    pub(crate) image_local_digest: Option<String>,
    pub(crate) image_id: Option<String>,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) process_config: OciProcessConfig,
    pub(crate) cache_profile: ImageSourceCacheProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageSourceCacheProfile {
    pub(crate) status: ImageSourceCacheStatus,
    pub(crate) digest_key: Option<String>,
    pub(crate) cache_path: Option<PathBuf>,
    pub(crate) uncached_reason: Option<String>,
}

impl ImageSourceCacheProfile {
    fn hit(entry: &BtrfsImageCacheEntry) -> Self {
        Self {
            status: ImageSourceCacheStatus::Hit,
            digest_key: Some(entry.digest_key.clone()),
            cache_path: Some(entry.entry_dir.clone()),
            uncached_reason: None,
        }
    }

    fn populated(entry: &BtrfsImageCacheEntry, rebuilt: bool) -> Self {
        Self {
            status: if rebuilt {
                ImageSourceCacheStatus::MissRebuilt
            } else {
                ImageSourceCacheStatus::MissPopulated
            },
            digest_key: Some(entry.digest_key.clone()),
            cache_path: Some(entry.entry_dir.clone()),
            uncached_reason: None,
        }
    }

    pub(crate) fn direct_uncached(reason: &'static str) -> Self {
        Self {
            status: ImageSourceCacheStatus::DirectUncached,
            digest_key: None,
            cache_path: None,
            uncached_reason: Some(reason.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageSourceCacheStatus {
    Hit,
    MissPopulated,
    MissRebuilt,
    DirectUncached,
}

impl ImageSourceCacheStatus {
    pub(crate) fn as_profile_value(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::MissPopulated => "miss-populated",
            Self::MissRebuilt => "miss-rebuilt",
            Self::DirectUncached => "direct-uncached",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OciProcessConfig {
    pub(crate) env: Vec<String>,
    pub(crate) cmd: Vec<String>,
    pub(crate) entrypoint: Vec<String>,
    pub(crate) working_dir: Option<String>,
}

pub(crate) trait BuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String>;
    fn status(&self, args: &[&str]) -> Result<bool>;
    fn run_unshare_materializer(&self, args: &[&str]) -> Result<String>;
}

trait ChildBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String>;
}

#[derive(Debug, Clone, Copy)]
struct HostChildBuildahCommands;

impl ChildBuildahCommands for HostChildBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String> {
        run_buildah(args)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HostBuildahCommands;

impl BuildahCommands for HostBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String> {
        run_buildah(args)
    }

    fn status(&self, args: &[&str]) -> Result<bool> {
        run_buildah_status(args)
    }

    fn run_unshare_materializer(&self, args: &[&str]) -> Result<String> {
        let executable = std::env::current_exe()
            .context("failed to resolve current executable for buildah unshare materialization")?;
        let executable = executable
            .to_str()
            .ok_or_else(|| anyhow!("cang executable path is not valid UTF-8"))?;
        let mut command_args = vec![
            "unshare".to_owned(),
            executable.to_owned(),
            "internal".to_owned(),
            INTERNAL_BTRFS_ROOTFS_COMMAND.to_owned(),
        ];
        command_args.extend(args.iter().map(|arg| (*arg).to_owned()));
        let borrowed = command_args.iter().map(String::as_str).collect::<Vec<_>>();
        run_buildah(&borrowed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BtrfsImageCacheEntry {
    digest: String,
    digest_key: String,
    entry_dir: PathBuf,
    rootfs_path: PathBuf,
    metadata_path: PathBuf,
}

impl BtrfsImageCacheEntry {
    fn new(cache_root: &Path, digest: &str) -> Result<Self> {
        let digest_key = safe_digest_key(digest)?;
        let entry_dir = cache_root.join(BTRFS_IMAGE_CACHE_DIR).join(&digest_key);
        Ok(Self {
            digest: digest.to_owned(),
            digest_key,
            rootfs_path: entry_dir.join(CACHE_ROOTFS_DIR),
            metadata_path: entry_dir.join(CACHE_METADATA_FILE),
            entry_dir,
        })
    }

    fn is_complete(&self) -> bool {
        self.rootfs_path.is_dir() && self.metadata_path.is_file()
    }
}

pub(crate) fn materialize_btrfs_source_rootfs(
    selection: &ImageSelection,
    destination: &Path,
    image_cache_root: &Path,
    commands: &impl BuildahCommands,
    btrfs: &impl BtrfsRootfsCommands,
) -> Result<ImageSourceRootfs> {
    let mut profile = DisabledRootfsMaterializationRecorder;
    materialize_btrfs_source_rootfs_profiled(
        selection,
        destination,
        image_cache_root,
        commands,
        btrfs,
        &mut profile,
    )
}

pub(in crate::runtime::session) fn materialize_btrfs_source_rootfs_profiled(
    selection: &ImageSelection,
    destination: &Path,
    image_cache_root: &Path,
    commands: &impl BuildahCommands,
    btrfs: &impl BtrfsRootfsCommands,
    profile: &mut impl RootfsMaterializationRecorder,
) -> Result<ImageSourceRootfs> {
    profile
        .measure_result("task_rootfs_materialization:buildah_version", || {
            commands.run(&["--version"])
        })
        .context("failed to verify buildah; btrfs-snapshot task rootfs requires buildah")?;

    let attempt = profile
        .measure_result("task_rootfs_materialization:select_image_attempt", || {
            select_attempt(selection, commands)
        })?;
    let resolved_digest = profile
        .measure_result("task_rootfs_materialization:resolve_image_digest", || {
            resolve_attempt_digest(&attempt, commands)
        })?;
    let mut invalid_cached_entry = false;

    if let Some(digest) = resolved_digest.as_deref() {
        let (entry, cached_entry) =
            profile.measure_result("task_rootfs_materialization:cache_entry_read", || {
                let entry = BtrfsImageCacheEntry::new(image_cache_root, digest)?;
                let cached_entry = read_valid_cache_entry(&entry);
                Ok((entry, cached_entry))
            })?;
        match cached_entry {
            Ok(Some(cached)) => {
                profile
                    .measure_result("task_rootfs_materialization:cache_snapshot", || {
                        snapshot_mounted_rootfs(&entry.rootfs_path, destination, btrfs)
                    })
                    .with_context(|| {
                        format!(
                            "failed to btrfs-snapshot cached image rootfs '{}' to task rootfs '{}'",
                            entry.rootfs_path.display(),
                            destination.display()
                        )
                    })?;
                return Ok(ImageSourceRootfs {
                    selected_reference: attempt.reference,
                    image_digest: Some(entry.digest.clone()),
                    image_local_digest: cached.image_local_digest,
                    image_id: cached.image_id,
                    rootfs_path: destination.to_path_buf(),
                    process_config: cached.process_config,
                    cache_profile: ImageSourceCacheProfile::hit(&entry),
                });
            }
            Ok(None) => invalid_cached_entry = entry.entry_dir.exists(),
            Err(err) => {
                invalid_cached_entry = true;
                tracing::debug!(
                    digest,
                    cache_entry = %entry.entry_dir.display(),
                    error = %format!("{err:#}"),
                    "ignoring invalid cang btrfs image-source cache entry"
                );
            }
        }
    }

    let materialized = profile
        .measure_result("task_rootfs_materialization:buildah_materializer", || {
            materialize_task_rootfs_from_buildah(&attempt, destination, commands)
        })?;
    let Some(digest) = materialized.image_digest.as_deref() else {
        return Ok(ImageSourceRootfs {
            cache_profile: ImageSourceCacheProfile::direct_uncached("unknown-digest"),
            ..materialized
        });
    };

    let (entry, rebuilt) =
        profile.measure_result("task_rootfs_materialization:cache_population", || {
            let entry = BtrfsImageCacheEntry::new(image_cache_root, digest)?;
            let rebuilt = invalid_cached_entry || entry.entry_dir.exists();
            populate_cache_entry(&entry, &materialized, btrfs).with_context(|| {
                format!(
                    "failed to populate cang btrfs image-source cache entry '{}' from task rootfs '{}'",
                    entry.entry_dir.display(),
                    materialized.rootfs_path.display()
                )
            })?;
            Ok((entry, rebuilt))
        })?;

    Ok(ImageSourceRootfs {
        cache_profile: ImageSourceCacheProfile::populated(&entry, rebuilt),
        ..materialized
    })
}

fn materialize_task_rootfs_from_buildah(
    attempt: &ImageSourceAttempt,
    destination: &Path,
    commands: &impl BuildahCommands,
) -> Result<ImageSourceRootfs> {
    let destination = destination
        .to_str()
        .ok_or_else(|| anyhow!("cang task rootfs path is not valid UTF-8"))?;
    let output = commands
        .run_unshare_materializer(&[
            attempt.reference.as_str(),
            attempt.pull_policy.as_buildah_value(),
            destination,
        ])
        .with_context(|| {
            format!(
                "failed to create btrfs task rootfs from Buildah image source '{}' with pull policy '{}'",
                attempt.reference,
                attempt.pull_policy.as_buildah_value()
            )
        })?;
    let mut rootfs = parse_materializer_output(&output)?;
    rootfs.cache_profile = ImageSourceCacheProfile::direct_uncached("pending-cache-decision");
    Ok(rootfs)
}

fn resolve_attempt_digest(
    attempt: &ImageSourceAttempt,
    commands: &impl BuildahCommands,
) -> Result<Option<String>> {
    if let Some(digest) = digest_from_reference(&attempt.reference) {
        return Ok(Some(digest.to_owned()));
    }

    if attempt.pull_policy == BuildahPullPolicy::Always {
        commands
            .run(&["pull", attempt.reference.as_str()])
            .with_context(|| format!("failed to refresh Buildah image '{}'", attempt.reference))?;
    }

    inspect_image_digest(&attempt.reference, commands)
}

fn inspect_image_digest(
    reference: &str,
    commands: &impl BuildahCommands,
) -> Result<Option<String>> {
    for template in ["{{.Digest}}", "{{.FromImageDigest}}"] {
        match commands.run(&[
            "inspect", "--type", "image", "--format", template, reference,
        ]) {
            Ok(output) => {
                if let Some(digest) = optional_digest(output) {
                    return Ok(Some(digest));
                }
            }
            Err(err) => {
                tracing::debug!(
                    image = reference,
                    template,
                    error = %format!("{err:#}"),
                    "Buildah image digest inspect did not resolve a digest"
                );
            }
        }
    }
    Ok(None)
}

fn digest_from_reference(reference: &str) -> Option<&str> {
    reference
        .split_once('@')
        .map(|(_, digest)| digest)
        .filter(|digest| digest_is_known(digest))
}

fn read_valid_cache_entry(entry: &BtrfsImageCacheEntry) -> Result<Option<ImageSourceRootfs>> {
    if !entry.is_complete() {
        return Ok(None);
    }
    let metadata = fs::read_to_string(&entry.metadata_path).with_context(|| {
        format!(
            "failed to read cang btrfs image-source cache metadata '{}'",
            entry.metadata_path.display()
        )
    })?;
    let cached = parse_cache_metadata(&metadata, &entry.rootfs_path)?;
    if cached.image_digest.as_deref() != Some(entry.digest.as_str()) {
        bail!(
            "cache metadata digest mismatch: expected '{}', got '{:?}'",
            entry.digest,
            cached.image_digest
        );
    }
    find_cang_guest_init(&entry.rootfs_path).with_context(|| {
        format!(
            "cached image rootfs '{}' is not cang-compatible",
            entry.rootfs_path.display()
        )
    })?;
    Ok(Some(cached))
}

fn parse_cache_metadata(metadata: &str, rootfs_path: &Path) -> Result<ImageSourceRootfs> {
    let rootfs_path = rootfs_path
        .to_str()
        .ok_or_else(|| anyhow!("cang image-source cache rootfs path is not valid UTF-8"))?;
    parse_materializer_output(&format!("{metadata}rootfs_path={rootfs_path}\n"))
}

fn populate_cache_entry(
    entry: &BtrfsImageCacheEntry,
    source: &ImageSourceRootfs,
    btrfs: &impl BtrfsRootfsCommands,
) -> Result<()> {
    reset_cache_entry(entry, btrfs)?;
    fs::create_dir_all(&entry.entry_dir).with_context(|| {
        format!(
            "failed to create cang btrfs image-source cache entry '{}'",
            entry.entry_dir.display()
        )
    })?;

    let result = (|| {
        snapshot_mounted_rootfs(&source.rootfs_path, &entry.rootfs_path, btrfs)?;
        fs::write(&entry.metadata_path, format_cache_metadata(source)).with_context(|| {
            format!(
                "failed to write cang btrfs image-source cache metadata '{}'",
                entry.metadata_path.display()
            )
        })
    })();

    if let Err(err) = result {
        reset_cache_entry(entry, btrfs).with_context(|| {
            format!(
                "failed to clean incomplete cang btrfs image-source cache entry '{}' after error: {err:#}",
                entry.entry_dir.display()
            )
        })?;
        return Err(err);
    }
    Ok(())
}

fn reset_cache_entry(entry: &BtrfsImageCacheEntry, btrfs: &impl BtrfsRootfsCommands) -> Result<()> {
    if entry.rootfs_path.exists() {
        btrfs
            .delete_btrfs_subvolume(&entry.rootfs_path)
            .with_context(|| {
                format!(
                    "failed to delete stale cang btrfs image-source cache rootfs '{}'",
                    entry.rootfs_path.display()
                )
            })?;
    }
    if entry.entry_dir.exists() {
        remove_cache_entry_tree(&entry.entry_dir).with_context(|| {
            format!(
                "failed to remove stale cang btrfs image-source cache entry '{}'",
                entry.entry_dir.display()
            )
        })?;
    }
    Ok(())
}

fn remove_cache_entry_tree(path: &Path) -> Result<()> {
    make_directories_owner_writable(path)?;
    fs::remove_dir_all(path)?;
    Ok(())
}

fn make_directories_owner_writable(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat '{}'", path.display()))?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let mode = metadata.permissions().mode();
    if mode & 0o200 == 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700)).with_context(|| {
            format!(
                "failed to make cang cache directory writable '{}'",
                path.display()
            )
        })?;
    }

    for entry in
        fs::read_dir(path).with_context(|| format!("failed to read '{}'", path.display()))?
    {
        make_directories_owner_writable(&entry?.path())?;
    }
    Ok(())
}

fn format_cache_metadata(rootfs: &ImageSourceRootfs) -> String {
    let mut output = String::new();
    output.push_str(&format!("selected_image={}\n", rootfs.selected_reference));
    if let Some(digest) = &rootfs.image_digest {
        output.push_str(&format!("image_digest={digest}\n"));
    }
    if let Some(ref digest) = rootfs.image_local_digest {
        output.push_str(&format!("image_local_digest={digest}\n"));
    }
    if let Some(ref id) = rootfs.image_id {
        output.push_str(&format!("image_id={id}\n"));
    }
    output.push_str(&format_oci_process_config(&rootfs.process_config));
    output
}

fn safe_digest_key(digest: &str) -> Result<String> {
    let (algorithm, value) = digest
        .split_once(':')
        .ok_or_else(|| anyhow!("image digest '{digest}' must use algorithm:value form"))?;
    if algorithm.is_empty() || value.is_empty() || !digest_is_known(digest) {
        bail!("image digest '{digest}' must include non-empty algorithm and value");
    }
    for component in [algorithm, value] {
        if component == "."
            || component == ".."
            || component.contains('/')
            || component.contains('\\')
        {
            bail!("image digest '{digest}' is not safe for cache paths");
        }
        if !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            bail!("image digest '{digest}' is not safe for cache paths");
        }
    }
    Ok(format!("{algorithm}-{value}"))
}

fn select_attempt(
    selection: &ImageSelection,
    commands: &impl BuildahCommands,
) -> Result<ImageSourceAttempt> {
    match selection {
        ImageSelection::PreferLocalhostThenCanonical => {
            if commands
                .status(&["inspect", "--type", "image", DEFAULT_IMAGE])
                .with_context(|| format!("failed to inspect local cang image '{DEFAULT_IMAGE}'"))?
            {
                Ok(ImageSourceAttempt {
                    reference: DEFAULT_IMAGE.to_owned(),
                    pull_policy: BuildahPullPolicy::Never,
                })
            } else {
                Ok(ImageSourceAttempt {
                    reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
                    pull_policy: BuildahPullPolicy::Missing,
                })
            }
        }
        ImageSelection::CanonicalWithRefresh => Ok(ImageSourceAttempt {
            reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
            pull_policy: BuildahPullPolicy::Always,
        }),
        ImageSelection::Explicit { reference } => Ok(ImageSourceAttempt {
            reference: reference.clone(),
            pull_policy: BuildahPullPolicy::Missing,
        }),
    }
}

fn parse_materializer_output(output: &str) -> Result<ImageSourceRootfs> {
    let mut selected_reference = None;
    let mut image_digest = None;
    let mut image_local_digest = None;
    let mut image_id = None;
    let mut rootfs_path = None;
    let mut env = BTreeMap::new();
    let mut cmd = BTreeMap::new();
    let mut entrypoint = BTreeMap::new();
    let mut working_dir = None;

    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(index) = key.strip_prefix("oci_env.") {
            env.insert(parse_oci_index(key, index)?, decode_hex_field(key, value)?);
        } else if let Some(index) = key.strip_prefix("oci_cmd.") {
            cmd.insert(parse_oci_index(key, index)?, decode_hex_field(key, value)?);
        } else if let Some(index) = key.strip_prefix("oci_entrypoint.") {
            entrypoint.insert(parse_oci_index(key, index)?, decode_hex_field(key, value)?);
        } else {
            match key {
                "selected_image" if !value.is_empty() => {
                    selected_reference = Some(value.to_owned());
                }
                "image_digest" if digest_is_known(value) => image_digest = Some(value.to_owned()),
                "image_local_digest" if digest_is_known(value) => {
                    image_local_digest = Some(value.to_owned());
                }
                "image_id" if !value.is_empty() => image_id = Some(value.to_owned()),
                "rootfs_path" if !value.is_empty() => rootfs_path = Some(PathBuf::from(value)),
                "oci_workdir" => {
                    if working_dir.is_some() {
                        bail!("Buildah materializer repeated {key}");
                    }
                    working_dir = Some(decode_hex_field(key, value)?);
                }
                _ => {}
            }
        }
    }

    Ok(ImageSourceRootfs {
        selected_reference: selected_reference
            .ok_or_else(|| anyhow!("Buildah materializer did not report selected image"))?,
        image_digest,
        image_local_digest,
        image_id,
        rootfs_path: rootfs_path
            .ok_or_else(|| anyhow!("Buildah materializer did not report task rootfs path"))?,
        process_config: OciProcessConfig {
            env: env.into_values().collect(),
            cmd: cmd.into_values().collect(),
            entrypoint: entrypoint.into_values().collect(),
            working_dir,
        },
        cache_profile: ImageSourceCacheProfile::direct_uncached("unclassified"),
    })
}

fn parse_oci_index(key: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("Buildah materializer field {key} has invalid index"))
}

fn decode_hex_field(key: &str, encoded: &str) -> Result<String> {
    if !encoded.len().is_multiple_of(2) {
        bail!("Buildah materializer field {key} has odd-length hex");
    }
    let bytes = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(text, 16)?)
        })
        .collect::<std::result::Result<Vec<_>, anyhow::Error>>()?;
    String::from_utf8(bytes)
        .with_context(|| format!("Buildah materializer field {key} is not UTF-8"))
}

fn digest_is_known(value: &str) -> bool {
    !value.is_empty() && value != "<no value>"
}

pub(crate) fn run_internal(args: Vec<OsString>) -> Result<()> {
    let mut args = args.into_iter();
    let command = args
        .next()
        .and_then(|arg| arg.into_string().ok())
        .ok_or_else(|| anyhow!("missing internal command"))?;

    match command.as_str() {
        INTERNAL_BTRFS_ROOTFS_COMMAND => {
            let image_ref = next_utf8_arg(&mut args, "image reference")?;
            let pull_policy = next_utf8_arg(&mut args, "pull policy")?;
            let destination = next_path_arg(&mut args, "destination rootfs")?;
            ensure_no_extra_args(args)?;
            run_btrfs_rootfs_child(&image_ref, &pull_policy, &destination)
        }
        _ => bail!("unknown internal command '{command}'"),
    }
}

fn next_utf8_arg(args: &mut impl Iterator<Item = OsString>, label: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow!("missing internal {label} argument"))?
        .into_string()
        .map_err(|_| anyhow!("internal {label} argument is not valid UTF-8"))
}

fn next_path_arg(args: &mut impl Iterator<Item = OsString>, label: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(next_utf8_arg(args, label)?))
}

fn ensure_no_extra_args(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    if args.next().is_some() {
        bail!("unexpected extra internal arguments");
    }
    Ok(())
}

fn run_btrfs_rootfs_child(image_ref: &str, pull_policy: &str, destination: &Path) -> Result<()> {
    run_btrfs_rootfs_child_with_commands(
        image_ref,
        pull_policy,
        destination,
        &HostChildBuildahCommands,
        &UnsharedBtrfsRootfsCommands,
    )
}

fn run_btrfs_rootfs_child_with_commands(
    image_ref: &str,
    pull_policy: &str,
    destination: &Path,
    buildah: &impl ChildBuildahCommands,
    btrfs: &impl BtrfsRootfsCommands,
) -> Result<()> {
    let container_id = trim_required(
        buildah.run(&["from", &format!("--pull={pull_policy}"), image_ref])?,
        "buildah from did not return a container id",
    )?;
    let mut container = BuildahContainerGuard::new(buildah, container_id);
    let image_local_digest = match buildah.run(&[
        "inspect",
        "--type",
        "image",
        "--format",
        "{{.Digest}}",
        image_ref,
    ]) {
        Ok(output) => optional_digest(output),
        Err(err) => {
            tracing::debug!(
                image = image_ref,
                error = %format!("{err:#}"),
                "Buildah image digest inspect did not resolve a local digest"
            );
            None
        }
    };
    let image_digest = optional_digest(buildah.run(&[
        "inspect",
        "--format",
        "{{.FromImageDigest}}",
        container.id(),
    ])?);
    let image_id = optional_digest(buildah.run(&[
        "inspect",
        "--format",
        "{{.FromImageID}}",
        container.id(),
    ])?);
    let process_config = inspect_oci_process_config(buildah, container.id())?;
    let mounted_rootfs = PathBuf::from(trim_required(
        buildah.run(&["mount", container.id()])?,
        "buildah mount did not return a mounted rootfs path",
    )?);
    container.mark_mounted();

    find_cang_guest_init(&mounted_rootfs).with_context(|| {
        format!(
            "Buildah-mounted image rootfs '{}' is not cang-compatible",
            mounted_rootfs.display()
        )
    })?;
    snapshot_mounted_rootfs(&mounted_rootfs, destination, btrfs).with_context(|| {
        format!(
            "failed to btrfs-snapshot Buildah-mounted rootfs '{}' to '{}'",
            mounted_rootfs.display(),
            destination.display()
        )
    })?;
    fix_rootfs_ownership(destination).with_context(|| {
        format!(
            "failed to fix rootfs ownership for '{}'",
            destination.display()
        )
    })?;
    fix_nix_store_ownership(destination).with_context(|| {
        format!(
            "failed to fix nix store ownership for '{}'",
            destination.display()
        )
    })?;

    println!("selected_image={image_ref}");
    if let Some(digest) = image_digest {
        println!("image_digest={digest}");
    }
    if let Some(ref digest) = image_local_digest {
        println!("image_local_digest={digest}");
    }
    if let Some(ref id) = image_id {
        println!("image_id={id}");
    }
    println!("rootfs_path={}", destination.display());
    print_oci_process_config(&process_config);
    Ok(())
}

fn fix_rootfs_ownership(rootfs: &Path) -> Result<()> {
    if !rootfs.exists() {
        return Ok(());
    }
    fix_ownership_recursive(rootfs)
}

fn fix_nix_store_ownership(rootfs: &Path) -> Result<()> {
    let store = rootfs.join("nix/store");
    if !store.exists() {
        return Ok(());
    }
    // SAFETY: fchownat with AT_SYMLINK_NOFOLLOW; we own the rootfs snapshot.
    // In production this runs inside the keep-id namespace as uid 0 with
    // CAP_CHOWN where gid 30000 is unconditionally mapped; the chown always
    // succeeds. Gracefully skip when the caller lacks the capability or the
    // gid is not mapped (e.g. test harnesses).
    let rc = unsafe {
        libc::fchownat(
            libc::AT_FDCWD,
            path_cstr(&store)?.as_ptr(),
            u32::MAX,
            30000,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error();
        // EPERM: caller lacks CAP_CHOWN outside the keep-id namespace.
        // EINVAL: gid 30000 is not mapped in the caller's user namespace.
        // Both are expected when running outside the keep-id namespace
        // (e.g. test harnesses); in production the chown always succeeds.
        if code == Some(libc::EPERM) || code == Some(libc::EINVAL) {
            tracing::debug!("skipping nix store chown for '{}': {err}", store.display());
        } else {
            return Err(err).with_context(|| {
                format!(
                    "failed to chown '{}' to :nixbld (gid 30000)",
                    store.display()
                )
            });
        }
    }
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o1775))
        .with_context(|| format!("failed to chmod '{}' to 1775", store.display()))?;
    Ok(())
}

fn fix_ownership_recursive(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat '{}'", path.display()))?;
    let uid = metadata.uid();
    let gid = metadata.gid();
    if uid == 0 || gid == 0 {
        let new_uid = if uid == 0 { 1 } else { uid };
        let new_gid = if gid == 0 { 1 } else { gid };
        // SAFETY: fchownat with AT_SYMLINK_NOFOLLOW; we own the path.
        let rc = unsafe {
            libc::fchownat(
                libc::AT_FDCWD,
                path_cstr(path)?.as_ptr(),
                new_uid,
                new_gid,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to chown '{}'", path.display()));
        }
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("failed to read dir '{}'", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in '{}'", path.display()))?;
            fix_ownership_recursive(&entry.path())?;
        }
    }
    Ok(())
}

fn path_cstr(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).context("path contains interior NUL")
}

fn inspect_oci_process_config(
    buildah: &impl ChildBuildahCommands,
    container_id: &str,
) -> Result<OciProcessConfig> {
    let output = buildah.run(&[
        "inspect",
        "--format",
        OCI_PROCESS_CONFIG_TEMPLATE,
        container_id,
    ])?;
    parse_oci_process_config_output(&output)
}

fn parse_oci_process_config_output(output: &str) -> Result<OciProcessConfig> {
    parse_materializer_output(&format!(
        "selected_image=placeholder\nrootfs_path=/placeholder\n{output}"
    ))
    .map(|rootfs| rootfs.process_config)
}

fn print_oci_process_config(config: &OciProcessConfig) {
    print!("{}", format_oci_process_config(config));
}

fn format_oci_process_config(config: &OciProcessConfig) -> String {
    let mut output = String::new();
    for (index, value) in config.env.iter().enumerate() {
        output.push_str(&format!("oci_env.{index}={}\n", encode_hex(value)));
    }
    for (index, value) in config.cmd.iter().enumerate() {
        output.push_str(&format!("oci_cmd.{index}={}\n", encode_hex(value)));
    }
    for (index, value) in config.entrypoint.iter().enumerate() {
        output.push_str(&format!("oci_entrypoint.{index}={}\n", encode_hex(value)));
    }
    if let Some(working_dir) = &config.working_dir {
        output.push_str(&format!("oci_workdir={}\n", encode_hex(working_dir)));
    }
    output
}

fn encode_hex(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn run_buildah(args: &[&str]) -> Result<String> {
    let output = Command::new(runtime_tool_program(RuntimeTool::Buildah))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => anyhow!(
                "buildah is not installed or not available on PATH; cang btrfs-snapshot task rootfs requires buildah"
            ),
            _ => err.into(),
        })
        .with_context(|| format!("failed to run buildah {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.is_empty() {
            bail!("buildah {} failed", args.join(" "));
        }
        bail!("buildah {} failed: {stderr}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_buildah_status(args: &[&str]) -> Result<bool> {
    let status = match Command::new(runtime_tool_program(RuntimeTool::Buildah))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(anyhow!(err))
                .with_context(|| format!("failed to run buildah {}", args.join(" ")));
        }
    };
    Ok(status.success())
}

fn trim_required(output: String, empty_message: &'static str) -> Result<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        bail!(empty_message);
    }
    Ok(trimmed.to_owned())
}

fn optional_digest(output: String) -> Option<String> {
    let digest = output.trim();
    digest_is_known(digest).then(|| digest.to_owned())
}

struct BuildahContainerGuard<'a, C: ChildBuildahCommands> {
    commands: &'a C,
    id: String,
    mounted: bool,
}

impl<'a, C: ChildBuildahCommands> BuildahContainerGuard<'a, C> {
    fn new(commands: &'a C, id: String) -> Self {
        Self {
            commands,
            id,
            mounted: false,
        }
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn mark_mounted(&mut self) {
        self.mounted = true;
    }
}

impl<C: ChildBuildahCommands> Drop for BuildahContainerGuard<'_, C> {
    fn drop(&mut self) {
        if self.mounted {
            let _ = self.commands.run(&["umount", &self.id]);
        }
        let _ = self.commands.run(&["rm", &self.id]);
    }
}

pub(crate) fn find_cang_guest_init(rootfs: &Path) -> Result<PathBuf> {
    let store = rootfs.join("nix/store");
    let mut matches = Vec::new();
    if store.is_dir() {
        for entry in fs::read_dir(&store).with_context(|| {
            format!(
                "failed to read cang image rootfs store '{}'",
                store.display()
            )
        })? {
            let entry = entry?;
            let candidate = entry.path().join("bin").join(GUEST_INIT_BASENAME);
            if is_executable_file(&candidate) {
                matches.push(candidate);
            }
        }
    }

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!(
            "cang image is not compatible: no executable {GUEST_INIT_BASENAME} found under {}/nix/store/*/bin",
            rootfs.display()
        ),
        count => bail!(
            "cang image is ambiguous: found {count} executable {GUEST_INIT_BASENAME} binaries under {}/nix/store/*/bin",
            rootfs.display()
        ),
    }
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
