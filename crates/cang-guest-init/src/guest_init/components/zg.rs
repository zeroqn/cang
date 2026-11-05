//! `zg`'s embedding side: which model this guest picks, and where the image's
//! pre-installed models are found.
//!
//! Two things happen here, both for the same reason - a first `zg --index` in a
//! fresh cang guest should not need the network, and it should use the hardware it
//! was given:
//!
//! * the model/device choice is exported into the guest's environment
//!   (`ZVEC_GREP_EMBEDDING`, `ZVEC_GREP_DEVICE`), which is where `zg` reads the
//!   defaults for a workspace index it has not built yet. An explicit
//!   `--embedding`/`--device` still wins, and an existing index keeps the model it
//!   stored.
//! * the image's model cache (`CANG_ZG_MODELS_DIR`) is linked into the task user's
//!   own cache, file by file, so `zg` resolves those snapshots locally while the
//!   cache itself stays a writable directory for models the user adds.
//!
//! The GPU model is `local/qwen3-embedding-0.6b` and the CPU one
//! `local/potion-code-16m-v2`: the GGUF model is the one a GPU helps (a 300M
//! encoder runs ~34x faster per indexed token on an RDNA3 GPU than on 28 Zen4
//! cores), and the Model2Vec model needs no accelerator at all - it indexes a small
//! workspace in well under a second on the CPU.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::guest_init::components::home::identity::DevIdentity;
use crate::guest_init::fs;

pub(in crate::guest_init) const MODELS_DIR_ENV: &str = "CANG_ZG_MODELS_DIR";
pub(in crate::guest_init) const EMBEDDING_ENV: &str = "ZVEC_GREP_EMBEDDING";
pub(in crate::guest_init) const DEVICE_ENV: &str = "ZVEC_GREP_DEVICE";
/// The CPU-side default: `zg`'s own out-of-the-box model, and one of the two the
/// image ships pre-downloaded.
pub(in crate::guest_init) const CPU_MODEL: &str = "local/potion-code-16m-v2";
/// The GPU-side default: llama.cpp GGUF, so it only makes sense when the guest has
/// a Vulkan device (`--gpu=drm`, through the venus ICD).
pub(in crate::guest_init) const GPU_MODEL: &str = "local/qwen3-embedding-0.6b";
/// `zg` resolves `auto` to the first ggml device that is not a CPU and falls back
/// to the CPU when it finds none, so this stays correct even if the guest's Vulkan
/// stack turns out to be unusable.
pub(in crate::guest_init) const GPU_DEVICE: &str = "auto";
pub(in crate::guest_init) const CPU_DEVICE: &str = "cpu";

/// How long the Vulkan probe may take before the guest decides it has no GPU.
/// `vulkaninfo --summary` either finds a device or fails; the bound is for a
/// venus device that wedges instead of erroring, which must not hold up boot.
pub(in crate::guest_init) const VULKAN_PROBE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);
/// Device names a software Vulkan implementation reports. A `--gpu=drm` guest can
/// still end up with one of these (llvmpipe/lavapipe through mesa), and that is a
/// CPU: the GGUF model on it is far slower than the Model2Vec model on the CPU.
const SOFTWARE_DEVICE_NAMES: &[&str] = &["llvmpipe", "lavapipe", "swiftshader", "softpipe"];

/// Export the model/device default for this guest.
///
/// The GPU default is only taken when the guest really has a Vulkan device: a
/// `--gpu=drm` guest whose venus device is unusable would otherwise index with
/// the GGUF model on the CPU, which is an order of magnitude slower than the CPU
/// model it replaced.
pub(in crate::guest_init) fn export_selection(gpu_drm: bool) {
    export_selection_with(gpu_drm, probe_vulkan_device);
}

pub(in crate::guest_init) fn export_selection_with(gpu_drm: bool, probe: impl FnOnce() -> bool) {
    let gpu = gpu_drm && probe();
    let (embedding, device) = if gpu {
        (GPU_MODEL, GPU_DEVICE)
    } else {
        (CPU_MODEL, CPU_DEVICE)
    };
    unsafe {
        std::env::set_var(EMBEDDING_ENV, embedding);
        std::env::set_var(DEVICE_ENV, device);
    }
}

