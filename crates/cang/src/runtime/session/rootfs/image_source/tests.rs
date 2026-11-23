use super::*;
use crate::runtime::session::profile::RootfsMaterializationRecorder;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug)]
struct FakeBuildahCommands {
    local_image_exists: bool,
    image_digest: Option<String>,
    reference_digests: HashMap<String, String>,
    output: String,
    inventory_output: String,
    fail_unshare: bool,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeBuildahCommands {
    fn new(local_image_exists: bool, rootfs_path: &Path) -> Self {
        Self {
            local_image_exists,
            image_digest: None,
            reference_digests: HashMap::new(),
            output: format!(
                "selected_image={}\nimage_digest=sha256:feedface\nrootfs_path={}\n",
                DEFAULT_FALLBACK_IMAGE,
                rootfs_path.display()
            ),
            inventory_output: String::new(),
            fail_unshare: false,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn with_image_digest(mut self, digest: &str) -> Self {
        self.image_digest = Some(digest.to_owned());
        self
    }

    fn with_digest_for(mut self, reference: &str, digest: &str) -> Self {
        self.reference_digests
            .insert(reference.to_owned(), digest.to_owned());
        self
    }
    fn with_output(mut self, output: String) -> Self {
        self.output = output;
        self
    }

    fn with_inventory(mut self, output: &str) -> Self {
        self.inventory_output = output.to_owned();
        self
    }

    fn fail_on_unshare(mut self) -> Self {
        self.fail_unshare = true;
        self
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl BuildahCommands for FakeBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        match args {
            ["--version"] => Ok("buildah version 1.42.0\n".to_owned()),
            [
                "images",
                "--all",
                "--digests",
                "--noheading",
                "--format",
                _format,
            ] => Ok(self.inventory_output.clone()),
            ["pull", _reference] => Ok("pulled\n".to_owned()),
            [
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                reference,
            ]
            | [
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.FromImageDigest}}",
                reference,
            ] => {
                let digest = self
                    .reference_digests
                    .get(*reference)
                    .or(self.image_digest.as_ref());
                digest
                    .map(|d| format!("{d}\n"))
                    .ok_or_else(|| anyhow!("image digest unavailable"))
            }
            ["rmi", _reference] => Ok("removed\n".to_owned()),
            other => bail!("unexpected buildah args: {other:?}"),
        }
    }

    fn status(&self, args: &[&str]) -> Result<bool> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        match args {
            ["inspect", "--type", "image", DEFAULT_IMAGE] => Ok(self.local_image_exists),
            other => bail!("unexpected buildah status args: {other:?}"),
        }
    }

    fn run_unshare_materializer(&self, args: &[&str]) -> Result<String> {
        self.calls.borrow_mut().push(
            std::iter::once("unshare-materializer".to_owned())
                .chain(args.iter().map(|arg| (*arg).to_owned()))
                .collect(),
        );
        if self.fail_unshare {
            bail!("unshare materializer must not run");
        }
        fs::create_dir_all(args[2]).expect("fake task rootfs should be created");
        Ok(self.output.clone())
    }
}

fn write_guest_init(root: &Path, store_name: &str, mode: u32) -> PathBuf {
    let path = root
        .join("nix/store")
        .join(store_name)
        .join("bin")
        .join(GUEST_INIT_BASENAME);
    fs::create_dir_all(path.parent().unwrap()).expect("parent should be created");
    fs::write(&path, "#!/bin/sh\n").expect("guest init should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode should be set");
    path
}

#[derive(Debug)]
struct FakeChildBuildahCommands {
    mount_root: PathBuf,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeChildBuildahCommands {
    fn new(mount_root: PathBuf) -> Self {
        Self {
            mount_root,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

impl ChildBuildahCommands for FakeChildBuildahCommands {
    fn run(&self, args: &[&str]) -> Result<String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        match args {
            ["from", "--pull=missing", "ghcr.io/example/cang:dev"] => {
                Ok("fake-container\n".to_owned())
            }
            [
                "inspect",
                "--format",
                "{{.FromImageDigest}}",
                "fake-container",
            ] => Ok("sha256:feedface\n".to_owned()),
            [
                "inspect",
                "--format",
                "{{.FromImageID}}",
                "fake-container",
            ] => Ok("deadbeefcafe\n".to_owned()),
            [
                "inspect",
                "--format",
                OCI_PROCESS_CONFIG_TEMPLATE,
                "fake-container",
            ] => Ok(
                concat!(
                    "oci_env.0=504154483d2f6e69782f73746f72652f666973682f62696e\n",
                    "oci_cmd.0=66697368\n",
                    "oci_cmd.1=2d6c\n",
                    "oci_entrypoint.0=2f6e69782f73746f72652f686173682d63616e672f62696e2f63616e672d67756573742d696e6974\n",
                    "oci_entrypoint.1=656e746572\n",
                    "oci_entrypoint.2=2d2d\n",
                    "oci_workdir=2f776f726b73706163652f70726f6a656374\n"
                )
                .to_owned(),
            ),
            [
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                "ghcr.io/example/cang:dev",
            ] => Ok("sha256:feedface\n".to_owned()),
            ["mount", "fake-container"] => Ok(format!("{}\n", self.mount_root.display())),
            ["umount", "fake-container"] => Ok(String::new()),
            ["rm", "fake-container"] => Ok(String::new()),
            other => bail!("unexpected child buildah args: {other:?}"),
        }
    }
}

#[derive(Debug)]
struct FakeBtrfsRootfsCommands {
    calls: RefCell<Vec<BtrfsCall>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BtrfsCall {
    Snapshot {
        source: PathBuf,
        destination: PathBuf,
    },
    Delete(PathBuf),
}

impl FakeBtrfsRootfsCommands {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<BtrfsCall> {
        self.calls.borrow().clone()
    }
}

impl BtrfsRootfsCommands for FakeBtrfsRootfsCommands {
    fn snapshot_btrfs_subvolume(&self, source: &Path, destination: &Path) -> Result<()> {
        self.calls.borrow_mut().push(BtrfsCall::Snapshot {
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
        });
        if source.exists() {
            copy_rootfs_tree(source, destination)
        } else {
            fs::create_dir_all(destination).expect("destination should be created");
            Ok(())
        }
    }

    fn delete_btrfs_subvolume(&self, subvolume: &Path) -> Result<()> {
        self.calls
            .borrow_mut()
            .push(BtrfsCall::Delete(subvolume.to_path_buf()));
        if subvolume.exists() {
            fs::remove_dir_all(subvolume).with_context(|| {
                format!("failed to remove fake subvolume '{}'", subvolume.display())
            })?;
        }
        Ok(())
    }
}

fn copy_rootfs_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to stat '{}'", source.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("failed to create '{}'", destination.display()))?;
        for entry in fs::read_dir(source)
            .with_context(|| format!("failed to read '{}'", source.display()))?
        {
            let entry = entry?;
            copy_rootfs_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode()),
        )?;
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode()),
        )?;
    }
    Ok(())
}

