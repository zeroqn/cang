use anyhow::bail;
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;

use crate::guest_init::components::nix::root::{
    NixOperation, PreseedState, attempt_marker, classify_preseed_state, completion_sentinel,
    ensure_host_overlay_nix_is_directory, overlay_options, planned_host_overlay_operations,
    planned_operations, planned_profile_labels, preseed_upper_with, store_reconcile_args,
};

#[test]
fn nix_prep_operation_order_marks_ready_without_socket_wait() {
    let ops = planned_operations();
    let pos = |op| ops.iter().position(|candidate| candidate == &op).unwrap();
    assert_eq!(
        ops,
        vec![
            NixOperation::WriteRunningStatus,
            NixOperation::FindDisk,
            NixOperation::MountDisk,
            NixOperation::PreseedUpper,
            NixOperation::MountOverlay,
            NixOperation::RepairStore,
            NixOperation::CreateSocketDir,
            NixOperation::StartDaemon,
            NixOperation::WriteReadyStatus,
        ]
    );
    assert!(pos(NixOperation::MountDisk) < pos(NixOperation::PreseedUpper));
    assert!(pos(NixOperation::PreseedUpper) < pos(NixOperation::MountOverlay));
    assert!(pos(NixOperation::MountOverlay) < pos(NixOperation::RepairStore));
    assert!(pos(NixOperation::RepairStore) < pos(NixOperation::CreateSocketDir));
    assert!(pos(NixOperation::StartDaemon) < pos(NixOperation::WriteReadyStatus));
}

#[test]
fn host_overlay_nix_prep_skips_raw_disk_and_guest_overlay_operations() {
    let ops = planned_host_overlay_operations();

    assert_eq!(
        ops,
        vec![
            NixOperation::WriteRunningStatus,
            NixOperation::RepairStore,
            NixOperation::CreateSocketDir,
            NixOperation::StartDaemon,
            NixOperation::WriteReadyStatus,
        ]
    );
    assert!(!ops.contains(&NixOperation::FindDisk));
    assert!(!ops.contains(&NixOperation::MountDisk));
    assert!(!ops.contains(&NixOperation::PreseedUpper));
    assert!(!ops.contains(&NixOperation::MountOverlay));
    assert!(ops.contains(&NixOperation::RepairStore));
}

#[test]
fn host_overlay_requires_nix_to_be_directory() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let nix_file = dir.path().join("nix");
    fs::write(&nix_file, "not a dir").expect("nix file");

    let err = ensure_host_overlay_nix_is_directory(&nix_file)
        .expect_err("host overlay /nix must be a directory");

    assert!(format!("{err:#}").contains("is not a directory"));
}

#[test]
fn host_overlay_accepts_nix_directory() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let nix_dir = dir.path().join("nix");
    fs::create_dir_all(&nix_dir).expect("nix dir");

    ensure_host_overlay_nix_is_directory(&nix_dir).expect("directory");
}

#[test]
fn overlay_options_keep_userxattr_without_redirect_dir() {
    let options = overlay_options(
        PathBuf::from("/lower").as_path(),
        PathBuf::from("/upper").as_path(),
        PathBuf::from("/work").as_path(),
    );

    assert_eq!(
        options,
        "lowerdir=/lower,upperdir=/upper,workdir=/work,userxattr"
    );
}

#[test]
fn nix_prep_profile_labels_stop_after_daemon_spawn() {
    let labels = planned_profile_labels();
    let pos = |label| {
        labels
            .iter()
            .position(|candidate| candidate == &label)
            .unwrap()
    };

    assert_eq!(
        labels,
        vec![
            "nix-prep:require-tools",
            "nix-prep:find-disk",
            "nix-prep:prepare-run-dirs",
            "nix-prep:mount-disk",
            "nix-prep:preseed-upper",
            "nix-prep:mount-overlay",
            "nix-prep:repair-store",
            "nix-prep:create-socket-dir",
            "nix-prep:start-daemon",
        ]
    );
    assert_eq!(labels.first(), Some(&"nix-prep:require-tools"));
    assert!(pos("nix-prep:mount-disk") < pos("nix-prep:preseed-upper"));
    assert!(pos("nix-prep:preseed-upper") < pos("nix-prep:mount-overlay"));
    assert!(pos("nix-prep:mount-overlay") < pos("nix-prep:repair-store"));
    assert!(pos("nix-prep:repair-store") < pos("nix-prep:create-socket-dir"));
    assert_eq!(labels.last(), Some(&"nix-prep:start-daemon"));
    assert!(!labels.iter().any(|label| label.contains("wait-socket")));
}