/// Whether this guest has a non-software Vulkan device.
fn probe_vulkan_device() -> bool {
    probe_vulkan_device_with(|| run_probe("vulkaninfo", &["--summary"], VULKAN_PROBE_TIMEOUT))
}

fn probe_vulkan_device_with(probe: impl FnOnce() -> Option<String>) -> bool {
    let Some(stdout) = probe() else {
        return false;
    };
    stdout.lines().any(|line| {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        if key.trim() != "deviceName" {
            return false;
        }
        let name = value.trim().to_ascii_lowercase();
        !name.is_empty() && !SOFTWARE_DEVICE_NAMES.iter().any(|soft| name.contains(soft))
    })
}

/// Run a probe binary and return its stdout, giving up after `timeout`.
///
/// Both streams are captured so a probe that fails loudly writes nothing into the
/// guest's console, and the wait happens on a helper thread so a wedged device
/// cannot block boot: on timeout the probe is abandoned (the boot continues, and
/// the child is reaped when this process exits).
fn run_probe(bin: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut command = std::process::Command::new(bin);
    command.args(args).stdin(std::process::Stdio::null());
    std::thread::spawn(move || {
        let _ = sender.send(command.output());
    });
    let output = receiver.recv_timeout(timeout).ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Link the image's pre-installed models into the task user's model cache.
///
/// The image ships them read-only, in the layout `zg` itself would have written.
/// `zg` accepts such a snapshot without a completion marker (it validates size and
/// SHA-256 per file) and only attempts to write the marker as a best-effort. Pointing
/// the cache at the store instead would make the whole cache read-only, so the files
/// are linked in and `~/.zvec-grep/models` stays writable for anything the user adds.
pub(in crate::guest_init) fn seed_models(is_root: bool, identity: &DevIdentity) -> Result<()> {
    let source = std::env::var(MODELS_DIR_ENV).ok().map(PathBuf::from);
    seed_models_from(source.as_deref(), is_root, identity)
}

/// Seed from an already-resolved source: `None` means the image ships no models.
///
/// Split out of [`seed_models`] so callers that are not the guest runtime (the
/// tests) do not have to set the process-wide `CANG_ZG_MODELS_DIR`: `set_var`
/// is process-global and `cargo test` runs these tests in parallel threads, so a
/// test that went through the environment would race every other one.
pub(in crate::guest_init) fn seed_models_from(
    source: Option<&Path>,
    is_root: bool,
    identity: &DevIdentity,
) -> Result<()> {
    if !is_root {
        return Ok(());
    }
    let Some(source) = source else {
        return Ok(());
    };
    if !source.is_dir() {
        return Ok(());
    }
    let cache = model_cache_dir(identity);
    // The cache root has to exist even when the image ships no models, so `zg` can
    // write its own downloads there.
    fs::create_dir_all(&cache)?;
    link_tree(source, source, &cache)?;
    fs::chown_tree_skipping_symlinks(&cache, identity.uid, identity.gid)
}

pub(in crate::guest_init) fn model_cache_dir(identity: &DevIdentity) -> PathBuf {
    identity.home.join(".zvec-grep/models")
}

fn link_tree(root: &Path, dir: &Path, cache: &Path) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            link_tree(root, &path, cache)?;
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
        ensure_link(&path, &cache.join(relative))?;
    }
    Ok(())
}

/// Create `target` as a symlink to `source` unless something already exists there:
/// a user's own copy of the same model has to win over the image's.
fn ensure_link(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    match std::fs::symlink_metadata(target) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", target.display()));
        }
    }
    std::os::unix::fs::symlink(source, target).with_context(|| {
        format!(
            "failed to link {} -> {}",
            target.display(),
            source.display()
        )
    })
}

#[cfg(test)]
#[path = "zg_tests.rs"]
mod tests;
