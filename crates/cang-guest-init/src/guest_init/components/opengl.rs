//! The guest's NixOS-conventional graphics layout (`/run/opengl-driver`).
//!
//! nixpkgs builds its graphics stack against the NixOS convention that
//! `/run/opengl-driver` is a symlink farm into whatever mesa/libglvnd the
//! system provides. Parts of that stack name paths under it at build time and
//! consult them with no environment help: libva's compiled-in driver search
//! (`/run/opengl-driver/lib/dri`) and libgbm's compiled-in backend search
//! (`/run/opengl-driver/lib/gbm`) are the two cang has already had to work
//! around with `LIBVA_DRIVERS_PATH` / `GBM_BACKENDS_PATH`. The EGL vendor and
//! Vulkan ICD manifests live at `share/` under the same root for a client that
//! was built without the per-client env pins. A nixpkgs binary or library run
//! in the cang guest therefore looks there, and without the farm it finds
//! nothing - which is why cang currently has to hand every client an explicit
//! env var through guest-init's `MESA_ENV`. The explicit env stays
//! authoritative (it is what cang intends, visible without guessing from the
//! farm); this module adds the conventional layout underneath it so an
//! env-less client resolves the same way a NixOS one does.
//!
//! The farm is symlinks only, into the image's already-published runtime
//! directories:
//!
//! ```text
//! /run/opengl-driver/lib/            <- flat libs (glvnd dispatcher + mesa)
//! /run/opengl-driver/lib/dri/        <- mesa DRI drivers + the PATCHED VA driver
//! /run/opengl-driver/lib/gbm/        <- mesa's GBM backends
//! /run/opengl-driver/share/glvnd/egl_vendor.d/
//! /run/opengl-driver/share/vulkan/icd.d/
//! ```
//!
//! The one entry that must not be a plain mirror is `lib/dri`: libva searches
//! `/run/opengl-driver/lib/dri` by default, and both the prebuilt mesa runtime
//! and the patched VA runtime publish a `virtio_gpu_drv_video.so`. Pointing the
//! farm at mesa's copy would silently reintroduce the constant-QP encode stall
//! of `docs/wayfinder/guest-vaapi-video/tickets/08-cqp-encode-hangs.md`, so the
//! patched driver is linked last with the mesa copy deliberately excluded, and
//! an image without a patched driver leaves the entry absent rather than
//! falling back to the unpatched one.

use anyhow::{Context, Result};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use crate::guest_init::fs as guest_fs;

/// The NixOS-conventional graphics root the guest materializes.
pub(in crate::guest_init) const DRIVER_ROOT: &str = "/run/opengl-driver";

/// glvnd's dispatcher and GL entry points. mesa's output carries only the
/// *vendor* libraries (`libEGL_mesa.so.0`, `libGLX_mesa.so.0`); the sonames a
/// client dlopens (`libEGL.so.1`, ...) are glvnd's, which the image publishes
/// at `/usr/lib/cang-gpu-runtime`.
const GLVND_RUNTIME_LIB: &str = "/usr/lib/cang-gpu-runtime/lib";
/// The image's mesa runtime: the flat libraries, `dri/` and `gbm/`.
const MESA_RUNTIME_LIB: &str = "/usr/lib/cang-mesa-runtime/lib";
/// mesa's glvnd EGL vendor and Vulkan ICD manifests.
const MESA_RUNTIME_SHARE: &str = "/usr/lib/cang-mesa-runtime/share";
/// The image's VA runtime: mesa built from source with cang's guest-side
/// vrend encode patch (`nix/image/layers.nix`'s `vaApiRuntime`).
const VA_RUNTIME_DRI: &str = "/usr/lib/cang-va-runtime/dri";
/// libva asks for `<name>_drv_video.so`; the name is shared with mesa's
/// prebuilt driver, which is why the farm has to pick one deliberately.
const VA_DRIVER_FILE: &str = "virtio_gpu_drv_video.so";

/// glvnd's dispatcher plus its GL/GLX/OpenGL entry points. `libGLdispatch` is
/// the shared dispatch core the others load, so it has to be present too.
const GLVND_LIBS: &[&str] = &[
    "libEGL.so.1",
    "libGL.so.1",
    "libGLX.so.0",
    "libGLdispatch.so.0",
    "libOpenGL.so.0",
];

/// The mesa `share/` subtrees the farm mirrors. These are the discovery
/// manifests a client with no `__EGL_VENDOR_LIBRARY_FILENAMES` /
/// `VK_ICD_FILENAMES` reads through the conventional root.
const MESA_SHARE_SUBTREES: &[&str] = &["glvnd/egl_vendor.d", "vulkan/icd.d"];

