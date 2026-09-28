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
use crate::runtime::vm::libkrun::DirectLibkrunLauncher;
use crate::runtime::vm::libkrun::launcher::{
    PROFILE_KERNEL_CMDLINE_APPEND, guest_nofile_rlimit_entry, with_audit_start_marker_hook_for_test,
};
use anyhow::{Result, anyhow, bail};
use cang_libkrun::{CANG_LIBKRUN_COMPAT_NET_FEATURES, LibkrunApi};
use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

/// Handles the recording fake hands out. Only the VMM builder is reboxed: its
/// C entry points take `KrunVmmBuilder*`, so libkrun may return a different
/// pointer from every builder call and the launcher must follow it.
const VMM_BUILDER: usize = 1;
const DEVICES: usize = 2;
const OVERLAY: usize = 3;
const PAYLOAD: usize = 4;
const INIT_BUILDER: usize = 5;
const INIT_CONFIG: usize = 6;
const VSOCK: usize = 7;
const ROOTFS: usize = 8;
const NET_DEVICE: usize = 9;
const VMM: usize = 50;
const CONSOLE_BUILDER_BASE: usize = 20;
const CONSOLE_DEVICE_BASE: usize = 30;
const BLOCK_DEVICE_BASE: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    InitLog(u32),
    CheckNestedVirt,
    PayloadLoadKrunfw,
    PayloadAppendCmdline(usize, String),
    FsOverlayNew,
    InitConfigBuilder,
    InitBuilderArgs(usize, Vec<String>),
    InitBuilderEnv(usize, Vec<(String, String)>),
    InitBuilderWorkdir(usize, String),
    InitBuilderRlimits(usize, Vec<String>),
    InitBuilderDhcp(usize, bool),
    InitBuilderBuild(usize),
    InitConfigApplyIn(usize, usize, usize),
    MmioDeviceManagerNew,
    MmioDeviceManagerAdd(usize, usize),
    FsDeviceNew(String, String, bool),
    FsDeviceSetOverlay(usize, usize),
    BlockDeviceNew(usize, String, String, bool),
    NetDeviceNewUnixstreamFd(i32, u32, u32),
    VsockDeviceNew(u64, u32),
    VsockDeviceAddUnixPort(usize, u32, String, bool),
    VsockDeviceAddPortForward(usize, String),
    ConsoleDeviceBuilder,
    ConsoleBuilderAddDefaultConsole(usize, i32, i32, i32),
    ConsoleBuilderAddInoutPort(usize, String, Option<i32>, Option<i32>),
    ConsoleBuilderBuild(usize),
    GpuDeviceNew(u32, u64, i32),
    VmmBuilderNew,
    VmmBuilderVcpus(usize, u8),
    VmmBuilderRamMib(usize, u32),
    VmmBuilderNestedVirt(usize, bool),
    VmmBuilderPayload(usize, usize),
    VmmBuilderDevices(usize, usize),
    VmmBuilderDestroy(usize),
    VmmBuilderBuild(usize),
    VmmRun(usize),
    SetProfilePath(usize, String),
    PreEnterHook,
    AuditStartMarker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedCheck {
    Supported,
    Unsupported,
    Failing,
}

struct FakeLibkrunApi {
    calls: Rc<RefCell<Vec<Call>>>,
    fail_call: Option<&'static str>,
    nested_check: NestedCheck,
    next_console_builder: usize,
    next_console_device: usize,
    next_block_device: usize,
}

impl FakeLibkrunApi {
    fn new(calls: Rc<RefCell<Vec<Call>>>) -> Self {
        Self {
            calls,
            fail_call: None,
            nested_check: NestedCheck::Supported,
            next_console_builder: CONSOLE_BUILDER_BASE,
            next_console_device: CONSOLE_DEVICE_BASE,
            next_block_device: BLOCK_DEVICE_BASE,
        }
    }

    fn failing(calls: Rc<RefCell<Vec<Call>>>, fail_call: &'static str) -> Self {
        Self {
            fail_call: Some(fail_call),
            ..Self::new(calls)
        }
    }

