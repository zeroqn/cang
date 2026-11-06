use crate::guest_init::components::env::{
    CONTAINERS_STORE_ENV, CangEnv, ContainerStoreBackend, ENTER_AS_ROOT_ENV, NIX_LOWER_BLOCK_ENV,
    NIX_LOWER_DISK_ID_ENV, NIX_LOWER_DISK_LABEL_ENV, NIX_LOWER_UPPERDIR_ENV, NIX_LOWER_WORKDIR_ENV,
    NixLowerContract, RAW_CONTAINER_DISK_ID, RAW_CONTAINER_DISK_LABEL, RAW_NIX_DISK_ID,
    RAW_NIX_DISK_LABEL, RAW_NIX_LOWER_DISK_ID, RAW_NIX_LOWER_DISK_LABEL,
};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn internal_runtime_disk_contract_defaults_match_host_contract() {
    assert_eq!(RAW_NIX_DISK_ID, "cang-nix");
    assert_eq!(RAW_NIX_DISK_LABEL, "CANG_NIX");
    assert_eq!(RAW_CONTAINER_DISK_ID, "cang-containers");
    assert_eq!(RAW_CONTAINER_DISK_LABEL, "CANG_CONTAINERS");
    assert_eq!(ENTER_AS_ROOT_ENV, "CANG_ENTER_AS_ROOT");
}

#[test]
fn internal_runtime_parses_raw_disk_container_store_backend_contract() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var("CANG_CONTAINERS_STORAGE", "1");
        std::env::set_var(CONTAINERS_STORE_ENV, "raw-disk");
    }
    let raw = CangEnv::from_process_env().expect("raw backend should parse");
    unsafe {
        std::env::remove_var("CANG_CONTAINERS_STORAGE");
        std::env::remove_var(CONTAINERS_STORE_ENV);
    }

    assert!(raw.containers_storage);
    assert_eq!(raw.container_store_backend, ContainerStoreBackend::RawDisk);
}

#[test]
fn internal_runtime_legacy_container_storage_defaults_to_raw_disk() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var("CANG_CONTAINERS_STORAGE", "1");
        std::env::remove_var(CONTAINERS_STORE_ENV);
    }
    let parsed = CangEnv::from_process_env().expect("legacy env should parse");
    unsafe {
        std::env::remove_var("CANG_CONTAINERS_STORAGE");
    }

    assert_eq!(
        parsed.container_store_backend,
        ContainerStoreBackend::RawDisk
    );
}

#[test]
fn internal_runtime_rejects_unknown_container_store_backend() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var(CONTAINERS_STORE_ENV, "overlay");
    }
    let err = CangEnv::from_process_env().expect_err("unknown backend should fail");
    unsafe {
        std::env::remove_var(CONTAINERS_STORE_ENV);
    }

    assert!(err.to_string().contains(CONTAINERS_STORE_ENV));
}

#[test]
fn internal_runtime_rejects_bind_container_store_backend() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var(CONTAINERS_STORE_ENV, "bind");
    }
    let err = CangEnv::from_process_env().expect_err("bind backend should fail");
    unsafe {
        std::env::remove_var(CONTAINERS_STORE_ENV);
    }

    assert!(err.to_string().contains(CONTAINERS_STORE_ENV));
}

#[test]
fn internal_runtime_parses_authoritative_host_nix_overlay_marker() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var("CANG_NIX_OVERLAY", "1");
        std::env::set_var("CANG_NIX_HOST_OVERLAY", "1");
    }
    let parsed = CangEnv::from_process_env().expect("env should parse");
    unsafe {
        std::env::remove_var("CANG_NIX_OVERLAY");
        std::env::remove_var("CANG_NIX_HOST_OVERLAY");
    }

    assert!(parsed.nix_overlay);
    assert!(parsed.nix_host_overlay);
}

#[test]
fn internal_runtime_parses_unified_permissions() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    unsafe {
        std::env::set_var(
            "CANG_PERMISSIONS",
            "perf,bpf,sys-admin,io-uring,net-admin,net-raw,bpf,sys-admin",
        );
    }

    let parsed = CangEnv::from_process_env().expect("env should parse");

    assert_eq!(
        parsed.permissions.to_string(),
        "io-uring,net-admin,net-raw,bpf,perf,sys-admin"
    );
    unsafe {
        std::env::remove_var("CANG_PERMISSIONS");
    }
}

