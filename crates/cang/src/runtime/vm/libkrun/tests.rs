use crate::logging::LogLevel;
use crate::runtime::launch::config::{
    BindMount, CARGO_TAG, CARGO_TARGET, CODEX_TAG, CODEX_TARGET, CORTEXKIT_TAG, CORTEXKIT_TARGET,
    DIRGE_CONFIG_TAG, DIRGE_CONFIG_TARGET, DIRGE_DATA_TAG, DIRGE_DATA_TARGET, DIRGE_HOME_TAG,
    DIRGE_HOME_TARGET, DiskAttachment, LaunchConfig, LaunchSpec, ManagedSessionConfig, NetworkMode,
    OMP_TAG, OMP_TARGET, PI_TAG, PI_TARGET, PulseBridgeConfig, SCCACHE_TAG, SCCACHE_TARGET,
    WORKSPACE_TAG, WORKSPACE_TARGET, WaypipeConfig,
};
use crate::runtime::seccomp::{AuditMode, SeccompMode};
use crate::runtime::vm::gpu::GpuMode;
use crate::runtime::vm::libkrun::launcher::{
    NET_FLAG_DHCP_CLIENT, PROFILE_KERNEL_CMDLINE_APPEND, guest_nofile_rlimit_entry,
    with_audit_start_marker_hook_for_test,
};
use crate::runtime::vm::libkrun::{
    CANG_LIBKRUN_COMPAT_NET_FEATURES, DirectLibkrunLauncher, LibkrunApi,
    nested_virt_symbol_presence_for_test, planned_libkrun_load_order,
    planned_libkrun_load_order_for_exe, required_rlimits_symbol_presence_for_test,
};
use anyhow::Result;
use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    CreateCtx,
    FreeCtx(u32),
    InitLog(u32),
    SetVmConfig(u32, u8, u32),
    SetGpuOptions3(u32, u32, u64, i32),
    CheckNestedVirt,
    SetNestedVirt(u32, bool),
    SetRoot(u32, String),
    AddDisk(u32, String, String, bool),
    AddNetUnixstream(u32, i32, u32),
    AddVsockPort(u32, u32, String, bool),
    SetPortMap(u32, Vec<String>),
    DisableImplicitConsole(u32),
    SetConsoleOutput(u32, String),
    AddVirtioConsoleDefault(u32, i32, i32, i32),
    SetWorkdir(u32, String),
    SetExec(u32, String, Vec<String>, Vec<(String, String)>),
    SetRlimits(u32, Vec<String>),
    SetProfilePath(u32, String),
    SetKernelCmdlineAppend(u32, String),
    PreEnterHook,
    AuditStartMarker,
    StartEnter(u32),
}

#[derive(Clone)]
struct FakeLibkrunApi {
    calls: Rc<RefCell<Vec<Call>>>,
    fail_call: Option<&'static str>,
    net_dhcp_flag_unsupported: bool,
    nested_check_result: Option<i32>,
}

impl FakeLibkrunApi {
    fn new(calls: Rc<RefCell<Vec<Call>>>) -> Self {
        Self {
            calls,
            fail_call: None,
            net_dhcp_flag_unsupported: false,
            nested_check_result: Some(1),
        }
    }

    fn failing(calls: Rc<RefCell<Vec<Call>>>, fail_call: &'static str) -> Self {
        Self {
            calls,
            fail_call: Some(fail_call),
            net_dhcp_flag_unsupported: false,
            nested_check_result: Some(1),
        }
    }

    fn net_dhcp_flag_unsupported(calls: Rc<RefCell<Vec<Call>>>) -> Self {
        Self {
            calls,
            fail_call: None,
            net_dhcp_flag_unsupported: true,
            nested_check_result: Some(1),
        }
    }

    fn nested_check_result(mut self, result: Option<i32>) -> Self {
        self.nested_check_result = result;
        self
    }

    fn rc(&self, call: &'static str) -> i32 {
        if self.fail_call == Some(call) { -22 } else { 0 }
    }
}

impl LibkrunApi for FakeLibkrunApi {
    fn create_ctx(&mut self) -> Result<u32> {
        self.calls.borrow_mut().push(Call::CreateCtx);
        Ok(7)
    }

    fn free_ctx(&mut self, ctx_id: u32) -> Result<()> {
        self.calls.borrow_mut().push(Call::FreeCtx(ctx_id));
        Ok(())
    }