    fn nested_check(mut self, nested_check: NestedCheck) -> Self {
        self.nested_check = nested_check;
        self
    }

    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }

    fn fails(&self, name: &str) -> bool {
        self.fail_call == Some(name)
    }

    fn checked(&self, name: &str) -> Result<()> {
        if self.fails(name) {
            bail!("fake {name} failure");
        }
        Ok(())
    }

    fn next_console_builder(&mut self) -> usize {
        let handle = self.next_console_builder;
        self.next_console_builder += 1;
        handle
    }

    fn next_console_device(&mut self) -> usize {
        let handle = self.next_console_device;
        self.next_console_device += 1;
        handle
    }

    fn next_block_device(&mut self) -> usize {
        let handle = self.next_block_device;
        self.next_block_device += 1;
        handle
    }
}

impl LibkrunApi for FakeLibkrunApi {
    fn init_log(&mut self, level: u32) -> Result<()> {
        self.record(Call::InitLog(level));
        self.checked("krun_init_log")
    }

    fn check_nested_virt(&mut self) -> Result<bool> {
        self.record(Call::CheckNestedVirt);
        match self.nested_check {
            NestedCheck::Supported => Ok(true),
            NestedCheck::Unsupported => Ok(false),
            NestedCheck::Failing => Err(anyhow!("fake krun_check_nested_virt failure")),
        }
    }

    fn payload_load_krunfw(&mut self) -> Result<usize> {
        self.record(Call::PayloadLoadKrunfw);
        self.checked("krun_payload_load_krunfw")?;
        Ok(PAYLOAD)
    }

    fn payload_append_cmdline(&mut self, payload: usize, fragment: &str) -> Result<()> {
        self.record(Call::PayloadAppendCmdline(payload, fragment.to_owned()));
        self.checked("krun_payload_append_cmdline")
    }

    fn fs_overlay_new(&mut self) -> Result<usize> {
        self.record(Call::FsOverlayNew);
        self.checked("krun_fs_overlay_new")?;
        Ok(OVERLAY)
    }

    fn init_config_builder(&mut self) -> Result<usize> {
        self.record(Call::InitConfigBuilder);
        self.checked("krun_init_config_builder")?;
        Ok(INIT_BUILDER)
    }

    fn init_builder_args(&mut self, builder: usize, args: &[String]) -> Result<usize> {
        self.record(Call::InitBuilderArgs(builder, args.to_vec()));
        self.checked("krun_init_builder_args")?;
        Ok(builder)
    }

    fn init_builder_env(&mut self, builder: usize, env: &[(String, String)]) -> Result<usize> {
        self.record(Call::InitBuilderEnv(builder, env.to_vec()));
        self.checked("krun_init_builder_env")?;
        Ok(builder)
    }

    fn init_builder_workdir(&mut self, builder: usize, workdir: &str) -> Result<usize> {
        self.record(Call::InitBuilderWorkdir(builder, workdir.to_owned()));
        self.checked("krun_init_builder_workdir")?;
        Ok(builder)
    }

    fn init_builder_rlimits(&mut self, builder: usize, rlimits: &[String]) -> Result<usize> {
        self.record(Call::InitBuilderRlimits(builder, rlimits.to_vec()));
        self.checked("krun_init_builder_rlimits")?;
        Ok(builder)
    }

    fn init_builder_dhcp(&mut self, builder: usize, enable: bool) -> Result<usize> {
        self.record(Call::InitBuilderDhcp(builder, enable));
        self.checked("krun_init_builder_dhcp")?;
        Ok(builder)
    }

    fn init_builder_build(&mut self, builder: usize) -> Result<usize> {
        self.record(Call::InitBuilderBuild(builder));
        self.checked("krun_init_builder_build")?;
        Ok(INIT_CONFIG)
    }

    fn init_config_apply_in(
        &mut self,
        config: usize,
        overlay: usize,
        payload: usize,
    ) -> Result<()> {
        self.record(Call::InitConfigApplyIn(config, overlay, payload));
        self.checked("krun_init_config_apply_in")
    }

    fn mmio_device_manager_new(&mut self) -> Result<usize> {
        self.record(Call::MmioDeviceManagerNew);
        self.checked("krun_mmio_device_manager_new")?;
        Ok(DEVICES)
    }

    fn mmio_device_manager_add(&mut self, devices: usize, device: usize) -> Result<()> {
        self.record(Call::MmioDeviceManagerAdd(devices, device));
        self.checked("krun_mmio_device_manager_add")
    }

    fn fs_device_new(&mut self, tag: &str, host_path: &Path, read_only: bool) -> Result<usize> {
        self.record(Call::FsDeviceNew(
            tag.to_owned(),
            host_path.display().to_string(),
            read_only,
        ));
        self.checked("krun_fs_device_new")?;
        Ok(ROOTFS)
    }