/// The source directories the farm is built from. Grouped so the builder can be
/// exercised against a temporary tree without touching the image paths.
struct DriverLayoutSource<'a> {
    glvnd_lib: &'a Path,
    mesa_lib: &'a Path,
    mesa_share: &'a Path,
    va_dri: &'a Path,
}

impl DriverLayoutSource<'static> {
    /// The image's runtime directories, as published by `nix/image/container.nix`.
    fn image() -> Self {
        Self {
            glvnd_lib: Path::new(GLVND_RUNTIME_LIB),
            mesa_lib: Path::new(MESA_RUNTIME_LIB),
            mesa_share: Path::new(MESA_RUNTIME_SHARE),
            va_dri: Path::new(VA_RUNTIME_DRI),
        }
    }
}

/// Materialize `/run/opengl-driver` for a DRM (`--gpu=drm`) guest.
///
/// Idempotent: the tree is rebuilt from scratch every call, so a boot that
/// already ran it converges on the same layout (and the cost is a few dozen
/// `symlink(2)` calls). Non-DRM guests leave the root untouched - the software
/// renderer is pinned by env and must not have hardware discovery grafted under
/// it.
pub(in crate::guest_init) fn ensure_driver_layout_if_enabled(enabled: bool) -> Result<()> {
    ensure_driver_layout(
        enabled,
        Path::new(DRIVER_ROOT),
        &DriverLayoutSource::image(),
    )
}

/// The guard split out from the image paths so a test can assert that a
/// non-DRM guest never gets the conventional root.
fn ensure_driver_layout(enabled: bool, root: &Path, source: &DriverLayoutSource<'_>) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    build_driver_layout(root, source)
}

fn build_driver_layout(root: &Path, source: &DriverLayoutSource<'_>) -> Result<()> {
    reset_dir(root)?;
    let lib = root.join("lib");
    let share = root.join("share");
    guest_fs::create_dir_all(&lib)?;
    guest_fs::create_dir_all(&share)?;

    // glvnd's dispatcher and GL entry points.
    for name in GLVND_LIBS {
        link_if_present(&source.glvnd_lib.join(name), &lib.join(name))?;
    }
    // mesa's flat libraries. `dri/` and `gbm/` are directories, so they are
    // skipped here and materialized explicitly below.
    link_flat_entries(source.mesa_lib, &lib, &|_| true)?;

    // mesa's DRI drivers, with the unpatched VA driver deliberately excluded.
    let farm_dri = lib.join("dri");
    guest_fs::create_dir_all(&farm_dri)?;
    link_flat_entries(&source.mesa_lib.join("dri"), &farm_dri, &|name| {
        name != VA_DRIVER_FILE
    })?;
    // Then the patched VA driver, so libva's default search of
    // `/run/opengl-driver/lib/dri` loads the guest half of vrend's encode fix.
    // If the image carries no patched driver, the entry is left absent: an
    // unpatched fallback is worse than no driver (ticket 08's stall).
    let patched_va_driver = source.va_dri.join(VA_DRIVER_FILE);
    if patched_va_driver.exists() {
        symlink(&patched_va_driver, farm_dri.join(VA_DRIVER_FILE)).with_context(|| {
            format!(
                "failed to link patched VA driver {} -> {}",
                farm_dri.join(VA_DRIVER_FILE).display(),
                patched_va_driver.display()
            )
        })?;
    }

    // mesa's GBM backends.
    let farm_gbm = lib.join("gbm");
    guest_fs::create_dir_all(&farm_gbm)?;
    link_flat_entries(&source.mesa_lib.join("gbm"), &farm_gbm, &|_| true)?;

    // The discovery manifests.
    for subtree in MESA_SHARE_SUBTREES {
        let target = share.join(subtree);
        guest_fs::create_dir_all(&target)?;
        link_flat_entries(&source.mesa_share.join(subtree), &target, &|_| true)?;
    }

    Ok(())
}

/// Replace `root` with an empty directory, tolerating a missing one.
fn reset_dir(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", root.display()));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(root)
                .with_context(|| format!("failed to remove symlinked {}", root.display()))?;
            return Ok(());
        }
        Ok(_) => {}
    }
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to clear {}", root.display())),
    }
}

/// Symlink every non-directory entry of `source` into `target`.
///
/// A missing `source` is not an error: the farm mirrors whatever the image
/// publishes, and an image without one of the runtime directories simply gets a
/// smaller farm. Directories are skipped so `lib/dri` and `lib/gbm` can be
/// materialized as real, enumerable directories rather than links to mesa's.
fn link_flat_entries(source: &Path, target: &Path, include: &dyn Fn(&str) -> bool) -> Result<()> {
    let Ok(entries) = fs::read_dir(source) else {
        return Ok(());
    };
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", source.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !include(&name) {
            continue;
        }
        let source_path = entry.path();
        if source_path.is_dir() {
            continue;
        }
        symlink(&source_path, target.join(&name)).with_context(|| {
            format!(
                "failed to link {} -> {}",
                target.join(&name).display(),
                source_path.display()
            )
        })?;
    }
    Ok(())
}