    fn init_log(&mut self, level: u32) -> Result<i32> {
        self.calls.borrow_mut().push(Call::InitLog(level));
        Ok(self.rc("libkrun_log_init"))
    }

    fn set_vm_config(&mut self, ctx_id: u32, vcpus: u8, ram_mib: u32) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetVmConfig(ctx_id, vcpus, ram_mib));
        Ok(self.rc("krun_set_vm_config"))
    }

    fn set_gpu_options3(
        &mut self,
        ctx_id: u32,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<Option<i32>> {
        self.calls.borrow_mut().push(Call::SetGpuOptions3(
            ctx_id,
            virgl_flags,
            shm_size,
            render_server_fd,
        ));
        Ok(Some(self.rc("krun_set_gpu_options3")))
    }

    fn check_nested_virt(&mut self) -> Result<Option<i32>> {
        self.calls.borrow_mut().push(Call::CheckNestedVirt);
        Ok(self.nested_check_result)
    }

    fn set_nested_virt(&mut self, ctx_id: u32, enabled: bool) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetNestedVirt(ctx_id, enabled));
        Ok(self.rc("krun_set_nested_virt"))
    }

    fn set_root(&mut self, ctx_id: u32, root_path: &Path) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetRoot(ctx_id, root_path.display().to_string()));
        Ok(self.rc("krun_set_root"))
    }

    fn add_disk(
        &mut self,
        ctx_id: u32,
        block_id: &str,
        disk_path: &Path,
        read_only: bool,
    ) -> Result<i32> {
        self.calls.borrow_mut().push(Call::AddDisk(
            ctx_id,
            block_id.to_owned(),
            disk_path.display().to_string(),
            read_only,
        ));
        Ok(self.rc("krun_add_disk"))
    }

    fn disable_implicit_console(&mut self, ctx_id: u32) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::DisableImplicitConsole(ctx_id));
        Ok(self.rc("krun_disable_implicit_console"))
    }

    fn set_console_output(&mut self, ctx_id: u32, output_path: &Path) -> Result<i32> {
        self.calls.borrow_mut().push(Call::SetConsoleOutput(
            ctx_id,
            output_path.display().to_string(),
        ));
        Ok(self.rc("krun_set_console_output"))
    }

    fn add_virtio_console_default(
        &mut self,
        ctx_id: u32,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<i32> {
        self.calls.borrow_mut().push(Call::AddVirtioConsoleDefault(
            ctx_id, input_fd, output_fd, err_fd,
        ));
        Ok(self.rc("krun_add_virtio_console_default"))
    }

    fn add_net_unixstream(&mut self, ctx_id: u32, socket_fd: i32, flags: u32) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::AddNetUnixstream(ctx_id, socket_fd, flags));
        if self.net_dhcp_flag_unsupported && flags == 2 {
            return Ok(-libc::EINVAL);
        }
        Ok(self.rc("krun_add_net_unixstream"))
    }

    fn add_vsock_port(
        &mut self,
        ctx_id: u32,
        guest_port: u32,
        socket_path: &Path,
        listen: bool,
    ) -> Result<i32> {
        self.calls.borrow_mut().push(Call::AddVsockPort(
            ctx_id,
            guest_port,
            socket_path.display().to_string(),
            listen,
        ));
        Ok(self.rc("krun_add_vsock_port2"))
    }

    fn set_port_map(&mut self, ctx_id: u32, port_map: &[String]) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetPortMap(ctx_id, port_map.to_vec()));
        Ok(self.rc("krun_set_port_map"))
    }

    fn set_workdir(&mut self, ctx_id: u32, workdir: &str) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetWorkdir(ctx_id, workdir.to_owned()));
        Ok(self.rc("krun_set_workdir"))
    }

    fn set_exec(
        &mut self,
        ctx_id: u32,
        exec_path: &str,
        argv: &[String],
        env: &[(String, String)],
    ) -> Result<i32> {
        self.calls.borrow_mut().push(Call::SetExec(
            ctx_id,
            exec_path.to_owned(),
            argv.to_vec(),
            env.to_vec(),
        ));
        Ok(self.rc("krun_set_exec"))
    }

    fn set_rlimits(&mut self, ctx_id: u32, rlimits: &[String]) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetRlimits(ctx_id, rlimits.to_vec()));
        Ok(self.rc("krun_set_rlimits"))
    }

    fn set_profile_path(&mut self, ctx_id: u32, profile_path: &Path) -> Result<i32> {
        self.calls.borrow_mut().push(Call::SetProfilePath(
            ctx_id,
            profile_path.display().to_string(),
        ));
        Ok(self.rc("krun_set_profile_path"))
    }

    fn set_kernel_cmdline_append(&mut self, ctx_id: u32, fragment: &str) -> Result<i32> {
        self.calls
            .borrow_mut()
            .push(Call::SetKernelCmdlineAppend(ctx_id, fragment.to_owned()));
        Ok(self.rc("krun_set_kernel_cmdline_append"))
    }

    fn start_enter(&mut self, ctx_id: u32) -> Result<i32> {
        self.calls.borrow_mut().push(Call::StartEnter(ctx_id));
        Ok(self.rc("krun_start_enter"))
    }
}