    fn fs_device_set_overlay(&mut self, device: usize, overlay: usize) -> Result<()> {
        self.record(Call::FsDeviceSetOverlay(device, overlay));
        self.checked("krun_fs_device_set_overlay")
    }

    fn block_device_new(&mut self, id: &str, path: &Path, read_only: bool) -> Result<usize> {
        self.checked("krun_block_device_new")?;
        let handle = self.next_block_device();
        self.record(Call::BlockDeviceNew(
            handle,
            id.to_owned(),
            path.display().to_string(),
            read_only,
        ));
        Ok(handle)
    }

    fn net_device_new_unixstream_fd(
        &mut self,
        _id: &str,
        fd: i32,
        mac: [u8; 6],
        features: u32,
        flags: u32,
    ) -> Result<usize> {
        self.record(Call::NetDeviceNewUnixstreamFd(fd, features, flags));
        debug_assert_eq!(mac.len(), 6);
        self.checked("krun_net_device_new_unixstream_fd")?;
        Ok(NET_DEVICE)
    }

    fn vsock_device_new(&mut self, cid: u64, tsi_features: u32) -> Result<usize> {
        self.record(Call::VsockDeviceNew(cid, tsi_features));
        self.checked("krun_vsock_device_new")?;
        Ok(VSOCK)
    }

    fn vsock_device_add_unix_port(
        &mut self,
        device: usize,
        port: u32,
        path: &Path,
        listen: bool,
    ) -> Result<()> {
        self.record(Call::VsockDeviceAddUnixPort(
            device,
            port,
            path.display().to_string(),
            listen,
        ));
        self.checked("krun_vsock_device_add_unix_port")
    }

    fn vsock_device_add_port_forward(&mut self, device: usize, mapping: &str) -> Result<()> {
        self.record(Call::VsockDeviceAddPortForward(device, mapping.to_owned()));
        self.checked("krun_vsock_device_add_port_forward")
    }

    fn console_device_builder(&mut self) -> Result<usize> {
        self.record(Call::ConsoleDeviceBuilder);
        self.checked("krun_console_device_builder")?;
        Ok(self.next_console_builder())
    }

    fn console_builder_add_default_console(
        &mut self,
        builder: usize,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> Result<()> {
        self.record(Call::ConsoleBuilderAddDefaultConsole(
            builder, input_fd, output_fd, err_fd,
        ));
        self.checked("krun_console_builder_add_default_console")
    }

    fn console_builder_add_inout_port(
        &mut self,
        builder: usize,
        name: &str,
        input_fd: Option<i32>,
        output_fd: Option<i32>,
    ) -> Result<()> {
        self.record(Call::ConsoleBuilderAddInoutPort(
            builder,
            name.to_owned(),
            input_fd,
            output_fd,
        ));
        self.checked("krun_console_builder_add_inout_port")
    }

    fn console_builder_build(&mut self, builder: usize) -> Result<usize> {
        self.record(Call::ConsoleBuilderBuild(builder));
        self.checked("krun_console_builder_build")?;
        Ok(self.next_console_device())
    }

    fn gpu_device_new(
        &mut self,
        virgl_flags: u32,
        shm_size: u64,
        render_server_fd: i32,
    ) -> Result<usize> {
        self.record(Call::GpuDeviceNew(virgl_flags, shm_size, render_server_fd));
        self.checked("krun_gpu_device_new")?;
        Ok(10)
    }

    fn vmm_builder_new(&mut self) -> Result<usize> {
        self.record(Call::VmmBuilderNew);
        self.checked("krun_vmm_builder_new")?;
        Ok(VMM_BUILDER)
    }

    fn vmm_builder_vcpus(&mut self, builder: usize, vcpus: u8) -> Result<usize> {
        self.record(Call::VmmBuilderVcpus(builder, vcpus));
        self.checked("krun_vmm_builder_vcpus")?;
        Ok(builder + 1)
    }

    fn vmm_builder_ram_mib(&mut self, builder: usize, ram_mib: u32) -> Result<usize> {
        self.record(Call::VmmBuilderRamMib(builder, ram_mib));
        self.checked("krun_vmm_builder_ram_mib")?;
        Ok(builder + 1)
    }

    fn vmm_builder_nested_virt(&mut self, builder: usize, enabled: bool) -> Result<usize> {
        self.record(Call::VmmBuilderNestedVirt(builder, enabled));
        self.checked("krun_vmm_builder_nested_virt")?;
        Ok(builder + 1)
    }

    fn vmm_builder_payload(&mut self, builder: usize, payload: usize) -> Result<usize> {
        self.record(Call::VmmBuilderPayload(builder, payload));
        self.checked("krun_vmm_builder_payload")?;
        Ok(builder + 1)
    }

    fn vmm_builder_devices(&mut self, builder: usize, devices: usize) -> Result<usize> {
        self.record(Call::VmmBuilderDevices(builder, devices));
        self.checked("krun_vmm_builder_devices")?;
        Ok(builder + 1)
    }

    fn vmm_builder_destroy(&mut self, builder: usize) -> Result<()> {
        self.record(Call::VmmBuilderDestroy(builder));
        Ok(())
    }

    fn vmm_builder_build(&mut self, builder: usize) -> Result<usize> {
        self.record(Call::VmmBuilderBuild(builder));
        self.checked("krun_vmm_builder_build")?;
        Ok(VMM)
    }

    fn vmm_run(&mut self, vmm: usize) -> Result<()> {
        self.record(Call::VmmRun(vmm));
        self.checked("krun_vmm_run")
    }

    fn set_profile_path(&mut self, builder: usize, profile_path: &Path) -> Result<usize> {
        self.record(Call::SetProfilePath(
            builder,
            profile_path.display().to_string(),
        ));
        self.checked("krun_vmm_builder_set_profile_path")?;
        Ok(builder + 1)
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
            guest_kernel_console_log: managed_console_log_path(attach_socket),
        }),
        ..config()
    }
}