#[test]
fn internal_runtime_parses_gpu_drm_independently_from_wayland() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // SAFETY: test mutates process env in a small single-threaded assertion.
    unsafe {
        std::env::set_var("CANG_GPU_DRM", "1");
        std::env::remove_var("CANG_WAYLAND");
    }
    let parsed = CangEnv::from_process_env().expect("env should parse");
    unsafe {
        std::env::remove_var("CANG_GPU_DRM");
    }

    assert!(parsed.gpu_drm);
    assert!(!parsed.wayland);
}

#[test]
fn internal_runtime_rejects_invalid_permissions() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    for value in ["", "io-uring,,perf", "mount-admin"] {
        unsafe {
            std::env::set_var("CANG_PERMISSIONS", value);
        }
        let error = CangEnv::from_process_env().expect_err("invalid permissions should fail");
        assert!(format!("{error:#}").contains("CANG_PERMISSIONS"));
    }
    unsafe {
        std::env::remove_var("CANG_PERMISSIONS");
    }
}

#[test]
fn nix_lower_contract_is_absent_without_the_block_flag() {
    let parsed = NixLowerContract::from_lookup(|name| match name {
        NIX_LOWER_UPPERDIR_ENV => Some("/run/cang/nix-overlay/upper".to_owned()),
        NIX_LOWER_WORKDIR_ENV => Some("/run/cang/nix-overlay/work-guest".to_owned()),
        _ => None,
    })
    .expect("no flag must not be an error");

    assert_eq!(parsed, None, "the host-composed overlay stays the default");
}

#[test]
fn nix_lower_contract_requires_both_overlay_directories() {
    let err = NixLowerContract::from_lookup(|name| match name {
        NIX_LOWER_BLOCK_ENV => Some("1".to_owned()),
        NIX_LOWER_UPPERDIR_ENV => Some("/run/cang/nix-overlay/upper".to_owned()),
        _ => None,
    })
    .expect_err("a missing workdir must fail loudly");

    assert!(format!("{err:#}").contains(NIX_LOWER_WORKDIR_ENV));
}

#[test]
fn nix_lower_contract_rejects_relative_directories() {
    let err = NixLowerContract::from_lookup(|name| match name {
        NIX_LOWER_BLOCK_ENV => Some("1".to_owned()),
        NIX_LOWER_UPPERDIR_ENV => Some("upper".to_owned()),
        NIX_LOWER_WORKDIR_ENV => Some("/run/cang/nix-overlay/work-guest".to_owned()),
        _ => None,
    })
    .expect_err("a relative overlay directory must fail");

    assert!(format!("{err:#}").contains("absolute"));
}

#[test]
fn nix_lower_contract_carries_the_host_defaults() {
    let parsed = NixLowerContract::from_lookup(|name| match name {
        NIX_LOWER_BLOCK_ENV => Some("1".to_owned()),
        NIX_LOWER_UPPERDIR_ENV => Some("/run/cang/nix-overlay/upper".to_owned()),
        NIX_LOWER_WORKDIR_ENV => Some("/run/cang/nix-overlay/work-guest".to_owned()),
        _ => None,
    })
    .expect("block mode should parse")
    .expect("block mode is on");

    assert_eq!(parsed.disk_id, RAW_NIX_LOWER_DISK_ID);
    assert_eq!(parsed.disk_label, RAW_NIX_LOWER_DISK_LABEL);
    assert_eq!(
        parsed.upperdir,
        std::path::PathBuf::from("/run/cang/nix-overlay/upper")
    );
    assert_eq!(
        parsed.workdir,
        std::path::PathBuf::from("/run/cang/nix-overlay/work-guest")
    );
}

#[test]
fn nix_lower_contract_takes_the_attached_disk_names() {
    let parsed = NixLowerContract::from_lookup(|name| match name {
        NIX_LOWER_BLOCK_ENV => Some("1".to_owned()),
        NIX_LOWER_DISK_ID_ENV => Some("cang-nix-lower".to_owned()),
        NIX_LOWER_DISK_LABEL_ENV => Some("CANG_NIX_LOWER".to_owned()),
        NIX_LOWER_UPPERDIR_ENV => Some("/run/cang/nix-overlay/upper".to_owned()),
        NIX_LOWER_WORKDIR_ENV => Some("/run/cang/nix-overlay/work-guest".to_owned()),
        _ => None,
    })
    .expect("block mode should parse")
    .expect("block mode is on");

    assert_eq!(parsed.disk_id, "cang-nix-lower");
    assert_eq!(parsed.disk_label, "CANG_NIX_LOWER");
}