fn config() -> LaunchConfig {
    LaunchConfig::build_for_task(LaunchSpec {
        task_rootfs: Path::new("/rootfs"),
        hostname: "cang-workspace",
        mounts: &test_mounts(),
        guest_init_override: None,
        guest_init_exec: "/nix/store/hash-cang/bin/cang-guest-init",
        guest_command: &[],
        image_process_config:
            &crate::runtime::session::rootfs::image_source::OciProcessConfig::default(),
        mem_gib: Some(4),
        log_level: LogLevel::Debug,
        network_mode: NetworkMode::Tsi,
        pulse: None,
        gpu_mode: GpuMode::Off,
        wayland: false,
        new_perms: crate::runtime::launch::config::GuestPermissions::default(),
        publish: &[],
        profile: false,
        root: false,
        allocator: crate::runtime::launch::config::AllocatorMode::Mimalloc,
        host_uid: 1000,
        host_gid: 1001,
        vcpus: 2,
        disks: vec![
            DiskAttachment {
                id: "cang-nix".to_owned(),
                path: Path::new("/state/cang-nix.raw").to_path_buf(),
                read_only: false,
            },
            DiskAttachment {
                id: "cang-containers".to_owned(),
                path: Path::new("/state/cang-containers.raw").to_path_buf(),
                read_only: false,
            },
        ],
        extra_env: Vec::new(),
        host_nix_overlay: None,
        pulse_bridge: None,
        waypipe: None,
        exec: None,
        managed_session: None,
    })
    .expect("config should build")
}

fn waypipe_config(socket: &Path) -> LaunchConfig {
    LaunchConfig {
        exec: None,
        waypipe: Some(WaypipeConfig {
            socket: socket.into(),
            guest_port: 50_427,
        }),
        ..config()
    }
}

fn managed_config(attach_socket: &Path) -> LaunchConfig {
    LaunchConfig {
        managed_session: Some(ManagedSessionConfig {
            attach_socket: attach_socket.into(),
            guest_port: 50_426,
            protocol_version: 1,
            attach_socket_uid: unsafe { libc::geteuid() },
            attach_socket_gid: unsafe { libc::getegid() },
            cleanup_task_rootfs_on_exit: true,
            guest_kernel_console_log: attach_socket.into(),
        }),
        ..config()
    }
}

fn waypipe_managed_config(waypipe_socket: &Path, attach_socket: &Path) -> LaunchConfig {
    LaunchConfig {
        exec: None,
        waypipe: Some(WaypipeConfig {
            socket: waypipe_socket.into(),
            guest_port: 50_427,
        }),
        managed_session: Some(ManagedSessionConfig {
            attach_socket: attach_socket.into(),
            guest_port: 50_426,
            protocol_version: 1,
            attach_socket_uid: unsafe { libc::geteuid() },
            attach_socket_gid: unsafe { libc::getegid() },
            cleanup_task_rootfs_on_exit: true,
            guest_kernel_console_log: attach_socket.into(),
        }),
        ..config()
    }
}

fn managed_attach_socket_path() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let socket = dir.path().join("attach.sock");
    assert!(
        !socket.exists(),
        "libkrun creates the managed attach socket during/after VM start"
    );
    (dir, socket)
}

fn passt_config() -> LaunchConfig {
    LaunchConfig {
        network_mode: NetworkMode::Passt,
        publish: Vec::new(),
        ..config().with_passt_fd(42)
    }
}

fn tsi_publish_config() -> LaunchConfig {
    LaunchConfig {
        publish: vec!["8080:80".to_owned(), "8443:443".to_owned()],
        ..config()
    }
}

fn passt_publish_config() -> LaunchConfig {
    LaunchConfig {
        publish: vec!["8080:80".to_owned(), "udp:5353:5353".to_owned()],
        ..passt_config()
    }
}