fn managed_console_log_path(attach_socket: &Path) -> PathBuf {
    attach_socket.with_file_name("guest-kernel-console.log")
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
            guest_kernel_console_log: managed_console_log_path(attach_socket),
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
fn compat_net_features_match_libkrun_header_contract() {
    assert_eq!(
        CANG_LIBKRUN_COMPAT_NET_FEATURES,
        (1 << 0) | (1 << 1) | (1 << 7) | (1 << 10) | (1 << 11) | (1 << 14)
    );
}

#[test]
fn fake_api_records_direct_libkrun_v2_call_order() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let expected = config();
    let mut expected_env = expected.env.clone();
    expected_env.extend(expected.guest_config_env.iter().cloned());
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_with_host_nofile_hard_limit(&expected, 1_048_576)
        .expect("launch should succeed");

    let calls = calls.borrow();
    assert_eq!(
        *calls,
        vec![
            Call::InitLog(4),
            Call::VmmBuilderNew,
            Call::VmmBuilderVcpus(VMM_BUILDER, 2),
            Call::VmmBuilderRamMib(VMM_BUILDER + 1, 4096),
            Call::MmioDeviceManagerNew,
            Call::CheckNestedVirt,
            Call::VmmBuilderNestedVirt(VMM_BUILDER + 2, true),
            Call::ConsoleDeviceBuilder,
            Call::ConsoleBuilderAddDefaultConsole(CONSOLE_BUILDER_BASE, 0, 1, 2),
            Call::ConsoleBuilderBuild(CONSOLE_BUILDER_BASE),
            Call::MmioDeviceManagerAdd(DEVICES, CONSOLE_DEVICE_BASE),
            Call::FsOverlayNew,
            Call::PayloadLoadKrunfw,
            Call::InitConfigBuilder,
            Call::InitBuilderArgs(
                INIT_BUILDER,
                vec![
                    "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
                    "enter".to_owned(),
                    "fish".to_owned(),
                    "-l".to_owned(),
                ]
            ),
            Call::InitBuilderEnv(INIT_BUILDER, expected_env),
            Call::InitBuilderWorkdir(INIT_BUILDER, "/workspace".to_owned()),
            Call::InitBuilderRlimits(
                INIT_BUILDER,
                vec![format!("{}=1048576:1048576", libc::RLIMIT_NOFILE)]
            ),
            Call::InitBuilderBuild(INIT_BUILDER),
            Call::InitConfigApplyIn(INIT_CONFIG, OVERLAY, PAYLOAD),
            Call::FsDeviceNew("/dev/root".to_owned(), "/rootfs".to_owned(), false),
            Call::FsDeviceSetOverlay(ROOTFS, OVERLAY),
            Call::MmioDeviceManagerAdd(DEVICES, ROOTFS),
            Call::BlockDeviceNew(
                BLOCK_DEVICE_BASE,
                "cang-nix".to_owned(),
                "/state/cang-nix.raw".to_owned(),
                false,
            ),
            Call::MmioDeviceManagerAdd(DEVICES, BLOCK_DEVICE_BASE),
            Call::BlockDeviceNew(
                BLOCK_DEVICE_BASE + 1,
                "cang-containers".to_owned(),
                "/state/cang-containers.raw".to_owned(),
                false,
            ),
            Call::MmioDeviceManagerAdd(DEVICES, BLOCK_DEVICE_BASE + 1),
            Call::VsockDeviceNew(3, 1),
            Call::MmioDeviceManagerAdd(DEVICES, VSOCK),
            Call::VmmBuilderPayload(VMM_BUILDER + 3, PAYLOAD),
            Call::VmmBuilderDevices(VMM_BUILDER + 4, DEVICES),
            Call::VmmBuilderBuild(VMM_BUILDER + 5),
            Call::VmmRun(VMM),
        ],
        "the launcher must build payload/devices/init and then run, with no port map and no per-bind virtiofs device"
    );
}

