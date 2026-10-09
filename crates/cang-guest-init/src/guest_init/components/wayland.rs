use anyhow::{Context, Result, anyhow};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::components::rootless::runtime_dir::ensure_user_runtime_dir;
use crate::guest_init::fs as guest_fs;
use crate::guest_init::process;

pub(in crate::guest_init) const WAYLAND_DISPLAY: &str = "wayland-0";
pub(in crate::guest_init) const PROXY_BIN: &str = "wl-cross-domain-proxy";
// The venus ICD is pinned through VK_ICD_FILENAMES rather than
// VK_DRIVER_FILES on purpose. ANGLE's SwiftShader display selects its own
// software ICD by setting VK_ICD_FILENAMES, and the Vulkan loader gives
// VK_DRIVER_FILES precedence over it, so pinning VK_DRIVER_FILES replaces that
// choice instead of leaving it open: a guest chromium asked for
// --use-angle=swiftshader then finds no software device, aborts its GPU
// process and reports no WebGL renderer, while the hardware venus path is
// unaffected either way. Verified in a --gpu=drm guest: with VK_ICD_FILENAMES
// the venus renderer stays the default and --use-angle=swiftshader reaches
// SwiftShader with zero GPU-process crashes.
const MESA_ENV: &[(&str, &str)] = &[
    // The native-EGL GL backend of an EGL client dlopens `libEGL.so.1` by
    // soname. That file is glvnd's *dispatcher*: mesa ships only the vendor
    // library (`libEGL_mesa.so.0`) that the dispatcher loads through
    // `__EGL_VENDOR_LIBRARY_FILENAMES`. The image exposes libglvnd at a stable
    // path because nothing else carries the dispatcher - the guest's `/lib` is
    // a merged symlink tree, but its glibc searches only its own store lib
    // dir - so without this an EGL client's native display cannot be created
    // (chromium's ANGLE: "Could not dlopen native EGL: libEGL.so.1") and the
    // browser falls back to `--use-gl=disabled` after its GPU-process crashes.
    ("LD_LIBRARY_PATH", "/usr/lib/cang-gpu-runtime/lib"),
    ("LIBGL_DRIVERS_PATH", "/usr/lib/cang-mesa-runtime/lib/dri"),
    // libva searches only /run/opengl-driver/lib/dri and /usr/lib*/dri by
    // default; the driver it needs (virtio_gpu_drv_video.so, the guest side of
    // vrend's VA-API video) lives in the mesa runtime directory, so without
    // this a VA-API client fails in va_openDriver() with no driver found.
    //
    // The VA runtime comes first on purpose. Both directories provide
    // virtio_gpu_drv_video.so, but only the one in the VA runtime is built from
    // mesa source and carries the guest half of the vrend encode fix; the mesa
    // runtime's copy (the image's prebuilt mesa, which also serves GL and
    // Vulkan) is the fallback for anything that resolves the driver after the
    // VA runtime directory is gone.
    (
        "LIBVA_DRIVERS_PATH",
        "/usr/lib/cang-va-runtime/dri:/usr/lib/cang-mesa-runtime/lib/dri",
    ),
    (
        "__EGL_VENDOR_LIBRARY_FILENAMES",
        "/usr/lib/cang-mesa-runtime/share/glvnd/egl_vendor.d/50_mesa.json",
    ),
    (
        "VK_ICD_FILENAMES",
        "/usr/lib/cang-mesa-runtime/share/vulkan/icd.d/virtio_icd.x86_64.json",
    ),
    // libgbm loads its backend (dri_gbm.so) from GBM_BACKENDS_PATH; without it
    // mesa searches the path its build was configured with
    // (`/run/opengl-driver/lib/gbm`, a host path that does not exist in the
    // guest), fails with `MESA-LOADER: failed to open dri`, and every client
    // falls back to wl_shm - so a Waypipe guest never presents GPU buffers.
    ("GBM_BACKENDS_PATH", "/usr/lib/cang-mesa-runtime/lib/gbm"),
];
const DRI_DIR: &str = "/dev/dri";
const ROOT_UID: u32 = 0;
const VIDEO_GID: u32 = 44;
const RENDER_GID: u32 = 107;
// `<linux/udmabuf.h>`'s device node. devtmpfs creates it root-only, and the
// guest proxy runs as the task user, so a GPU guest has to open the mode up or
// the proxy's `/dev/udmabuf` probe fails and every `wl_shm` pool takes the
// copy path instead of PR 822's zero-copy one.
const UDMABUF_PATH: &str = "/dev/udmabuf";
const UDMABUF_MODE: u32 = 0o666;