fn test_mounts() -> Vec<BindMount> {
    vec![
        BindMount::directory("/workspace-src", WORKSPACE_TAG, WORKSPACE_TARGET),
        BindMount::directory("/home/host/.codex", CODEX_TAG, CODEX_TARGET),
        BindMount::directory("/home/host/.omp", OMP_TAG, OMP_TARGET),
        BindMount::directory("/home/host/.pi", PI_TAG, PI_TARGET),
        BindMount::directory(
            "/home/host/.local/share/cortexkit",
            CORTEXKIT_TAG,
            CORTEXKIT_TARGET,
        ),
        BindMount::directory(
            "/home/host/.config/dirge",
            DIRGE_CONFIG_TAG,
            DIRGE_CONFIG_TARGET,
        ),
        BindMount::directory(
            "/home/host/.local/share/dirge",
            DIRGE_DATA_TAG,
            DIRGE_DATA_TARGET,
        ),
        BindMount::directory("/home/host/.dirge", DIRGE_HOME_TAG, DIRGE_HOME_TARGET),
        BindMount::directory("/state/project/cargo", CARGO_TAG, CARGO_TARGET),
        BindMount::directory("/state/sccache", SCCACHE_TAG, SCCACHE_TARGET),
    ]
}

#[test]
fn libkrun_loader_prefers_explicit_library_override_before_sonames() {
    assert_eq!(
        planned_libkrun_load_order(Some("/tmp/libkrun-custom.so")),
        vec!["/tmp/libkrun-custom.so"]
    );
    assert_eq!(
        planned_libkrun_load_order(None),
        vec!["libkrun.so.2", "libkrun.so"]
    );
    assert_eq!(
        planned_libkrun_load_order(Some("")),
        vec!["libkrun.so.2", "libkrun.so"]
    );
}

#[test]
fn libkrun_loader_tries_package_relative_libraries_before_sonames() {
    assert_eq!(
        planned_libkrun_load_order_for_exe(
            None,
            Some(std::path::PathBuf::from("/nix/store/hash-cang/bin/cang"))
        ),
        vec![
            "/nix/store/hash-cang/lib/cang/libkrun.so.2",
            "/nix/store/hash-cang/lib/cang/libkrun.so",
            "libkrun.so.2",
            "libkrun.so",
        ]
    );
}

#[test]
fn compat_net_features_match_libkrun_header_contract() {
    assert_eq!(
        CANG_LIBKRUN_COMPAT_NET_FEATURES,
        (1 << 0) | (1 << 1) | (1 << 7) | (1 << 10) | (1 << 11) | (1 << 14)
    );
}

#[test]
fn passt_net_flags_match_libkrun_header_width() {
    fn assert_u32(_: u32) {}

    assert_u32(NET_FLAG_DHCP_CLIENT);
}

#[test]
fn nested_virt_symbol_resolution_allows_missing_check_but_requires_set() {
    let set_symbol = std::ptr::dangling_mut::<std::os::raw::c_void>();
    assert_eq!(
        nested_virt_symbol_presence_for_test(None, Some(set_symbol))
            .expect("missing diagnostic check symbol should be accepted"),
        (false, true)
    );

    let err = nested_virt_symbol_presence_for_test(Some(set_symbol), None)
        .expect_err("missing set symbol should fail");
    assert!(format!("{err:#}").contains("krun_set_nested_virt"));
}

#[test]
fn rlimits_symbol_resolution_requires_krun_set_rlimits() {
    let set_rlimits_symbol = std::ptr::dangling_mut::<std::os::raw::c_void>();

    assert!(
        required_rlimits_symbol_presence_for_test(Some(set_rlimits_symbol))
            .expect("present rlimits symbol should be accepted")
    );

    let err = required_rlimits_symbol_presence_for_test(None)
        .expect_err("missing rlimits symbol should fail");
    assert!(format!("{err:#}").contains("krun_set_rlimits"));
}