#[test]
fn init_config_carries_the_entrypoint_argv_env_workdir_and_rlimits() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let expected = config();
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_with_host_nofile_hard_limit(&expected, 2_097_152)
        .expect("launch should succeed");

    let calls = calls.borrow();
    let args = calls
        .iter()
        .find_map(|call| match call {
            Call::InitBuilderArgs(_, args) => Some(args.clone()),
            _ => None,
        })
        .expect("init config should carry argv");
    assert_eq!(
        args,
        vec![
            "/nix/store/hash-cang/bin/cang-guest-init".to_owned(),
            "enter".to_owned(),
            "fish".to_owned(),
            "-l".to_owned(),
        ],
        "the guest init execs argv[0], so argv[0] must be the entrypoint"
    );

    let env = calls
        .iter()
        .find_map(|call| match call {
            Call::InitBuilderEnv(_, env) => Some(env.clone()),
            _ => None,
        })
        .expect("init config should carry env");
    let mut expected_env = expected.env.clone();
    expected_env.extend(expected.guest_config_env.iter().cloned());
    assert_eq!(
        env, expected_env,
        "the init config must carry the cang config pointer plus every guest env var"
    );
    assert!(
        env.iter().any(|(key, _)| key == "CANG_HOST_UID"),
        "the entrypoint needs the guest identity env: {env:?}"
    );

    assert_eq!(
        calls
            .iter()
            .find_map(|call| match call {
                Call::InitBuilderWorkdir(_, workdir) => Some(workdir.clone()),
                _ => None,
            })
            .as_deref(),
        Some("/workspace")
    );
    let rlimits = calls
        .iter()
        .find_map(|call| match call {
            Call::InitBuilderRlimits(_, rlimits) => Some(rlimits.clone()),
            _ => None,
        })
        .expect("init config should carry rlimits");
    assert_eq!(
        rlimits,
        vec![guest_nofile_rlimit_entry(2_097_152)],
        "the guest nofile floor must reach the init config"
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
    let gpu_config = LaunchConfig {
        gpu_mode: GpuMode::Drm,
        ..config()
    };
    let result = DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&gpu_config)
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
        .position(|call| matches!(call, Call::GpuDeviceNew(..)))
        .expect("drm GPU mode should create a GPU device");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");

    assert_eq!(
        calls[gpu_index],
        Call::GpuDeviceNew(
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
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::MmioDeviceManagerAdd(DEVICES, 10))),
        "the GPU device must be attached to the device manager"
    );
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
        "krun_vmm_builder_nested_virt",
    ))
    .start_enter(&config())
    .expect_err("nested virt setup failure should fail");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_vmm_builder_nested_virt"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
}