fn link_if_present(source: &Path, target: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    symlink(source, target).with_context(|| {
        format!(
            "failed to link {} -> {}",
            target.display(),
            source.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeImage {
        _dir: tempfile::TempDir,
        glvnd_lib: PathBuf,
        mesa_lib: PathBuf,
        mesa_share: PathBuf,
        va_dri: PathBuf,
    }

    fn fake_image() -> FakeImage {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path();
        let glvnd_lib = root.join("gpu-runtime/lib");
        let mesa_lib = root.join("mesa/lib");
        let mesa_share = root.join("mesa/share");
        let va_dri = root.join("va-runtime/dri");
        for (path, body) in [
            (&glvnd_lib, "libEGL.so.1"),
            (&glvnd_lib, "libGL.so.1"),
            (&glvnd_lib, "libGLX.so.0"),
            (&glvnd_lib, "libGLdispatch.so.0"),
            (&glvnd_lib, "libOpenGL.so.0"),
            (&mesa_lib, "libEGL_mesa.so.0"),
            (&mesa_lib, "libgallium-26.1.8.so"),
        ] {
            fs::create_dir_all(path).expect("source directory should be created");
            fs::write(path.join(body), body).expect("source library should be written");
        }
        let mesa_dri = mesa_lib.join("dri");
        fs::create_dir_all(&mesa_dri).expect("mesa dri directory should be created");
        fs::write(mesa_dri.join("virtio_gpu_dri.so"), "mesa dri").expect("dri driver");
        // mesa's own (unpatched) VA driver: the farm must never expose this one.
        fs::write(mesa_dri.join(VA_DRIVER_FILE), "unpatched mesa va driver").expect("mesa va");
        let mesa_gbm = mesa_lib.join("gbm");
        fs::create_dir_all(&mesa_gbm).expect("mesa gbm directory should be created");
        fs::write(mesa_gbm.join("dri_gbm.so"), "gbm backend").expect("gbm backend");
        let egl_vendor = mesa_share.join("glvnd/egl_vendor.d");
        fs::create_dir_all(&egl_vendor).expect("egl vendor dir should be created");
        fs::write(egl_vendor.join("50_mesa.json"), "{}").expect("egl vendor manifest");
        let vulkan_icd = mesa_share.join("vulkan/icd.d");
        fs::create_dir_all(&vulkan_icd).expect("vulkan icd dir should be created");
        fs::write(vulkan_icd.join("virtio_icd.x86_64.json"), "{}").expect("icd manifest");
        // The patched VA driver, in its own runtime with its own libgallium.
        fs::create_dir_all(&va_dri).expect("va dri directory should be created");
        fs::write(va_dri.join(VA_DRIVER_FILE), "patched va driver").expect("patched va driver");

        FakeImage {
            _dir: dir,
            glvnd_lib,
            mesa_lib,
            mesa_share,
            va_dri,
        }
    }

    fn source(image: &FakeImage) -> DriverLayoutSource<'_> {
        DriverLayoutSource {
            glvnd_lib: &image.glvnd_lib,
            mesa_lib: &image.mesa_lib,
            mesa_share: &image.mesa_share,
            va_dri: &image.va_dri,
        }
    }

    fn link_target(path: &Path) -> PathBuf {
        fs::read_link(path).expect("entry should be a symlink")
    }

    fn resolved_contents(path: &Path) -> String {
        fs::read_to_string(path).expect("symlink target should be readable")
    }

    #[test]
    fn farm_links_every_published_runtime_into_the_conventional_root() {
        let image = fake_image();
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");

        build_driver_layout(&root, &source(&image)).expect("farm should build");

        // glvnd dispatcher and GL entry points.
        for name in GLVND_LIBS {
            assert_eq!(
                link_target(&root.join("lib").join(name)),
                image.glvnd_lib.join(name),
                "glvnd library {name} should point at the gpu runtime"
            );
        }
        // mesa's flat libraries.
        assert_eq!(
            link_target(&root.join("lib/libEGL_mesa.so.0")),
            image.mesa_lib.join("libEGL_mesa.so.0")
        );
        // mesa's DRI drivers and GBM backends.
        assert_eq!(
            link_target(&root.join("lib/dri/virtio_gpu_dri.so")),
            image.mesa_lib.join("dri/virtio_gpu_dri.so")
        );
        assert_eq!(
            link_target(&root.join("lib/gbm/dri_gbm.so")),
            image.mesa_lib.join("gbm/dri_gbm.so")
        );
        // The discovery manifests.
        assert_eq!(
            link_target(&root.join("share/glvnd/egl_vendor.d/50_mesa.json")),
            image.mesa_share.join("glvnd/egl_vendor.d/50_mesa.json")
        );
        assert_eq!(
            link_target(&root.join("share/vulkan/icd.d/virtio_icd.x86_64.json")),
            image.mesa_share.join("vulkan/icd.d/virtio_icd.x86_64.json")
        );
        // For every link, the chain resolves to a real file.
        for (path, expected) in [
            (root.join("lib/libEGL.so.1"), GLVND_LIBS[0]),
            (root.join("lib/libEGL_mesa.so.0"), "libEGL_mesa.so.0"),
            (root.join("lib/gbm/dri_gbm.so"), "gbm backend"),
        ] {
            assert_eq!(resolved_contents(&path), expected, "{}", path.display());
        }
    }

    #[test]
    fn farm_dri_entry_is_the_patched_va_driver_never_mesas() {
        let image = fake_image();
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");

        build_driver_layout(&root, &source(&image)).expect("farm should build");

        let farm_driver = root.join("lib/dri").join(VA_DRIVER_FILE);
        // The link names the VA runtime, not mesa's own `dri/` copy.
        assert_eq!(
            link_target(&farm_driver),
            image.va_dri.join(VA_DRIVER_FILE),
            "the farm's dri entry must come from the patched VA runtime"
        );
        assert_eq!(resolved_contents(&farm_driver), "patched va driver");
    }

    #[test]
    fn farm_omits_the_va_entry_when_the_image_has_no_patched_driver() {
        let image = fake_image();
        fs::remove_file(image.va_dri.join(VA_DRIVER_FILE)).expect("drop the patched driver");
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");

        build_driver_layout(&root, &source(&image)).expect("farm should build");

        // mesa's unpatched copy must not leak into the conventional root: libva
        // searches it by default, and the unpatched driver stalls CQP encodes.
        assert!(
            !root.join("lib/dri").join(VA_DRIVER_FILE).exists(),
            "an absent patched driver must leave the entry absent"
        );
        // The rest of the farm is unaffected.
        assert!(root.join("lib/dri/virtio_gpu_dri.so").exists());
    }

    #[test]
    fn farm_rebuild_is_idempotent_and_drops_stale_entries() {
        let image = fake_image();
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");

        build_driver_layout(&root, &source(&image)).expect("first build");
        // A stale entry from an earlier layout must not survive a rebuild.
        let stale = root.join("lib/libstale.so");
        symlink("/nonexistent", &stale).expect("stale entry");
        build_driver_layout(&root, &source(&image)).expect("second build");

        assert!(!stale.exists(), "a rebuild must drop stale entries");
        assert_eq!(
            link_target(&root.join("lib/dri").join(VA_DRIVER_FILE)),
            image.va_dri.join(VA_DRIVER_FILE)
        );
        assert_eq!(
            link_target(&root.join("lib/libEGL.so.1")),
            image.glvnd_lib.join("libEGL.so.1")
        );
    }

    #[test]
    fn a_missing_runtime_directory_shrinks_the_farm_instead_of_failing() {
        let image = fake_image();
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");
        // Simulate an image built before the glvnd runtime existed.
        fs::remove_dir_all(&image.glvnd_lib).expect("drop the gpu runtime");
        fs::remove_dir_all(&image.mesa_share).expect("drop the mesa share tree");

        build_driver_layout(&root, &source(&image))
            .expect("a partial image should not fail the farm");

        assert!(!root.join("lib/libEGL.so.1").exists());
        assert!(root.join("lib/libEGL_mesa.so.0").exists());
        assert!(
            root.join("share/glvnd/egl_vendor.d").is_dir(),
            "the manifest directories are created even when empty"
        );
    }

    #[test]
    fn disabled_drm_mode_leaves_the_conventional_root_absent() {
        let image = fake_image();
        let dir = tempfile::tempdir().expect("tempdir should be created");
        let root = dir.path().join("opengl-driver");

        // A software-only guest pin points its own ICD by env, so it must not
        // get a hardware discovery tree grafted in.
        ensure_driver_layout(false, &root, &source(&image)).expect("disabled is a no-op");
        assert!(!root.exists(), "a non-DRM guest must not get the root");

        ensure_driver_layout(true, &root, &source(&image)).expect("enabled builds");
        assert!(root.join("lib/libEGL.so.1").exists());
    }
}