#[test]
fn fake_api_records_direct_libkrun_v1_call_order() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_with_host_nofile_hard_limit(&config(), 1_048_576)
        .expect("launch should succeed");

    let calls = calls.borrow();
    assert_eq!(calls[0], Call::InitLog(4));
    assert_eq!(calls[1], Call::CreateCtx);
    assert_eq!(calls[2], Call::SetVmConfig(7, 2, 4096));
    assert_eq!(calls[3], Call::CheckNestedVirt);
    assert_eq!(calls[4], Call::SetNestedVirt(7, true));
    assert_eq!(calls[5], Call::SetRoot(7, "/rootfs".to_owned()));
    assert_eq!(
        calls[6],
        Call::AddDisk(
            7,
            "cang-nix".to_owned(),
            "/state/cang-nix.raw".to_owned(),
            false,
        )
    );
    assert_eq!(
        calls[7],
        Call::AddDisk(
            7,
            "cang-containers".to_owned(),
            "/state/cang-containers.raw".to_owned(),
            false,
        )
    );
    assert_eq!(calls[8], Call::DisableImplicitConsole(7));
    assert_eq!(calls[9], Call::AddVirtioConsoleDefault(7, 0, 1, 2));
    assert_eq!(calls[10], Call::SetWorkdir(7, "/workspace".to_owned()));
    assert_eq!(
        calls[11],
        Call::SetExec(
            7,
            "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
            vec!["enter".to_owned(), "fish".to_owned(), "-l".to_owned()],
            vec![("KRUN_CONFIG".to_owned(), "/.cang_config.json".to_owned())]
        )
    );
    assert_eq!(
        calls[12],
        Call::SetRlimits(7, vec![format!("{}=1048576:1048576", libc::RLIMIT_NOFILE)])
    );
    assert_eq!(calls[13], Call::StartEnter(7));
    assert_eq!(
        calls.len(),
        14,
        "TSI default must not call krun_set_env, passt APIs, profile cmdline, or per-bind virtiofs devices"
    );
}

#[test]
fn drm_gpu_mode_enables_venus_render_server_flags_before_start() {
    const VIRGLRENDERER_USE_EGL: u32 = 1 << 0;
    const VIRGLRENDERER_THREAD_SYNC: u32 = 1 << 1;
    const VIRGLRENDERER_VENUS: u32 = 1 << 6;
    const VIRGLRENDERER_RENDER_SERVER: u32 = 1 << 9;
    const VIRGLRENDERER_DRM: u32 = 1 << 10;
    const VIRGLRENDERER_USE_VIDEO: u32 = 1 << 11;
    const GPU_SHM_SIZE_BYTES: u64 = 256 * 1024 * 1024;

    // configure_gpu reads the render-server fd number from the env var the
    // supervisor sets on the VM worker.
    let previous_fd = std::env::var("CANG_RENDER_SERVER_FD").ok();
    // SAFETY: test-only env mutation, restored at the end of the test.
    unsafe { std::env::set_var("CANG_RENDER_SERVER_FD", "9") };

    let calls = Rc::new(RefCell::new(Vec::new()));
    let config = LaunchConfig {
        gpu_mode: GpuMode::Drm,
        ..config()
    };
    let result = DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&config)
        .map_err(|_| ());

    // SAFETY: restore the env var for other tests.
    unsafe {
        match previous_fd {
            Some(value) => std::env::set_var("CANG_RENDER_SERVER_FD", value),
            None => std::env::remove_var("CANG_RENDER_SERVER_FD"),
        }
    }
    result.expect("launch should succeed");

    let calls = calls.borrow();
    let gpu_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetGpuOptions3(..)))
        .expect("drm GPU mode should configure GPU options");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");

    assert_eq!(
        calls[gpu_index],
        Call::SetGpuOptions3(
            7,
            VIRGLRENDERER_USE_EGL
                | VIRGLRENDERER_THREAD_SYNC
                | VIRGLRENDERER_VENUS
                | VIRGLRENDERER_RENDER_SERVER
                | VIRGLRENDERER_DRM
                | VIRGLRENDERER_USE_VIDEO,
            GPU_SHM_SIZE_BYTES,
            9,
        )
    );
    assert!(gpu_index < start_index);
}

#[test]
fn nofile_rlimit_entry_uses_linux_resource_id_and_host_hard_as_soft_and_hard() {
    assert_eq!(
        guest_nofile_rlimit_entry(1_048_576),
        format!("{}=1048576:1048576", libc::RLIMIT_NOFILE)
    );
}