fn call_materialize(
    selection: &ImageSelection,
    task_rootfs: &Path,
    cache_root: &Path,
    commands: &FakeBuildahCommands,
    btrfs: &FakeBtrfsRootfsCommands,
) -> Result<ImageSourceRootfs> {
    materialize_btrfs_source_rootfs(selection, task_rootfs, cache_root, commands, btrfs)
}

#[derive(Debug, Default)]
struct RecordingRootfsProfile {
    labels: Vec<&'static str>,
}

impl RecordingRootfsProfile {
    fn labels(&self) -> Vec<&'static str> {
        self.labels.clone()
    }
}

impl RootfsMaterializationRecorder for RecordingRootfsProfile {
    fn measure_result<T, F>(&mut self, label: &'static str, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        self.labels.push(label);
        f()
    }
}

#[test]
fn default_selection_uses_localhost_with_pull_never_when_present() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("rootfs");
    let output = format!(
        "selected_image={DEFAULT_IMAGE}\nimage_digest=sha256:local\nrootfs_path={}\n",
        task_rootfs.display()
    );
    let commands = FakeBuildahCommands::new(true, &task_rootfs)
        .with_image_digest("sha256:local")
        .with_output(output);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let rootfs = call_materialize(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
    )
    .expect("local image should materialize");

    assert_eq!(rootfs.selected_reference, DEFAULT_IMAGE);
    assert_eq!(rootfs.image_digest.as_deref(), Some("sha256:local"));
    assert_eq!(
        rootfs.cache_profile.status,
        ImageSourceCacheStatus::MissPopulated
    );
    assert_eq!(
        rootfs.cache_profile.digest_key.as_deref(),
        Some("sha256-local")
    );
    assert_eq!(
        commands.calls(),
        vec![
            vec!["--version"],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.FromImageDigest}}",
                DEFAULT_IMAGE
            ],
            vec![
                "unshare-materializer",
                DEFAULT_IMAGE,
                "never",
                task_rootfs.to_str().unwrap()
            ],
        ]
    );
}

#[test]
fn profile_records_miss_subphases_without_child_internals() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("rootfs");
    let output = format!(
        "selected_image={DEFAULT_IMAGE}\nimage_digest=sha256:local\nrootfs_path={}\n",
        task_rootfs.display()
    );
    let commands = FakeBuildahCommands::new(true, &task_rootfs)
        .with_image_digest("sha256:local")
        .with_output(output);
    let btrfs = FakeBtrfsRootfsCommands::new();
    let mut profile = RecordingRootfsProfile::default();

    materialize_btrfs_source_rootfs_profiled(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
        &mut profile,
    )
    .expect("local image should materialize");

    assert_eq!(
        profile.labels(),
        vec![
            "task_rootfs_materialization:buildah_version",
            "task_rootfs_materialization:image_probe",
            "task_rootfs_materialization:cache_entry_read",
            "task_rootfs_materialization:buildah_materializer",
            "task_rootfs_materialization:cache_population",
        ]
    );
}

#[test]
fn default_selection_falls_back_to_canonical_with_pull_missing() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("rootfs");
    let commands = FakeBuildahCommands::new(false, &task_rootfs);
    let btrfs = FakeBtrfsRootfsCommands::new();

    call_materialize(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
    )
    .expect("canonical fallback should materialize");

    assert_eq!(
        commands.calls(),
        vec![
            vec!["--version"],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.FromImageDigest}}",
                DEFAULT_IMAGE
            ],
            vec!["inspect", "--type", "image", DEFAULT_IMAGE],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                DEFAULT_FALLBACK_IMAGE
            ],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.FromImageDigest}}",
                DEFAULT_FALLBACK_IMAGE
            ],
            vec![
                "unshare-materializer",
                DEFAULT_FALLBACK_IMAGE,
                "missing",
                task_rootfs.to_str().unwrap()
            ],
        ]
    );
}

#[test]
fn pull_latest_refreshes_image_before_cache_lookup() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("rootfs");
    let commands =
        FakeBuildahCommands::new(false, &task_rootfs).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    call_materialize(
        &ImageSelection::CanonicalWithRefresh,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
    )
    .expect("canonical refresh should materialize");

    assert_eq!(
        commands.calls(),
        vec![
            vec!["--version"],
            vec!["pull", DEFAULT_FALLBACK_IMAGE],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                DEFAULT_FALLBACK_IMAGE
            ],
            vec![
                "unshare-materializer",
                DEFAULT_FALLBACK_IMAGE,
                "always",
                task_rootfs.to_str().unwrap()
            ],
        ]
    );
}

#[test]
fn explicit_digest_image_uses_digest_key_without_image_inspect() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("rootfs");
    let commands = FakeBuildahCommands::new(false, &task_rootfs);
    let btrfs = FakeBtrfsRootfsCommands::new();
    let selection = ImageSelection::Explicit {
        reference: "ghcr.io/example/cang@sha256:abc123".to_owned(),
    };

    call_materialize(
        &selection,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
    )
    .expect("explicit image should materialize");

    assert_eq!(
        commands.calls(),
        vec![
            vec!["--version"],
            vec![
                "unshare-materializer",
                "ghcr.io/example/cang@sha256:abc123",
                "missing",
                task_rootfs.to_str().unwrap()
            ],
        ]
    );
}