#[test]
fn nested_virt_check_unsupported_or_failing_still_sets_nested_virt() {
    for nested_check in [
        NestedCheck::Unsupported,
        NestedCheck::Failing,
        NestedCheck::Supported,
    ] {
        let calls = Rc::new(RefCell::new(Vec::new()));
        DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()).nested_check(nested_check))
            .start_enter(&config())
            .expect("launch should continue after a non-fatal nested check diagnostic");

        let calls = calls.borrow();
        let check_index = calls
            .iter()
            .position(|call| matches!(call, Call::CheckNestedVirt))
            .expect("nested support check should be attempted");
        let set_index = calls
            .iter()
            .position(|call| matches!(call, Call::VmmBuilderNestedVirt(_, true)))
            .expect("nested virt should still be requested");
        let start_index = calls
            .iter()
            .position(|call| matches!(call, Call::VmmRun(..)))
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
    let mut pulse_config = config();
    pulse_config.pulse_bridge = Some(PulseBridgeConfig {
        socket: socket.clone(),
        guest_port: 50_429,
        host_port: 4714,
    });

    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&pulse_config)
        .expect("Pulse bridge launch should succeed");

    assert!(calls.borrow().contains(&Call::VsockDeviceAddUnixPort(
        VSOCK,
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
    assert!(calls.contains(&Call::VsockDeviceAddUnixPort(
        VSOCK,
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
    assert!(calls.contains(&Call::VsockDeviceAddUnixPort(
        VSOCK,
        50_427,
        waypipe_socket.display().to_string(),
        false,
    )));
    assert!(calls.contains(&Call::VsockDeviceAddUnixPort(
        VSOCK,
        50_426,
        attach_socket.display().to_string(),
        true,
    )));
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::VsockDeviceAddUnixPort(..)))
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
        "krun_vsock_device_add_unix_port",
    ))
    .start_enter(&waypipe_config(&socket))
    .expect_err("waypipe launch should fail when libkrun rejects vsock registration");

    assert!(
        format!("{err:#}")
            .contains("libkrun setup failed: krun_vsock_device_add_unix_port Waypipe"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
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
    let vsock_index = calls
        .iter()
        .position(|call| matches!(call, Call::VsockDeviceAddUnixPort(..)))
        .expect("managed launch should add vsock port");
    let console_index = calls
        .iter()
        .position(|call| matches!(call, Call::ConsoleBuilderAddDefaultConsole(..)))
        .expect("console should be configured");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");

    assert_eq!(
        calls[vsock_index],
        Call::VsockDeviceAddUnixPort(VSOCK, 50_426, socket.display().to_string(), true)
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::VsockDeviceAddUnixPort(..)))
            .count(),
        1
    );
    // ABI 2 builds the console while the init config is still being assembled and
    // the vsock device with the rest of the host channels; both precede the run.
    assert!(console_index < vsock_index);
    assert!(console_index < start_index);
    assert!(vsock_index < start_index);
}

#[test]
fn managed_session_routes_the_kernel_console_to_a_file() {
    let (_dir, socket) = managed_attach_socket_path();
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&managed_config(&socket))
        .expect("managed launch should succeed");

    let console_log = managed_console_log_path(&socket);
    let calls = calls.borrow();
    let kernel_console = calls
        .iter()
        .find_map(|call| match call {
            Call::ConsoleBuilderAddInoutPort(_, name, input, output) => {
                Some((name.clone(), *input, *output))
            }
            _ => None,
        })
        .expect("a managed launch should build the leading kernel console device");
    assert_eq!(kernel_console.0, "", "port 0 is the kernel console (hvc0)");
    assert_eq!(
        kernel_console.1, None,
        "the kernel console has no host input"
    );
    let output_fd = kernel_console
        .2
        .expect("the kernel console writes to the captured file");
    assert!(output_fd >= 0, "the console file must be open");
    assert!(
        console_log.exists(),
        "the launcher must create the managed kernel console log at {}",
        console_log.display()
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::ConsoleBuilderAddDefaultConsole(_, 0, 1, 2))),
        "the worker stdio console must still be present"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::ConsoleBuilderAddInoutPort(..)))
            .count(),
        1,
        "exactly one extra console device is the kernel console"
    );
}