#[test]
fn nested_virt_setup_failure_is_setup_failure_and_frees_context() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_set_nested_virt",
    ))
    .start_enter(&config())
    .expect_err("nested virt setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed: krun_set_nested_virt"));
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn nested_virt_check_unsupported_failure_or_absent_still_sets_nested_virt() {
    for check_result in [Some(0), Some(-5), None] {
        let calls = Rc::new(RefCell::new(Vec::new()));
        DirectLibkrunLauncher::new(
            FakeLibkrunApi::new(calls.clone()).nested_check_result(check_result),
        )
        .start_enter(&config())
        .expect("launch should continue after non-fatal nested check diagnostic");

        let calls = calls.borrow();
        let check_index = calls
            .iter()
            .position(|call| matches!(call, Call::CheckNestedVirt))
            .expect("nested support check should be attempted");
        let set_index = calls
            .iter()
            .position(|call| matches!(call, Call::SetNestedVirt(7, true)))
            .expect("nested virt should still be requested");
        let start_index = calls
            .iter()
            .position(|call| matches!(call, Call::StartEnter(7)))
            .expect("launch should start");

        assert!(check_index < set_index);
        assert!(set_index < start_index);
    }
}

#[test]
fn pulse_bridge_adds_guest_to_host_vsock_connector() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let socket = dir.path().join("pulse.sock");
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut config = config();
    config.pulse_bridge = Some(PulseBridgeConfig {
        socket: socket.clone(),
        guest_port: 50_429,
        host_port: 4714,
    });

    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&config)
        .expect("Pulse bridge launch should succeed");

    assert!(calls.borrow().contains(&Call::AddVsockPort(
        7,
        50_429,
        socket.display().to_string(),
        false,
    )));
}

#[test]
fn waypipe_adds_guest_to_host_vsock_connector() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let socket = dir.path().join("waypipe.sock");
    let calls = Rc::new(RefCell::new(Vec::new()));

    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&waypipe_config(&socket))
        .expect("waypipe launch should succeed");

    let calls = calls.borrow();
    assert!(calls.contains(&Call::AddVsockPort(
        7,
        50_427,
        socket.display().to_string(),
        false,
    )));
}

#[test]
fn waypipe_and_managed_session_register_independent_vsock_channels() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let waypipe_socket = dir.path().join("waypipe.sock");
    let attach_socket = dir.path().join("attach.sock");
    let calls = Rc::new(RefCell::new(Vec::new()));

    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&waypipe_managed_config(&waypipe_socket, &attach_socket))
        .expect("combined launch should succeed");

    let calls = calls.borrow();
    assert!(calls.contains(&Call::AddVsockPort(
        7,
        50_427,
        waypipe_socket.display().to_string(),
        false,
    )));
    assert!(calls.contains(&Call::AddVsockPort(
        7,
        50_426,
        attach_socket.display().to_string(),
        true,
    )));
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::AddVsockPort(..)))
            .count(),
        2
    );
}

#[test]
fn waypipe_vsock_registration_failure_is_setup_failure_and_frees_context() {
    let dir = tempfile::tempdir().expect("tempdir should be created");
    let socket = dir.path().join("waypipe.sock");
    let calls = Rc::new(RefCell::new(Vec::new()));

    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_add_vsock_port2",
    ))
    .start_enter(&waypipe_config(&socket))
    .expect_err("waypipe launch should fail when libkrun rejects vsock registration");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_add_vsock_port2 Waypipe"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::StartEnter(..)))
    );
}

#[test]
fn managed_session_adds_vsock_listener_and_starts_before_host_socket_exists() {
    let (_dir, socket) = managed_attach_socket_path();
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&managed_config(&socket))
        .expect("managed launch should succeed");

    assert!(
        !socket.exists(),
        "launcher must not require libkrun-managed host socket before start"
    );
    let calls = calls.borrow();
    let managed = managed_config(&socket);
    let console_path = managed
        .managed_session
        .as_ref()
        .expect("managed session")
        .guest_kernel_console_log
        .display()
        .to_string();
    assert!(
        calls.contains(&Call::SetConsoleOutput(7, console_path)),
        "managed launch should route the guest kernel console to a file: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::DisableImplicitConsole(..))),
        "a managed launch must keep the implicit console it captures"
    );
    let vsock_index = calls
        .iter()
        .position(|call| matches!(call, Call::AddVsockPort(..)))
        .expect("managed launch should add vsock port");
    let console_index = calls
        .iter()
        .position(|call| matches!(call, Call::AddVirtioConsoleDefault(..)))
        .expect("console should be configured");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");

    assert_eq!(
        calls[vsock_index],
        Call::AddVsockPort(7, 50_426, socket.display().to_string(), true)
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::AddVsockPort(..)))
            .count(),
        1
    );
    assert!(vsock_index < console_index);
    assert!(console_index < start_index);
    assert!(vsock_index < start_index);
}