pub(in crate::guest_init) fn export_mesa_if_enabled(enabled: bool) {
    if !enabled {
        return;
    }

    for (name, value) in MESA_ENV {
        unsafe { std::env::set_var(name, value) };
    }
}

pub(in crate::guest_init) fn start_if_enabled(
    wayland_enabled: bool,
    gpu_drm_enabled: bool,
    identity: &DevIdentity,
) -> Result<Option<Child>> {
    prepare_drm_devices_for_start(wayland_enabled, gpu_drm_enabled, Path::new(DRI_DIR))?;
    prepare_udmabuf_for_start(wayland_enabled, gpu_drm_enabled, Path::new(UDMABUF_PATH))?;
    if !wayland_enabled {
        return Ok(None);
    }
    let runtime_dir = ensure_user_runtime_dir(identity)?;
    export_guest_env(&runtime_dir);
    let child = spawn_proxy(&runtime_dir, WAYLAND_DISPLAY, identity)
        .context("failed to start guest Wayland cross-domain proxy")?;
    Ok(Some(child))
}

fn prepare_drm_devices_for_start(
    wayland_enabled: bool,
    gpu_drm_enabled: bool,
    dri_dir: &Path,
) -> Result<()> {
    prepare_drm_devices_for_start_with(
        wayland_enabled,
        gpu_drm_enabled,
        dri_dir,
        &mut |path, permissions| {
            guest_fs::chown(path, ROOT_UID, permissions.gid)?;
            guest_fs::chmod(path, permissions.mode)
        },
    )
}

fn prepare_drm_devices_for_start_with(
    wayland_enabled: bool,
    gpu_drm_enabled: bool,
    dri_dir: &Path,
    apply: &mut impl FnMut(&Path, DrmDevicePermissions) -> Result<()>,
) -> Result<()> {
    if wayland_enabled || gpu_drm_enabled {
        prepare_drm_devices_under_with(dri_dir, apply)?;
    }
    Ok(())
}

fn prepare_udmabuf_for_start(
    wayland_enabled: bool,
    gpu_drm_enabled: bool,
    path: &Path,
) -> Result<()> {
    prepare_udmabuf_for_start_with(wayland_enabled, gpu_drm_enabled, path, &mut |path, mode| {
        guest_fs::chmod(path, mode)
    })
}

fn prepare_udmabuf_for_start_with(
    wayland_enabled: bool,
    gpu_drm_enabled: bool,
    path: &Path,
    apply: &mut impl FnMut(&Path, u32) -> Result<()>,
) -> Result<()> {
    // Only a GPU guest has anything to do with the node, and a kernel built
    // without `CONFIG_UDMABUF` has none to open.
    if (wayland_enabled || gpu_drm_enabled) && path.exists() {
        apply(path, UDMABUF_MODE)?;
    }
    Ok(())
}

fn export_guest_env(runtime_dir: &Path) {
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", runtime_dir);
        std::env::set_var("WAYLAND_DISPLAY", WAYLAND_DISPLAY);
    }
}

fn spawn_proxy(runtime_dir: &Path, wayland_display: &str, identity: &DevIdentity) -> Result<Child> {
    let socket_path = runtime_dir.join(wayland_display);
    if socket_path.exists() {
        fs::remove_file(&socket_path).with_context(|| {
            format!(
                "failed to remove stale guest Wayland socket '{}'",
                socket_path.display()
            )
        })?;
    }
    spawn_proxy_command(runtime_dir, wayland_display, identity, process::uid())
        .spawn()
        .with_context(|| anyhow!("failed to spawn {PROXY_BIN}"))
}

fn proxy_credential_plan(
    starting_uid: u32,
    identity: &DevIdentity,
) -> Option<[process::CredentialOperation; 3]> {
    (starting_uid == ROOT_UID).then(|| process::credential_plan(identity))
}

fn spawn_proxy_command(
    runtime_dir: &Path,
    wayland_display: &str,
    identity: &DevIdentity,
    starting_uid: u32,
) -> Command {
    let mut command = Command::new(PROXY_BIN);
    command
        .arg("--socket-name")
        .arg(wayland_display)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("WAYLAND_DISPLAY", wayland_display)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if proxy_credential_plan(starting_uid, identity).is_some() {
        let identity = identity.clone();
        unsafe {
            command.pre_exec(move || process::apply_dev_credentials(&identity, Default::default()));
        }
    }
    command
}