#[test]
fn managed_session_vsock_registration_failure_is_setup_failure_and_frees_context() {
    let (_dir, socket) = managed_attach_socket_path();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_vsock_device_add_unix_port",
    ))
    .start_enter(&managed_config(&socket))
    .expect_err("managed launch should fail when libkrun rejects vsock registration");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_vsock_device_add_unix_port"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VsockDeviceAddUnixPort(..)))
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..))),
        "managed vsock registration setup failure should free the vmm builder"
    );
    assert!(
        !calls.iter().any(|call| matches!(call, Call::VmmRun(..))),
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
        .position(|call| matches!(call, Call::NetDeviceNewUnixstreamFd(..)))
        .expect("passt mode should add a net unixstream device");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");

    assert_eq!(
        calls[net_index],
        Call::NetDeviceNewUnixstreamFd(42, CANG_LIBKRUN_COMPAT_NET_FEATURES, 0,)
    );
    assert!(net_index < start_index);
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::MmioDeviceManagerAdd(DEVICES, NET_DEVICE))),
        "the net device must be attached"
    );
}

#[test]
fn passt_mode_requests_dhcp_through_the_init_config() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&passt_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, Call::InitBuilderDhcp(_, true)))
            .count(),
        1,
        "ABI 2's net device ignores the old DHCP flag, so the init config must ask for DHCP: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::InitBuilderDhcp(_, false))),
        "a TSI-mode launch must not request DHCP"
    );
}

#[test]
fn tsi_mode_does_not_request_dhcp() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&config())
        .expect("launch should succeed");

    assert!(
        !calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::InitBuilderDhcp(..)))
    );
}

#[test]
fn tsi_publish_adds_vsock_port_forwards_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&tsi_publish_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    let forwards = calls
        .iter()
        .filter_map(|call| match call {
            Call::VsockDeviceAddPortForward(device, mapping) => Some((*device, mapping.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        forwards,
        vec![
            (VSOCK, "80:8080".to_owned()),
            (VSOCK, "443:8443".to_owned()),
        ],
        "ABI 2 spells a port forward guest:host, the reverse of the v1 port map"
    );
    let last_forward = calls
        .iter()
        .rposition(|call| matches!(call, Call::VsockDeviceAddPortForward(..)))
        .expect("TSI publish should add port forwards");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");
    assert!(last_forward < start_index);
}

#[test]
fn passt_publish_does_not_add_port_forwards() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&passt_publish_config())
        .expect("launch should succeed");

    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::NetDeviceNewUnixstreamFd(..)))
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::VsockDeviceAddPortForward(..)))
    );
}

#[test]
fn pre_enter_hook_runs_after_setup_and_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_with_pre_enter_hook(&config(), || {
            let calls = calls.borrow();
            assert!(
                calls
                    .iter()
                    .any(|call| matches!(call, Call::InitBuilderArgs(..)))
            );
            assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
            Ok(())
        })
        .expect("launch should succeed");

    let calls = calls.borrow();
    let init_index = calls
        .iter()
        .position(|call| matches!(call, Call::InitBuilderArgs(..)))
        .expect("setup should configure the init config before the hook");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start after the hook");
    assert!(init_index < start_index);
}

#[test]
fn audit_start_marker_runs_immediately_before_vmm_build() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut audit_config = config();
    audit_config.seccomp = SeccompMode::Audit(AuditMode::Full {
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
                .start_enter_with_pre_enter_hook(&audit_config, || {
                    calls.borrow_mut().push(Call::PreEnterHook);
                    Ok(())
                })
        },
    )
    .expect("launch should succeed");

    let calls = calls.borrow();
    let init_index = calls
        .iter()
        .position(|call| matches!(call, Call::InitBuilderArgs(..)))
        .expect("setup should configure the init config");
    let pre_enter_index = calls
        .iter()
        .position(|call| matches!(call, Call::PreEnterHook))
        .expect("pre-enter hook should run");
    let marker_index = calls
        .iter()
        .position(|call| matches!(call, Call::AuditStartMarker))
        .expect("audit start marker should run");
    let build_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmBuilderBuild(..)))
        .expect("vmm builder should build");
    assert!(init_index < pre_enter_index);
    assert!(pre_enter_index < marker_index);
    assert_eq!(
        marker_index + 1,
        build_index,
        "the marker must precede all VMM construction, as it preceded krun_start_enter"
    );
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
fn profile_setup_runs_after_init_config_and_before_pre_enter_hook() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter_profiled_with_pre_enter_hook(
            &config(),
            Some(Path::new("/tmp/vm-worker-host-profile.tsv")),
            || {
                let calls = calls.borrow();
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::InitBuilderArgs(..)))
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::VmmBuilderNestedVirt(_, true)))
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::SetProfilePath(..)))
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| matches!(call, Call::PayloadAppendCmdline(..)))
                );
                assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
                Ok(())
            },
        )
        .expect("launch should succeed");

    let calls = calls.borrow();
    let nested_set_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmBuilderNestedVirt(_, true)))
        .expect("nested virt should be configured");
    let init_index = calls
        .iter()
        .position(|call| matches!(call, Call::InitBuilderArgs(..)))
        .expect("setup should configure the init config");
    let set_profile_index = calls
        .iter()
        .position(|call| matches!(call, Call::SetProfilePath(..)))
        .expect("profile path should be configured");
    let set_cmdline_index = calls
        .iter()
        .position(|call| matches!(call, Call::PayloadAppendCmdline(..)))
        .expect("profile cmdline diagnostics should be configured");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");

    assert_eq!(
        calls[set_profile_index],
        Call::SetProfilePath(
            VMM_BUILDER + 3,
            "/tmp/vm-worker-host-profile.tsv".to_owned()
        )
    );
    assert_eq!(
        calls[set_cmdline_index],
        Call::PayloadAppendCmdline(PAYLOAD, PROFILE_KERNEL_CMDLINE_APPEND.to_owned())
    );
    assert!(nested_set_index < init_index);
    assert!(init_index < set_profile_index);
    assert!(set_profile_index < set_cmdline_index);
    assert!(set_cmdline_index < start_index);
}