#[test]
fn managed_session_vsock_registration_failure_is_setup_failure_and_frees_context() {
    let (_dir, socket) = managed_attach_socket_path();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_add_vsock_port2",
    ))
    .start_enter(&managed_config(&socket))
    .expect_err("managed launch should fail when libkrun rejects vsock registration");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_add_vsock_port2"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::AddVsockPort(..)))
    );
    assert!(
        calls.contains(&Call::FreeCtx(7)),
        "managed vsock registration setup failure should free the libkrun context"
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::StartEnter(..))),
        "VM must not start if libkrun rejects managed vsock registration"
    );
}

#[test]
fn passt_mode_adds_unixstream_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&passt_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    let net_index = calls
        .iter()
        .position(|call| matches!(call, Call::AddNetUnixstream(..)))
        .expect("passt mode should add net unixstream");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");

    assert_eq!(calls[net_index], Call::AddNetUnixstream(7, 42, 2));
    assert!(net_index < start_index);
}

#[test]
fn passt_mode_retries_without_dhcp_client_flag_when_libkrun_rejects_it() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::net_dhcp_flag_unsupported(calls.clone()))
        .start_enter(&passt_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    let net_calls = calls
        .iter()
        .filter(|call| matches!(call, Call::AddNetUnixstream(..)))
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(
        net_calls,
        vec![
            Call::AddNetUnixstream(7, 42, 2),
            Call::AddNetUnixstream(7, 42, 0)
        ]
    );
}

#[test]
fn tsi_publish_calls_set_port_map_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&tsi_publish_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    let port_map_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetPortMap(..)))
        .expect("TSI publish should set port map");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");

    assert_eq!(
        calls[port_map_index],
        Call::SetPortMap(7, vec!["8080:80".to_owned(), "8443:443".to_owned()])
    );
    assert!(port_map_index < start_index);
}

#[test]
fn passt_publish_does_not_call_set_port_map() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&passt_publish_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::AddNetUnixstream(..)))
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::SetPortMap(..)))
    );
}

#[test]
fn pre_enter_hook_runs_after_setup_and_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_with_pre_enter_hook(&config(), || {
            let calls = calls.borrow();
            assert!(calls.iter().any(|call| matches!(call, Call::SetExec(..))));
            assert!(
                !calls
                    .iter()
                    .any(|call| matches!(call, Call::StartEnter(..)))
            );
            Ok(())
        })
        .expect("launch should succeed");

    let calls = calls.borrow();
    let set_exec_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetExec(..)))
        .expect("setup should configure exec before hook");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start after hook");
    assert!(set_exec_index < start_index);
}

#[test]
fn audit_start_marker_runs_immediately_before_start_enter() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut config = config();
    config.seccomp = SeccompMode::Audit(AuditMode::Full {
        trace_path: PathBuf::from("/tmp/cang-seccomp-audit.jsonl"),
    });

    let marker_calls = calls.clone();
    with_audit_start_marker_hook_for_test(
        move || {
            marker_calls.borrow_mut().push(Call::AuditStartMarker);
            Ok(())
        },
        || {
            DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
                .start_enter_with_pre_enter_hook(&config, || {
                    calls.borrow_mut().push(Call::PreEnterHook);
                    Ok(())
                })
        },
    )
    .expect("launch should succeed");

    let calls = calls.borrow();
    let set_exec_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetExec(..)))
        .expect("setup should configure exec");
    let pre_enter_index = calls
        .iter()
        .position(|call| matches!(call, Call::PreEnterHook))
        .expect("pre-enter hook should run");
    let marker_index = calls
        .iter()
        .position(|call| matches!(call, Call::AuditStartMarker))
        .expect("audit start marker should run");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");
    assert!(set_exec_index < pre_enter_index);
    assert!(pre_enter_index < marker_index);
    assert_eq!(marker_index + 1, start_index);
}

#[test]
fn audit_markers_are_not_emitted_outside_audit_mode() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let marker_calls = calls.clone();

    with_audit_start_marker_hook_for_test(
        move || {
            marker_calls.borrow_mut().push(Call::AuditStartMarker);
            Ok(())
        },
        || DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone())).start_enter(&config()),
    )
    .expect("launch should succeed");

    assert!(
        !calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::AuditStartMarker))
    );
}