#[test]
fn safe_digest_key_rejects_path_unsafe_digest_values() {
    assert_eq!(
        safe_digest_key("sha256:feedface").unwrap(),
        "sha256-feedface"
    );
    assert!(safe_digest_key("sha256:").is_err());
    assert!(safe_digest_key("sha/256:feedface").is_err());
    assert!(safe_digest_key("sha256:../feedface").is_err());
    assert!(safe_digest_key("<no value>").is_err());
}

#[test]
fn cache_metadata_round_trips_process_config() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let source = ImageSourceRootfs {
        selected_reference: DEFAULT_IMAGE.to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: temp.path().join("task-rootfs"),
        process_config: OciProcessConfig {
            env: vec!["PATH=/nix/store/fish/bin".to_owned()],
            cmd: vec!["fish".to_owned(), "-l".to_owned()],
            entrypoint: vec!["/nix/store/hash-cang/bin/cang-guest-init".to_owned()],
            working_dir: Some("/workspace".to_owned()),
        },
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };

    let metadata = format_cache_metadata(&source);
    let parsed = parse_cache_metadata(&metadata, &temp.path().join("cache-rootfs"))
        .expect("cache metadata should parse");

    assert_eq!(parsed.selected_reference, source.selected_reference);
    assert_eq!(parsed.image_digest, source.image_digest);
    assert_eq!(parsed.process_config, source.process_config);
    assert_eq!(parsed.rootfs_path, temp.path().join("cache-rootfs"));
}

#[test]
fn cache_hit_reuses_source_without_unshare_materializer() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    write_guest_init(&entry.rootfs_path, "hash-cang", 0o755);
    let source = ImageSourceRootfs {
        selected_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig {
            env: vec!["PATH=/nix/store/fish/bin".to_owned()],
            ..OciProcessConfig::default()
        },
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let task_rootfs = temp.path().join("task-rootfs");
    let commands = FakeBuildahCommands::new(false, &task_rootfs)
        .with_image_digest("sha256:feedface")
        .fail_on_unshare();
    let btrfs = FakeBtrfsRootfsCommands::new();

    let rootfs = call_materialize(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("cache hit should materialize task rootfs");

    assert_eq!(rootfs.cache_profile.status, ImageSourceCacheStatus::Hit);
    assert_eq!(rootfs.rootfs_path, task_rootfs);
    assert_eq!(
        rootfs.process_config.env,
        vec!["PATH=/nix/store/fish/bin".to_owned()]
    );
    assert_eq!(
        btrfs.calls(),
        vec![BtrfsCall::Snapshot {
            source: entry.rootfs_path,
            destination: rootfs.rootfs_path,
        }]
    );
    assert!(commands.calls().iter().all(|call| {
        !matches!(
            call.first().map(String::as_str),
            Some("unshare-materializer")
        ) && !call
            .iter()
            .any(|arg| matches!(arg.as_str(), "from" | "mount" | "umount" | "rm"))
    }));
}

#[test]
fn profile_records_cache_hit_snapshot_without_materializer_or_population() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    write_guest_init(&entry.rootfs_path, "hash-cang", 0o755);
    let source = ImageSourceRootfs {
        selected_reference: DEFAULT_FALLBACK_IMAGE.to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig {
            env: vec!["PATH=/nix/store/fish/bin".to_owned()],
            ..OciProcessConfig::default()
        },
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let task_rootfs = temp.path().join("task-rootfs");
    let commands = FakeBuildahCommands::new(false, &task_rootfs)
        .with_image_digest("sha256:feedface")
        .fail_on_unshare();
    let btrfs = FakeBtrfsRootfsCommands::new();
    let mut profile = RecordingRootfsProfile::default();

    materialize_btrfs_source_rootfs_profiled(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &cache_root,
        &commands,
        &btrfs,
        &mut profile,
    )
    .expect("cache hit should materialize task rootfs");

    assert_eq!(
        profile.labels(),
        vec![
            "task_rootfs_materialization:buildah_version",
            "task_rootfs_materialization:image_probe",
            "task_rootfs_materialization:cache_entry_read",
            "task_rootfs_materialization:cache_snapshot",
        ]
    );
}

#[test]
fn unknown_digest_uses_direct_uncached_path_without_cache_write() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let task_rootfs = temp.path().join("task-rootfs");
    let output = format!(
        "selected_image={DEFAULT_FALLBACK_IMAGE}\nimage_digest=<no value>\nrootfs_path={}\n",
        task_rootfs.display()
    );
    let commands = FakeBuildahCommands::new(false, &task_rootfs).with_output(output);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let rootfs = call_materialize(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &temp.path().join("cache"),
        &commands,
        &btrfs,
    )
    .expect("unknown digest should still materialize");

    assert_eq!(rootfs.image_digest, None);
    assert_eq!(
        rootfs.cache_profile.status,
        ImageSourceCacheStatus::DirectUncached
    );
    assert_eq!(
        rootfs.cache_profile.uncached_reason.as_deref(),
        Some("unknown-digest")
    );
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !temp
            .path()
            .join("cache")
            .join(BTRFS_IMAGE_CACHE_DIR)
            .exists()
    );
}

#[test]
fn corrupt_cache_entry_rebuilds_before_writing_metadata() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("incomplete cache rootfs should exist");
    let task_rootfs = temp.path().join("task-rootfs");
    let commands =
        FakeBuildahCommands::new(false, &task_rootfs).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let rootfs = call_materialize(
        &ImageSelection::PreferLocalhostThenCanonical,
        &task_rootfs,
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("corrupt cache should rebuild");

    assert_eq!(
        rootfs.cache_profile.status,
        ImageSourceCacheStatus::MissRebuilt
    );
    assert!(entry.metadata_path.is_file());
    assert_eq!(
        btrfs.calls(),
        vec![
            BtrfsCall::Delete(entry.rootfs_path.clone()),
            BtrfsCall::Snapshot {
                source: task_rootfs,
                destination: entry.rootfs_path,
            },
        ]
    );
}