#[test]
fn preseed_state_completed_wins_over_attempt_and_legacy_state() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let upper = dir.path().join("upper");
    fs::create_dir_all(upper.join("var/nix/db")).expect("legacy dir should be created");
    fs::write(attempt_marker(&upper), "attempted\n").expect("attempt marker should be written");
    fs::write(completion_sentinel(&upper), "preseeded\n")
        .expect("completion marker should be written");

    assert_eq!(classify_preseed_state(&upper), PreseedState::Completed);
}

#[test]
fn preseed_state_migrates_legacy_only_without_attempt_marker() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let upper = dir.path().join("upper");
    fs::create_dir_all(upper.join("var/nix/profiles")).expect("legacy dir should be created");

    assert_eq!(classify_preseed_state(&upper), PreseedState::LegacySeeded);

    fs::write(attempt_marker(&upper), "attempted\n").expect("attempt marker should be written");
    assert_eq!(classify_preseed_state(&upper), PreseedState::FreshOrRetry);
}

#[test]
fn preseed_state_ignores_dirs_created_by_bootstrap() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let upper = dir.path().join("upper");
    fs::create_dir_all(upper.join("var/nix")).expect("bootstrap dirs should be created");

    assert_eq!(classify_preseed_state(&upper), PreseedState::FreshOrRetry);
}

#[test]
fn fresh_empty_upper_copies_and_writes_completion() {
    let harness = PreseedHarness::new();
    let copied = Cell::new(false);
    let repaired = Cell::new(false);

    preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |lower_var, upper_var| {
            copied.set(true);
            assert_eq!(lower_var, harness.lower.join("var"));
            assert_eq!(upper_var, harness.upper.join("var"));
            fs::create_dir_all(upper_var.join("nix/db"))?;
            Ok(())
        },
        |upper| {
            repaired.set(true);
            assert_eq!(upper, harness.upper);
            Ok(())
        },
    )
    .expect("fresh upper should preseed successfully");

    assert!(copied.get(), "fresh preseed should copy lower /nix/var");
    assert!(repaired.get(), "fresh preseed should still repair");
    assert!(completion_sentinel(&harness.upper).exists());
    assert!(!attempt_marker(&harness.upper).exists());
}

#[test]
fn completed_upper_skips_copy_but_repairs() {
    let harness = PreseedHarness::new();
    fs::create_dir_all(&harness.upper).expect("upper dir should be created");
    fs::write(completion_sentinel(&harness.upper), "preseeded\n")
        .expect("completion marker should be written");
    fs::write(attempt_marker(&harness.upper), "attempted\n")
        .expect("stale attempt marker should be written");
    let repaired = Cell::new(false);

    preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |_, _| bail!("copy must be skipped when completion sentinel exists"),
        |upper| {
            repaired.set(true);
            assert_eq!(upper, harness.upper);
            Ok(())
        },
    )
    .expect("completed upper should repair successfully");

    assert!(repaired.get());
    assert!(completion_sentinel(&harness.upper).exists());
    assert!(!attempt_marker(&harness.upper).exists());
}

#[test]
fn legacy_upper_without_sentinels_skips_copy_and_writes_completion() {
    let harness = PreseedHarness::new();
    fs::create_dir_all(harness.upper.join("var/nix/db")).expect("legacy db should be created");
    let repaired = Cell::new(false);

    preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |_, _| bail!("legacy seeded upper should not recopy /nix/var"),
        |upper| {
            repaired.set(true);
            assert_eq!(upper, harness.upper);
            Ok(())
        },
    )
    .expect("legacy upper should migrate successfully");

    assert!(repaired.get());
    assert!(completion_sentinel(&harness.upper).exists());
    assert!(!attempt_marker(&harness.upper).exists());
}