#[test]
fn profile_setup_runs_after_exec_and_before_pre_enter_hook() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_profiled_with_pre_enter_hook(
            &config(),
            Some(Path::new("/tmp/vm-worker-host-profile.tsv")),
            || {
                let calls = calls.borrow();
                assert!(calls.iter().any(|call| matches!(call, Call::SetExec(..))));
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::SetNestedVirt(7, true)))
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::SetProfilePath(..)))
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::SetKernelCmdlineAppend(..)))
                );
                assert!(
                    !calls
                        .iter()
                        .any(|call| matches!(call, Call::StartEnter(..)))
                );
                Ok(())
            },
        )
        .expect("launch should succeed");

    let calls = calls.borrow();
    let nested_set_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetNestedVirt(7, true)))
        .expect("nested virt should be configured");
    let set_exec_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetExec(..)))
        .expect("setup should configure exec");
    let set_profile_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetProfilePath(..)))
        .expect("profile path should be configured");
    let set_cmdline_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetKernelCmdlineAppend(..)))
        .expect("profile cmdline diagnostics should be configured");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");

    assert_eq!(
        calls[set_profile_index],
        Call::SetProfilePath(7, "/tmp/vm-worker-host-profile.tsv".to_owned())
    );
    assert_eq!(
        calls[set_cmdline_index],
        Call::SetKernelCmdlineAppend(7, PROFILE_KERNEL_CMDLINE_APPEND.to_owned())
    );
    assert!(nested_set_index < set_exec_index);
    assert!(set_exec_index < set_profile_index);
    assert!(set_profile_index < set_cmdline_index);
    assert!(set_cmdline_index < start_index);
}

#[test]
fn passt_mode_requires_prepared_socket_fd() {
    let mut config = config();
    config.network_mode = NetworkMode::Passt;
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&config)
        .expect_err("missing fd should fail setup");

    assert!(format!("{err:#}").contains("prepared passt socket fd"));
    assert!(calls.borrow().contains(&Call::FreeCtx(7)));
}

#[test]
fn setup_failure_is_classified_and_frees_context_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_set_root"))
        .start_enter(&config())
        .expect_err("setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed"));
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn set_port_map_failure_is_setup_failure_and_frees_context() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err =
        DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_set_port_map"))
            .start_enter(&tsi_publish_config())
            .expect_err("port-map setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed: krun_set_port_map"));
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn rlimit_setup_failure_is_setup_failure_and_frees_context_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err =
        DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_set_rlimits"))
            .start_enter_with_host_nofile_hard_limit(&config(), 1_048_576)
            .expect_err("rlimit setup failure should fail");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_set_rlimits"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(calls.contains(&Call::SetRlimits(
        7,
        vec![format!("{}=1048576:1048576", libc::RLIMIT_NOFILE)]
    )));
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn tsi_invalid_publish_fails_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let invalid = LaunchConfig {
        publish: vec!["udp:5353:5353".to_owned()],
        ..config()
    };
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&invalid)
        .expect_err("unsupported TSI publish should fail");

    assert!(format!("{err:#}").contains("TSI publish"));
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn console_registration_failure_frees_context_before_exec() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_add_virtio_console_default",
    ))
    .start_enter(&config())
    .expect_err("console setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed"));
    let calls = calls.borrow();
    assert!(calls.contains(&Call::FreeCtx(7)));
    assert!(!calls.iter().any(|call| matches!(call, Call::SetExec(..))));
    assert!(!calls.contains(&Call::StartEnter(7)));
}

#[test]
fn start_failure_is_classified() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err =
        DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_start_enter"))
            .start_enter(&config())
            .expect_err("start failure should fail");

    assert!(format!("{err:#}").contains("libkrun start failed"));
    assert!(calls.borrow().contains(&Call::StartEnter(7)));
}

#[test]
fn audit_start_marker_runs_before_nonzero_start_result_is_classified() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut config = config();
    config.seccomp = SeccompMode::Audit(AuditMode::Full {
        trace_path: PathBuf::from("/tmp/cang-seccomp-audit.jsonl"),
    });

    let marker_calls = calls.clone();
    let err = with_audit_start_marker_hook_for_test(
        move || {
            marker_calls.borrow_mut().push(Call::AuditStartMarker);
            Ok(())
        },
        || {
            DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_start_enter"))
                .start_enter(&config)
        },
    )
    .expect_err("nonzero start rc should fail after start marker");

    assert!(format!("{err:#}").contains("libkrun start failed"));
    let calls = calls.borrow();
    let start_marker_index = calls
        .iter()
        .position(|call| matches!(call, Call::AuditStartMarker))
        .expect("audit start marker should run");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::StartEnter(..)))
        .expect("launch should start");
    assert_eq!(start_marker_index + 1, start_index);
}