#[test]
fn materializer_output_accepts_missing_digest() {
    let rootfs = parse_materializer_output(
        "selected_image=localhost/cang:latest\nimage_digest=<no value>\nrootfs_path=/tmp/rootfs\n",
    )
    .expect("output should parse");

    assert_eq!(rootfs.selected_reference, "localhost/cang:latest");
    assert_eq!(rootfs.image_digest, None);
    assert_eq!(rootfs.rootfs_path, Path::new("/tmp/rootfs"));
    assert_eq!(rootfs.process_config, OciProcessConfig::default());
}

#[test]
fn materializer_output_parses_oci_process_config_fields() {
    let rootfs = parse_materializer_output(
        concat!(
            "selected_image=localhost/cang:latest\n",
            "image_digest=sha256:feedface\n",
            "rootfs_path=/tmp/rootfs\n",
            "oci_env.1=484f4d453d2f686f6d652f646576\n",
            "oci_env.0=504154483d2f6e69782f73746f72652f666973682f62696e\n",
            "oci_cmd.0=66697368\n",
            "oci_cmd.1=2d6c\n",
            "oci_entrypoint.0=2f6e69782f73746f72652f686173682d63616e672f62696e2f63616e672d67756573742d696e6974\n",
            "oci_entrypoint.1=656e746572\n",
            "oci_entrypoint.2=2d2d\n",
            "oci_workdir=2f776f726b7370616365\n",
        ),
    )
    .expect("metadata output should parse");

    assert_eq!(
        rootfs.process_config,
        OciProcessConfig {
            env: vec![
                "PATH=/nix/store/fish/bin".to_owned(),
                "HOME=/home/dev".to_owned()
            ],
            cmd: vec!["fish".to_owned(), "-l".to_owned()],
            entrypoint: vec![
                "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
                "enter".to_owned(),
                "--".to_owned()
            ],
            working_dir: Some("/workspace".to_owned()),
        }
    );
}

#[test]
fn materializer_output_rejects_malformed_known_oci_fields() {
    assert!(
        parse_materializer_output(
            "selected_image=localhost/cang:latest\nrootfs_path=/tmp/rootfs\noci_env.x=61\n",
        )
        .is_err()
    );
    assert!(
        parse_materializer_output(
            "selected_image=localhost/cang:latest\nrootfs_path=/tmp/rootfs\noci_cmd.0=6\n",
        )
        .is_err()
    );
    assert!(
        parse_materializer_output(
            "selected_image=localhost/cang:latest\nrootfs_path=/tmp/rootfs\noci_workdir=2f\noci_workdir=2f\n",
        )
        .is_err()
    );
}

#[test]
fn compatibility_check_requires_exactly_one_executable_cang_guest_init() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let guest_init = write_guest_init(temp.path(), "hash-cang", 0o755);

    assert_eq!(
        find_cang_guest_init(temp.path()).expect("guest init should resolve"),
        guest_init
    );

    let missing = tempfile::tempdir().expect("tempdir should exist");
    assert!(find_cang_guest_init(missing.path()).is_err());

    let non_executable = tempfile::tempdir().expect("tempdir should exist");
    write_guest_init(non_executable.path(), "hash-cang", 0o644);
    assert!(find_cang_guest_init(non_executable.path()).is_err());

    write_guest_init(temp.path(), "hash-cang-two", 0o755);
    let err = find_cang_guest_init(temp.path()).expect_err("multiple init binaries fail");
    assert!(err.to_string().contains("ambiguous"));
}

#[test]
fn internal_child_snapshots_buildah_mount_and_cleans_container_on_success() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let mount_root = temp.path().join("mount");
    write_guest_init(&mount_root, "hash-cang", 0o755);
    let destination = temp.path().join("task-rootfs");
    let buildah = FakeChildBuildahCommands::new(mount_root.clone());
    let btrfs = FakeBtrfsRootfsCommands::new();

    run_btrfs_rootfs_child_with_commands(
        "ghcr.io/example/cang:dev",
        "missing",
        &destination,
        &buildah,
        &btrfs,
    )
    .expect("child materialization should succeed");

    assert_eq!(
        btrfs.calls(),
        vec![BtrfsCall::Snapshot {
            source: mount_root,
            destination: destination.clone(),
        }]
    );
    assert_eq!(
        buildah.calls(),
        vec![
            vec!["from", "--pull=missing", "ghcr.io/example/cang:dev"],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                "ghcr.io/example/cang:dev",
            ],
            vec![
                "inspect",
                "--format",
                "{{.FromImageDigest}}",
                "fake-container"
            ],
            vec!["inspect", "--format", "{{.FromImageID}}", "fake-container"],
            vec![
                "inspect",
                "--format",
                OCI_PROCESS_CONFIG_TEMPLATE,
                "fake-container"
            ],
            vec!["mount", "fake-container"],
            vec!["umount", "fake-container"],
            vec!["rm", "fake-container"],
        ]
    );
}

#[test]
fn internal_child_cleans_container_on_compatibility_failure() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let buildah = FakeChildBuildahCommands::new(temp.path().join("mount"));
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_btrfs_rootfs_child_with_commands(
        "ghcr.io/example/cang:dev",
        "missing",
        &temp.path().join("task-rootfs"),
        &buildah,
        &btrfs,
    )
    .expect_err("missing guest init should fail");

    assert!(format!("{error:#}").contains("not cang-compatible"));
    assert_eq!(
        buildah.calls(),
        vec![
            vec!["from", "--pull=missing", "ghcr.io/example/cang:dev"],
            vec![
                "inspect",
                "--type",
                "image",
                "--format",
                "{{.Digest}}",
                "ghcr.io/example/cang:dev",
            ],
            vec![
                "inspect",
                "--format",
                "{{.FromImageDigest}}",
                "fake-container"
            ],
            vec!["inspect", "--format", "{{.FromImageID}}", "fake-container"],
            vec![
                "inspect",
                "--format",
                OCI_PROCESS_CONFIG_TEMPLATE,
                "fake-container"
            ],
            vec!["mount", "fake-container"],
            vec!["umount", "fake-container"],
            vec!["rm", "fake-container"],
        ]
    );
}