#[test]
fn failed_copy_leaves_attempt_marker_without_completion() {
    let harness = PreseedHarness::new();

    let err = preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |_, upper_var| {
            fs::create_dir_all(upper_var.join("nix/db"))?;
            bail!("copy failed after partial state")
        },
        |_| bail!("repair should not run after copy failure"),
    )
    .expect_err("copy failure should fail preseed");

    assert!(format!("{err:#}").contains("copy failed after partial state"));
    assert!(attempt_marker(&harness.upper).exists());
    assert!(!completion_sentinel(&harness.upper).exists());
    assert_eq!(
        classify_preseed_state(&harness.upper),
        PreseedState::FreshOrRetry
    );
}

#[test]
fn failed_repair_after_copy_leaves_attempt_marker_without_completion() {
    let harness = PreseedHarness::new();

    let err = preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |_, upper_var| {
            fs::create_dir_all(upper_var.join("nix/db"))?;
            Ok(())
        },
        |_| bail!("repair failed"),
    )
    .expect_err("repair failure should fail preseed");

    assert!(format!("{err:#}").contains("repair failed"));
    assert!(attempt_marker(&harness.upper).exists());
    assert!(!completion_sentinel(&harness.upper).exists());
    assert_eq!(
        classify_preseed_state(&harness.upper),
        PreseedState::FreshOrRetry
    );
}

#[test]
fn partial_failed_copy_retries_instead_of_migrating_as_legacy() {
    let harness = PreseedHarness::new();
    fs::create_dir_all(&harness.upper).expect("upper dir should be created");
    fs::write(attempt_marker(&harness.upper), "attempted\n")
        .expect("attempt marker should be written");
    fs::create_dir_all(harness.upper.join("var/nix/db")).expect("partial db should be created");
    let copied = Cell::new(false);

    preseed_upper_with(
        &harness.lower,
        &harness.upper,
        &harness.work,
        |_, _| {
            copied.set(true);
            Ok(())
        },
        |_| Ok(()),
    )
    .expect("retry after partial copy should succeed");

    assert!(
        copied.get(),
        "attempt marker should force fresh/retry copy path"
    );
    assert!(completion_sentinel(&harness.upper).exists());
    assert!(!attempt_marker(&harness.upper).exists());
}

#[test]
fn missing_lower_var_repairs_without_writing_completion() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let lower = dir.path().join("lower");
    let upper = dir.path().join("upper");
    let work = dir.path().join("work");
    fs::create_dir_all(&lower).expect("lower dir should be created without var");
    let repaired = Cell::new(false);

    preseed_upper_with(
        &lower,
        &upper,
        &work,
        |_, _| bail!("copy should be skipped when lower /nix/var is missing"),
        |_| {
            repaired.set(true);
            Ok(())
        },
    )
    .expect("missing lower var should preserve previous success behavior");

    assert!(repaired.get());
    assert!(!completion_sentinel(&upper).exists());
    assert!(!attempt_marker(&upper).exists());
}

struct PreseedHarness {
    _tempdir: tempfile::TempDir,
    lower: PathBuf,
    upper: PathBuf,
    work: PathBuf,
}

impl PreseedHarness {
    fn new() -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let lower = tempdir.path().join("lower");
        let upper = tempdir.path().join("upper");
        let work = tempdir.path().join("work");
        fs::create_dir_all(lower.join("var/nix")).expect("lower var should be created");
        Self {
            _tempdir: tempdir,
            lower,
            upper,
            work,
        }
    }
}

#[test]
fn seeded_generation_imports_the_parked_db_and_never_fetches() {
    assert_eq!(
        store_reconcile_args(true),
        vec![
            "--import-db",
            "/nix/var/nix/.cang-previous-db/db.sqlite",
            "--purge-only"
        ]
    );
    assert_eq!(store_reconcile_args(false), Vec::<&str>::new());
}