#[test]
fn passt_mode_requires_prepared_socket_fd() {
    let mut missing_fd = config();
    missing_fd.network_mode = NetworkMode::Passt;
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::new(calls.clone()))
        .start_enter(&missing_fd)
        .expect_err("missing fd should fail setup");

    assert!(format!("{err:#}").contains("prepared passt socket fd"));
    assert!(
        calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
}

#[test]
fn setup_failure_is_classified_and_frees_context_before_start() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err =
        DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_fs_device_new"))
            .start_enter(&config())
            .expect_err("setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed"));
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
}

#[test]
fn port_forward_failure_is_setup_failure_and_frees_context() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_vsock_device_add_port_forward",
    ))
    .start_enter(&tsi_publish_config())
    .expect_err("port-forward setup failure should fail");

    assert!(
        format!("{err:#}").contains("libkrun setup failed: krun_vsock_device_add_port_forward"),
        "unexpected error: {err:#}"
    );
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
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
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
}

#[test]
fn console_registration_failure_frees_context_before_exec() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(
        calls.clone(),
        "krun_console_builder_add_default_console",
    ))
    .start_enter(&config())
    .expect_err("console setup failure should fail");

    assert!(format!("{err:#}").contains("libkrun setup failed"));
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, Call::VmmBuilderDestroy(..)))
    );
    assert!(
        !calls
            .iter()
            .any(|call| matches!(call, Call::InitBuilderArgs(..)))
    );
    assert!(!calls.iter().any(|call| matches!(call, Call::VmmRun(..))));
}

#[test]
fn vmm_run_return_is_classified_as_start_failure() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let err = DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_vmm_run"))
        .start_enter(&config())
        .expect_err("a returning vmm run should fail");

    assert!(format!("{err:#}").contains("libkrun start failed"));
    assert!(
        calls
            .borrow()
            .iter()
            .any(|call| matches!(call, Call::VmmRun(..)))
    );
}

#[test]
fn audit_start_marker_runs_before_a_returning_vmm_run_is_classified() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut audit_config = config();
    audit_config.seccomp = SeccompMode::Audit(AuditMode::Full {
        trace_path: PathBuf::from("/tmp/cang-seccomp-audit.jsonl"),
    });

    let marker_calls = calls.clone();
    let err = with_audit_start_marker_hook_for_test(
        move || {
            marker_calls.borrow_mut().push(Call::AuditStartMarker);
            Ok(())
        },
        || {
            DirectLibkrunLauncher::new(FakeLibkrunApi::failing(calls.clone(), "krun_vmm_run"))
                .start_enter(&audit_config)
        },
    )
    .expect_err("a returning vmm run should fail after the start marker");

    assert!(format!("{err:#}").contains("libkrun start failed"));
    let calls = calls.borrow();
    let marker_index = calls
        .iter()
        .position(|call| matches!(call, Call::AuditStartMarker))
        .expect("audit start marker should run");
    let start_index = calls
        .iter()
        .position(|call| matches!(call, Call::VmmRun(..)))
        .expect("launch should start");
    assert!(marker_index < start_index);
}