fn write_image_command_cache_entry(
    cache_root: &Path,
    digest: &str,
    reference: &str,
    image_local_digest: Option<&str>,
) -> BtrfsImageCacheEntry {
    let entry = BtrfsImageCacheEntry::new(cache_root, digest).expect("cache entry should be valid");
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: reference.to_owned(),
        image_digest: Some(digest.to_owned()),
        image_local_digest: image_local_digest.map(|s| s.to_owned()),
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    entry
}

#[test]
fn image_command_list_renders_buildah_aligned_short_rows() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(
        &cache_root,
        "sha256:feedfacecafebeef00112233445566778899aabbccddeeff0011223344556677",
        "ghcr.io/example/cang:old",
        None,
    );
    let inventory = concat!(
        "<none>|<none>|dd70cff1816cafebabe|sha256:feedfacecafebeef00112233445566778899aabbccddeeff0011223344556677\n",
        "ghcr.io/example/cang|dev|ba5a514299b8ffff|sha256:1234567890abcdef00112233445566778899aabbccddeeff0011223344556677\n",
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest(
            "sha256:feedfacecafebeef00112233445566778899aabbccddeeff0011223344556677",
        )
        .with_inventory(inventory);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed")
        .render_stdout();
    assert!(output.starts_with("REPOSITORY"));
    assert!(output.contains("  TAG  "));
    assert!(output.contains("  IMAGE ID  "));
    assert!(output.contains("  DIGEST  "));
    assert!(output.contains("  CACHE  "));
    assert!(output.contains("  BUILDAH  "));
    assert!(output.contains("  PATH  "));
    assert!(!output.contains("DIGEST_KEY"));
    assert!(output.contains("dd70cff1816c"));
    assert!(output.contains("feedfacecafe"));
    assert!(output.contains("complete"));
    assert!(output.contains("match"));
    assert!(output.contains("btrfs-snapshots"));
    assert!(output.contains("<none>"));
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_list_does_not_show_uncached_buildah_rows() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(
        &cache_root,
        "sha256:feedfacecafebeef00112233445566778899aabbccddeeff0011223344556677",
        "ghcr.io/example/cang:dev",
        None,
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest(
            "sha256:feedfacecafebeef00112233445566778899aabbccddeeff0011223344556677",
        )
        .with_inventory("<none>|<none>|dd70cff1816cffff|<none>\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed");
    let ImageCacheCommandOutput::List(entries) = output else {
        panic!("list output expected");
    };

    // Only cached entries appear — uncached buildah rows are excluded.
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].status,
        super::commands::ImageCacheEntryStatus::Complete
    );
}

#[test]
fn image_command_remove_resolves_unique_visible_prefixes() {
    for (target, inventory) in [
        ("feedfac", ""),
        ("ghcr.io/example/cang:d", ""),
        (
            "ba5a514",
            "ghcr.io/example/cang|dev|ba5a514299b8ffff|sha256:feedface\n",
        ),
    ] {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let cache_root = temp.path().join("cache");
        let entry = write_image_command_cache_entry(
            &cache_root,
            "sha256:feedface",
            "ghcr.io/example/cang:dev",
            None,
        );
        let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
            .with_image_digest("sha256:feedface")
            .with_inventory(inventory);
        let btrfs = FakeBtrfsRootfsCommands::new();

        run_image_cache_command(
            ImageCacheCommand::Remove {
                dry_run: false,
                target: target.to_owned(),
            },
            &cache_root,
            &commands,
            &btrfs,
        )
        .expect("unique selector should remove cached row");

        assert!(!entry.entry_dir.exists());
        assert_eq!(btrfs.calls(), vec![BtrfsCall::Delete(entry.rootfs_path)]);
        assert!(
            commands
                .calls()
                .iter()
                .any(|call| call == &["rmi", "ghcr.io/example/cang:dev"])
        );
    }
}