fn prepare_drm_devices_under_with(
    dri_dir: &Path,
    apply: &mut impl FnMut(&Path, DrmDevicePermissions) -> Result<()>,
) -> Result<()> {
    let Ok(entries) = fs::read_dir(dri_dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", dri_dir.display()))?;
        let path = entry.path();
        if let Some(permissions) = drm_device_permissions(&path) {
            apply(&path, permissions)?;
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct DrmDevicePermissions {
    gid: u32,
    mode: u32,
}

fn drm_device_permissions(path: &Path) -> Option<DrmDevicePermissions> {
    let name = path.file_name()?.to_str()?;
    if name.starts_with("renderD") {
        Some(DrmDevicePermissions {
            gid: RENDER_GID,
            mode: 0o666,
        })
    } else if name.starts_with("card") {
        Some(DrmDevicePermissions {
            gid: VIDEO_GID,
            mode: 0o660,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn drm_mode_exports_hardware_mesa_discovery_without_forcing_software() {
        let _guard = ENV_LOCK.lock().expect("env test lock");
        unsafe {
            std::env::remove_var("LIBGL_ALWAYS_SOFTWARE");
            for (name, _) in MESA_ENV {
                std::env::remove_var(name);
            }
        }

        export_mesa_if_enabled(true);

        for (name, value) in MESA_ENV {
            assert_eq!(std::env::var(name).as_deref(), Ok(*value));
            // Everything points into the image's mesa runtime, except the VA
            // driver search path, which starts at the patched VA runtime and
            // the loader path, which points at the image's libglvnd symlink.
            assert!(
                value.starts_with("/usr/lib/cang-mesa-runtime")
                    || value.starts_with("/usr/lib/cang-va-runtime")
                    || value.starts_with("/usr/lib/cang-gpu-runtime")
            );
        }
        // The ICD pin must use VK_ICD_FILENAMES and never VK_DRIVER_FILES: the
        // loader gives VK_DRIVER_FILES precedence over the VK_ICD_FILENAMES that
        // ANGLE's SwiftShader display sets for itself, so pinning VK_DRIVER_FILES
        // would strip a guest chromium asked for --use-angle=swiftshader of its
        // software device (GPU process abort, no WebGL renderer).
        assert!(MESA_ENV.iter().any(|(name, _)| *name == "VK_ICD_FILENAMES"));
        assert!(!MESA_ENV.iter().any(|(name, _)| *name == "VK_DRIVER_FILES"));
        // libgbm must be told where the image's mesa keeps its backend: the
        // prebuilt runtime's compiled-in default is /run/opengl-driver/lib/gbm,
        // a host path that does not exist in the guest, and every client that
        // allocates a GBM buffer (chromium's Wayland presentation, Waypipe's
        // dmabuf transport) otherwise falls back to wl_shm.
        let gbm = MESA_ENV
            .iter()
            .find(|(name, _)| *name == "GBM_BACKENDS_PATH")
            .expect("GBM_BACKENDS_PATH in MESA_ENV")
            .1;
        assert_eq!(gbm, "/usr/lib/cang-mesa-runtime/lib/gbm");
        // VA-API needs both the driver directory (LIBVA_DRIVERS_PATH, since
        // libva has no default path for the mesa runtime) and the DRM node,
        // which is why this rides with the --gpu=drm env block.
        // The VA driver has to be the patched one, so the VA runtime directory
        // comes before the mesa runtime directory in the search path.
        let va_paths = MESA_ENV
            .iter()
            .find(|(name, _)| *name == "LIBVA_DRIVERS_PATH")
            .expect("LIBVA_DRIVERS_PATH in MESA_ENV")
            .1;
        assert_eq!(
            va_paths,
            "/usr/lib/cang-va-runtime/dri:/usr/lib/cang-mesa-runtime/lib/dri"
        );
        // Native EGL needs glvnd's dispatcher on the loader path: mesa ships
        // only the vendor library (`libEGL_mesa.so.0`), the guest glibc's only
        // default search directory is its own store lib dir, and an EGL client
        // (chromium's ANGLE) dlopens `libEGL.so.1` by soname. The value is the
        // image's stable libglvnd symlink, never a store hash.
        let loader_path = MESA_ENV
            .iter()
            .find(|(name, _)| *name == "LD_LIBRARY_PATH")
            .expect("LD_LIBRARY_PATH in MESA_ENV")
            .1;
        assert_eq!(loader_path, "/usr/lib/cang-gpu-runtime/lib");
        assert!(std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none());

        for (name, _) in MESA_ENV {
            unsafe { std::env::remove_var(name) };
        }
    }

    #[test]
    fn disabled_drm_mode_does_not_export_mesa_discovery() {
        let _guard = ENV_LOCK.lock().expect("env test lock");
        for (name, _) in MESA_ENV {
            unsafe { std::env::remove_var(name) };
        }

        export_mesa_if_enabled(false);

        for (name, _) in MESA_ENV {
            assert!(std::env::var_os(name).is_none());
        }
    }
    #[test]
    fn proxy_constants_match_guest_env_contract() {
        assert_eq!(WAYLAND_DISPLAY, "wayland-0");
        assert_eq!(PROXY_BIN, "wl-cross-domain-proxy");
    }

    #[test]
    fn proxy_command_forces_exported_socket_name_and_dev_credentials() {
        let identity = DevIdentity::new(1000, 1000, "/bin/sh".into());
        let command = spawn_proxy_command(
            Path::new("/run/user/1000"),
            WAYLAND_DISPLAY,
            &identity,
            ROOT_UID,
        );

        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--socket-name", WAYLAND_DISPLAY]);
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new("XDG_RUNTIME_DIR")),
            Some((
                std::ffi::OsStr::new("XDG_RUNTIME_DIR"),
                Some(std::ffi::OsStr::new("/run/user/1000"))
            ))
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new("WAYLAND_DISPLAY")),
            Some((
                std::ffi::OsStr::new("WAYLAND_DISPLAY"),
                Some(std::ffi::OsStr::new(WAYLAND_DISPLAY))
            ))
        );
        assert_eq!(
            proxy_credential_plan(ROOT_UID, &identity),
            Some(process::credential_plan(&identity))
        );
        assert_eq!(proxy_credential_plan(identity.uid, &identity), None);
    }

    #[test]
    fn drm_preparation_runs_without_wayland_when_gpu_drm_is_enabled() {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let card = temp.path().join("card0");
        let render = temp.path().join("renderD128");
        let unrelated = temp.path().join("by-path");
        fs::write(&card, "").expect("card node should be created");
        fs::write(&render, "").expect("render node should be created");
        fs::write(&unrelated, "").expect("unrelated entry should be created");
        let mut applied = Vec::new();

        prepare_drm_devices_for_start_with(false, true, temp.path(), &mut |path, permissions| {
            applied.push((path.to_path_buf(), permissions));
            Ok(())
        })
        .expect("DRM preparation should succeed without Wayland");

        applied.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            applied,
            [
                (
                    card,
                    DrmDevicePermissions {
                        gid: VIDEO_GID,
                        mode: 0o660,
                    },
                ),
                (
                    render,
                    DrmDevicePermissions {
                        gid: RENDER_GID,
                        mode: 0o666,
                    },
                ),
            ]
        );
    }

    #[test]
    fn udmabuf_preparation_opens_the_node_for_gpu_guests_only() {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let node = temp.path().join("udmabuf");
        fs::write(&node, "").expect("udmabuf node should be created");
        let mut applied = Vec::new();

        prepare_udmabuf_for_start_with(true, false, &node, &mut |path, mode| {
            applied.push((path.to_path_buf(), mode));
            Ok(())
        })
        .expect("udmabuf preparation should succeed");
        prepare_udmabuf_for_start_with(false, true, &node, &mut |path, mode| {
            applied.push((path.to_path_buf(), mode));
            Ok(())
        })
        .expect("udmabuf preparation should succeed with --gpu=drm alone");

        assert_eq!(
            applied,
            [(node.clone(), UDMABUF_MODE), (node.clone(), UDMABUF_MODE)]
        );

        let mut applied = Vec::new();
        prepare_udmabuf_for_start_with(false, false, &node, &mut |path, mode| {
            applied.push((path.to_path_buf(), mode));
            Ok(())
        })
        .expect("udmabuf preparation should succeed without a GPU");
        assert!(
            applied.is_empty(),
            "a non-GPU guest must not touch the node"
        );
    }

    #[test]
    fn udmabuf_preparation_tolerates_a_kernel_without_the_node() {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let missing = temp.path().join("udmabuf");
        let mut applied = Vec::new();

        prepare_udmabuf_for_start_with(true, true, &missing, &mut |path, mode| {
            applied.push((path.to_path_buf(), mode));
            Ok(())
        })
        .expect("a missing udmabuf node is not an error");

        assert!(applied.is_empty());
    }

    #[test]
    fn drm_device_permissions_match_libkrun_gpu_nodes() {
        assert_eq!(
            drm_device_permissions(Path::new("/dev/dri/renderD128")),
            Some(DrmDevicePermissions {
                gid: RENDER_GID,
                mode: 0o666,
            })
        );
        assert_eq!(
            drm_device_permissions(Path::new("/dev/dri/card0")),
            Some(DrmDevicePermissions {
                gid: VIDEO_GID,
                mode: 0o660,
            })
        );
        assert_eq!(drm_device_permissions(Path::new("/dev/dri/by-path")), None);
    }
}