#[test]
fn image_command_remove_ambiguous_selector_refuses_before_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let dev = write_image_command_cache_entry(
        &cache_root,
        "sha256:feedface",
        "ghcr.io/example/cang:dev",
        None,
    );
    let latest = write_image_command_cache_entry(
        &cache_root,
        "sha256:cafebabe",
        "ghcr.io/example/cang:latest",
        None,
    );
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "ghcr.io/example/cang".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect_err("ambiguous selector should fail");

    assert!(error.to_string().contains("matched multiple rows"));
    assert!(dev.entry_dir.exists());
    assert!(latest.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_refuses_uncached_buildah_row_before_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_inventory("ghcr.io/example/cang|dev|ba5a514299b8ffff|sha256:feedface\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "ba5a514".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect_err("uncached Buildah-only row should not be removed by cang");

    assert!(error.to_string().contains("did not match"));
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_sync_resolves_local_visible_selector_before_staging() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(
        &cache_root,
        "sha256:feedface",
        "ghcr.io/example/cang:dev",
        None,
    );
    let task_rootfs = temp.path().join("fake-output-rootfs");
    let output = format!(
        "selected_image=ghcr.io/example/cang:dev\nimage_digest=sha256:feedface\nrootfs_path={}\n",
        task_rootfs.display()
    );
    let commands = FakeBuildahCommands::new(false, &task_rootfs)
        .with_image_digest("sha256:feedface")
        .with_output(output)
        .with_inventory("ghcr.io/example/cang|dev|ba5a514299b8ffff|sha256:feedface\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    run_image_cache_command(
        ImageCacheCommand::Sync {
            reference: "ba5a514".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("sync should resolve image id prefix to a concrete reference");

    assert!(commands.calls().iter().any(|call| {
        call.first().map(String::as_str) == Some("unshare-materializer")
            && call.get(1).map(String::as_str) == Some("ghcr.io/example/cang:dev")
    }));
}

#[test]
fn image_command_sync_ambiguous_selector_fails_before_staging() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(
        &cache_root,
        "sha256:feedface",
        "ghcr.io/example/cang:dev",
        None,
    );
    write_image_command_cache_entry(
        &cache_root,
        "sha256:cafebabe",
        "ghcr.io/example/cang:latest",
        None,
    );
    let commands = FakeBuildahCommands::new(false, &temp.path().join("unused")).fail_on_unshare();
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_image_cache_command(
        ImageCacheCommand::Sync {
            reference: "ghcr.io/example/cang".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect_err("ambiguous selector should fail before materialization");

    assert!(error.to_string().contains("matched multiple local rows"));
    assert!(!cache_root.join(".staging").exists());
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_sync_populates_cache_through_staging_and_cleans_staging() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let commands = FakeBuildahCommands::new(false, &temp.path().join("fake-output-rootfs"))
        .with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Sync {
            reference: "ghcr.io/example/cang:dev".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("sync should populate cache");

    let ImageCacheCommandOutput::Sync(report) = output else {
        panic!("sync report expected");
    };
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    assert_eq!(report.digest.as_deref(), Some("sha256:feedface"));
    assert_eq!(report.digest_key.as_deref(), Some("sha256-feedface"));
    assert_eq!(report.cache_status, ImageSourceCacheStatus::MissPopulated);
    assert!(entry.metadata_path.is_file());

    let calls = btrfs.calls();
    assert_eq!(calls.len(), 2);
    match &calls[0] {
        BtrfsCall::Snapshot {
            source,
            destination,
        } => {
            assert_eq!(source, &temp.path().join("fake-output-rootfs"));
            assert_eq!(destination, &entry.rootfs_path);
        }
        other => panic!("expected cache snapshot call, got {other:?}"),
    }
    match &calls[1] {
        BtrfsCall::Delete(path) => {
            assert!(path.starts_with(cache_root.join(".staging")));
            assert!(!path.exists());
        }
        other => panic!("expected staging cleanup delete, got {other:?}"),
    }
    assert!(
        !cache_root
            .join(".staging")
            .read_dir()
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
        "sync staging directory should be empty or absent"
    );
}

#[test]
fn image_command_list_reports_complete_entries_deterministically() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");

    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed");

    let ImageCacheCommandOutput::List(entries) = output else {
        panic!("list output expected");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].digest_key.as_deref(), Some("sha256-feedface"));
    assert_eq!(entries[0].digest.as_deref(), Some("sha256:feedface"));
    assert_eq!(
        entries[0].selected_reference.as_deref(),
        Some("ghcr.io/example/cang:dev")
    );
    assert_eq!(
        entries[0].status,
        super::commands::ImageCacheEntryStatus::Complete
    );
    assert_eq!(
        entries[0].buildah_status,
        super::commands::BuildahMatchStatus::Match
    );
    assert!(commands.calls().iter().any(|call| {
        call == &[
            "inspect".to_owned(),
            "--type".to_owned(),
            "image".to_owned(),
            "--format".to_owned(),
            "{{.Digest}}".to_owned(),
            "ghcr.io/example/cang:dev".to_owned(),
        ]
    }));
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_list_reports_invalid_entries_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let complete = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&complete.rootfs_path).expect("complete rootfs should exist");
    let complete_source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: complete.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(
        &complete.metadata_path,
        format_cache_metadata(&complete_source),
    )
    .expect("metadata should exist");

    let invalid = BtrfsImageCacheEntry::new(&cache_root, "sha256:baddata").unwrap();
    fs::create_dir_all(&invalid.rootfs_path).expect("invalid rootfs should exist");
    fs::write(
        &invalid.metadata_path,
        "selected_image=ghcr.io/example/cang:old\nimage_digest=sha256:other\n",
    )
    .expect("invalid metadata should exist");

    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should tolerate invalid entries");

    let ImageCacheCommandOutput::List(entries) = output else {
        panic!("list output expected");
    };
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.digest_key.as_deref().unwrap_or("<none>"))
            .collect::<Vec<_>>(),
        vec!["sha256-baddata", "sha256-feedface"]
    );
    assert_eq!(
        entries[0].status,
        super::commands::ImageCacheEntryStatus::Invalid
    );
    assert_eq!(
        entries[0].buildah_status,
        super::commands::BuildahMatchStatus::InvalidCache
    );
    assert_eq!(
        entries[1].status,
        super::commands::ImageCacheEntryStatus::Complete
    );
    assert_eq!(
        entries[1].buildah_status,
        super::commands::BuildahMatchStatus::Match
    );
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_remove_deletes_cache_and_matching_buildah_reference() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256-feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove should succeed");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    assert_eq!(
        report.local_image_removal,
        super::commands::LocalImageRemoval::Removed {
            reference: "ghcr.io/example/cang:dev".to_owned()
        }
    );
    assert_eq!(btrfs.calls(), vec![BtrfsCall::Delete(entry.rootfs_path)]);
    assert!(commands.calls().contains(&vec![
        "rmi".to_owned(),
        "ghcr.io/example/cang:dev".to_owned()
    ]));
}

#[test]
fn image_command_remove_dry_run_reports_cache_and_matching_buildah_reference_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: true,
            target: "sha256-feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("dry-run should succeed");

    let ImageCacheCommandOutput::RemoveDryRun(report) = output else {
        panic!("dry-run remove output expected");
    };
    assert_eq!(report.digest_key, "sha256-feedface");
    assert_eq!(report.digest, "sha256:feedface");
    assert_eq!(report.local_image_target, "ghcr.io/example/cang:dev");
    assert!(entry.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_dry_run_fails_when_local_removal_would_be_skipped_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:other");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: true,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect_err("dry-run should fail when local removal would be skipped");

    assert!(error.to_string().contains("dry-run cannot preview"));
    assert!(error.to_string().contains("does not match cache digest"));
    assert!(entry.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_dry_run_preserves_selector_errors_before_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let dev = write_image_command_cache_entry(
        &cache_root,
        "sha256:feedface",
        "ghcr.io/example/cang:dev",
        None,
    );
    let latest = write_image_command_cache_entry(
        &cache_root,
        "sha256:cafebabe",
        "ghcr.io/example/cang:latest",
        None,
    );
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let error = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: true,
            target: "ghcr.io/example/cang".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect_err("ambiguous dry-run selector should fail");

    assert!(error.to_string().contains("matched multiple rows"));
    assert!(dev.entry_dir.exists());
    assert!(latest.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_is_cache_first_when_buildah_digest_mismatches() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:other");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove should succeed despite local mismatch");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    let super::commands::LocalImageRemoval::Skipped { reason } = report.local_image_removal else {
        panic!("local removal should be skipped");
    };
    assert!(reason.contains("does not match cache digest"));
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_falls_back_to_image_id_when_reference_drifted() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: Some("oldid".to_owned()),
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_digest_for("ghcr.io/example/cang:dev", "sha256:other")
        .with_digest_for("oldid", "sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove should succeed with image_id fallback");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    let super::commands::LocalImageRemoval::Removed { reference } = report.local_image_removal
    else {
        panic!("local removal should succeed via image_id fallback");
    };
    assert_eq!(reference, "oldid");
    assert!(
        commands
            .calls()
            .iter()
            .any(|call| call == &vec!["rmi".to_owned(), "oldid".to_owned()])
    );
}

#[test]
fn image_command_remove_dry_run_falls_back_to_image_id_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: Some("oldid".to_owned()),
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_digest_for("ghcr.io/example/cang:dev", "sha256:other")
        .with_digest_for("oldid", "sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: true,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("dry-run should succeed with image-id fallback");

    let ImageCacheCommandOutput::RemoveDryRun(report) = output else {
        panic!("dry-run remove output expected");
    };
    assert_eq!(report.local_image_target, "oldid");
    assert!(entry.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_by_digest_key_deletes_invalid_drifted_cache_entry() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let drifted_source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:other".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&drifted_source))
        .expect("drifted metadata should exist");
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:feedface");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256-feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove by digest key should delete invalid entry cache-first");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    let super::commands::LocalImageRemoval::Skipped { reason } = report.local_image_removal else {
        panic!("local removal should be skipped for invalid metadata");
    };
    assert!(reason.contains("cache metadata missing or invalid"));
    assert_eq!(btrfs.calls(), vec![BtrfsCall::Delete(entry.rootfs_path)]);
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_visible_selector_uses_cache_key_for_drifted_invalid_row() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let drifted = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&drifted.rootfs_path).expect("drifted rootfs should exist");
    let drifted_source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:dev".to_owned(),
        image_digest: Some("sha256:other".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: drifted.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(
        &drifted.metadata_path,
        format_cache_metadata(&drifted_source),
    )
    .expect("drifted metadata should exist");
    let other = write_image_command_cache_entry(
        &cache_root,
        "sha256:other",
        "ghcr.io/example/cang:other",
        None,
    );
    let commands =
        FakeBuildahCommands::new(false, Path::new("/unused")).with_image_digest("sha256:other");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "ghcr.io/example/cang:d".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("visible selector should remove the matched drifted cache row");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert_eq!(report.digest_key, "sha256-feedface");
    assert_eq!(report.digest, "sha256:feedface");
    assert!(!drifted.entry_dir.exists());
    assert!(other.entry_dir.exists());
    assert_eq!(btrfs.calls(), vec![BtrfsCall::Delete(drifted.rootfs_path)]);
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_list_stale_tag_detection() {
    // Old cached image had reference "ghcr.io/x/cang:latest" with digest sha256:old.
    // A newer image replaced "latest" with a different digest sha256:new.
    // The old image still exists in buildah as <none>:<none> with the old digest.
    // The cached entry (digest=sha256:old) should match the orphaned <none> row and show TAG=<none>.
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(&cache_root, "sha256:old", "ghcr.io/x/cang:latest", None);
    let inventory = concat!(
        "ghcr.io/x/cang|latest|newid|sha256:new\n",
        "<none>|<none>|oldid|sha256:old\n",
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest("sha256:old")
        .with_inventory(inventory);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed")
        .render_stdout();

    // Cached entry matched the <none>-tagged row via digest → TAG=<none>, IMAGE ID=oldid
    assert!(output.contains("<none>"));
    assert!(output.contains("oldid"));
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_list_stale_tag_reference_claimed_by_matched_entry() {
    // Two cached entries share the same selected_reference "ghcr.io/x/cang:latest".
    // Entry A (sha256:old) matches a buildah row by reference.
    // Entry B (sha256:mismatch) falls through all passes — its reference was claimed.
    // Pass 4 should set Entry B's TAG and REPOSITORY to <none>.
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    write_image_command_cache_entry(&cache_root, "sha256:old", "ghcr.io/x/cang:latest", None);
    write_image_command_cache_entry(
        &cache_root,
        "sha256:mismatch",
        "ghcr.io/x/cang:latest",
        None,
    );
    // Only one buildah row for the reference; no digest match for either cached entry.
    let inventory = concat!(
        "ghcr.io/x/cang|latest|newid|sha256:new\n",
        "<none>|<none>|orid|sha256:other\n",
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest("sha256:new")
        .with_inventory(inventory);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed")
        .render_stdout();

    // Entry A matched by reference → TAG=latest, IMAGE ID=newid
    assert!(output.contains("latest"));
    assert!(output.contains("newid"));
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_list_local_digest_matches_buildah_inventory_digest() {
    // image_local_digest (from buildah inspect --type image {{.Digest}})
    // matches the buildah inventory digest format, even when image_digest
    // (from {{.FromImageDigest}}) uses a different format.
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    // Create cache entry where image_local_digest (from buildah inspect --type image)
    // matches the buildah inventory format. image_digest uses the old format.
    let entry =
        BtrfsImageCacheEntry::new(&cache_root, "sha256:digest-format").expect("cache entry valid");
    fs::create_dir_all(&entry.rootfs_path).expect("rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/x/cang:latest".to_owned(),
        image_digest: Some("sha256:container-inspect-format".to_owned()),
        image_local_digest: Some("sha256:digest-format".to_owned()),
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("write metadata");

    let inventory = concat!(
        "ghcr.io/x/cang|latest|newid|sha256:digest-format\n",
        "<none>|<none>|oldid|sha256:other\n",
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest("sha256:digest-format")
        .with_inventory(inventory);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed")
        .render_stdout();

    assert!(output.contains("latest"));
    assert!(output.contains("newid"));
    assert!(output.contains("complete"));
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_list_stale_tag_single_unmatched_entry_with_image_id() {
    // Single cached entry with image_id=oldid. Digest (old format) doesn't
    // match any buildah inventory digest. Buildah shows the image as
    // <none>:<none> with matching image_id.
    // Pass 4 should reconcile by image_id → TAG=<none>.
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:old").expect("cache entry valid");
    fs::create_dir_all(&entry.rootfs_path).expect("rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/x/cang:latest".to_owned(),
        image_digest: Some("sha256:old".to_owned()),
        image_local_digest: None,
        image_id: Some("oldid".to_owned()),
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("write metadata");

    // Buildah inventory: the current image has a different digest; old image
    // is <none>:<none> with matching image_id but different digest.
    let inventory = concat!(
        "ghcr.io/x/cang|latest|newid|sha256:new\n",
        "<none>|<none>|oldid|sha256:different\n",
    );
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_image_digest("sha256:new")
        .with_inventory(inventory);
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(ImageCacheCommand::List, &cache_root, &commands, &btrfs)
        .expect("list should succeed")
        .render_stdout();

    // Pass 4 reconciled by image_id → TAG=<none> from buildah <none>:<none> row
    assert!(output.contains("<none>"));
    assert!(output.contains("oldid"));
    assert_eq!(btrfs.calls(), Vec::new());
}

#[test]
fn image_command_remove_falls_back_to_buildah_inventory_when_reference_inspect_fails() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:latest".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    // Stale selected_reference inspect fails; no image_id; inventory has matching row.
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_digest_for("ghcr.io/example/cang:sha-a9cc52d79353", "sha256:feedface")
        .with_inventory("ghcr.io/example/cang|sha-a9cc52d79353|ba5a514299b8ffff|sha256:feedface\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove should succeed via inventory fallback");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    let super::commands::LocalImageRemoval::Removed { reference } = report.local_image_removal
    else {
        panic!("local removal should succeed via inventory fallback");
    };
    assert_eq!(reference, "ghcr.io/example/cang:sha-a9cc52d79353");
    assert!(commands.calls().iter().any(|call| call
        == &vec![
            "rmi".to_owned(),
            "ghcr.io/example/cang:sha-a9cc52d79353".to_owned()
        ]));
}

#[test]
fn image_command_remove_dry_run_falls_back_to_buildah_inventory_without_mutation() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:latest".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_digest_for("ghcr.io/example/cang:sha-a9cc52d79353", "sha256:feedface")
        .with_inventory("ghcr.io/example/cang|sha-a9cc52d79353|ba5a514299b8ffff|sha256:feedface\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: true,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("dry-run should succeed via inventory fallback");

    let ImageCacheCommandOutput::RemoveDryRun(report) = output else {
        panic!("dry-run remove output expected");
    };
    assert_eq!(
        report.local_image_target,
        "ghcr.io/example/cang:sha-a9cc52d79353"
    );
    assert!(entry.entry_dir.exists());
    assert_eq!(btrfs.calls(), Vec::new());
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}

#[test]
fn image_command_remove_skips_inventory_fallback_when_no_matching_row() {
    let temp = tempfile::tempdir().expect("tempdir should exist");
    let cache_root = temp.path().join("cache");
    let entry = BtrfsImageCacheEntry::new(&cache_root, "sha256:feedface").unwrap();
    fs::create_dir_all(&entry.rootfs_path).expect("cache rootfs should exist");
    let source = ImageSourceRootfs {
        selected_reference: "ghcr.io/example/cang:latest".to_owned(),
        image_digest: Some("sha256:feedface".to_owned()),
        image_local_digest: None,
        image_id: None,
        rootfs_path: entry.rootfs_path.clone(),
        process_config: OciProcessConfig::default(),
        cache_profile: ImageSourceCacheProfile::direct_uncached("test"),
    };
    fs::write(&entry.metadata_path, format_cache_metadata(&source)).expect("metadata should exist");
    // Stale selected_reference inspect fails; no image_id; inventory has no matching row.
    let commands = FakeBuildahCommands::new(false, Path::new("/unused"))
        .with_inventory("ghcr.io/example/cang|othertag|newid|sha256:other-digest\n");
    let btrfs = FakeBtrfsRootfsCommands::new();

    let output = run_image_cache_command(
        ImageCacheCommand::Remove {
            dry_run: false,
            target: "sha256:feedface".to_owned(),
        },
        &cache_root,
        &commands,
        &btrfs,
    )
    .expect("remove should succeed despite no local match");

    let ImageCacheCommandOutput::Remove(report) = output else {
        panic!("remove output expected");
    };
    assert!(!entry.entry_dir.exists());
    let super::commands::LocalImageRemoval::Skipped { reason } = report.local_image_removal else {
        panic!("local removal should be skipped");
    };
    assert!(reason.contains("is missing, digestless, or ambiguous"));
    assert!(
        !commands
            .calls()
            .iter()
            .any(|call| call.first().map(String::as_str) == Some("rmi"))
    );
}
