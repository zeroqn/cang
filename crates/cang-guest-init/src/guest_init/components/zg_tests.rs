use super::*;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::guest_init::components::home::identity::DevIdentity;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// An identity inside `home` whose uid/gid are the ones this test process really
/// runs as: seeding chowns the cache, which only root can do to another uid.
fn identity_under(home: &Path) -> DevIdentity {
    let metadata = std::fs::metadata(home).expect("home metadata should be readable");
    DevIdentity {
        uid: metadata.uid(),
        gid: metadata.gid(),
        home: home.to_path_buf(),
        shell: PathBuf::from("fish"),
    }
}

/// Records what the stacking step asked the system for, so a test never runs a
/// real `mount` and can still see the order of the calls.
#[derive(Default)]
struct FakeModelsOps {
    mounted: bool,
    binds: Mutex<Vec<(PathBuf, PathBuf)>>,
    chowns: Mutex<Vec<(PathBuf, u32, u32)>>,
}

impl FakeModelsOps {
    fn already_stacked() -> Self {
        Self {
            mounted: true,
            ..Self::default()
        }
    }

    fn binds(&self) -> Vec<(PathBuf, PathBuf)> {
        self.binds.lock().expect("bind lock").clone()
    }

    fn chowns(&self) -> Vec<(PathBuf, u32, u32)> {
        self.chowns.lock().expect("chown lock").clone()
    }
}

impl ZvecModelsOps for FakeModelsOps {
    fn create_dir_all(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)?;
        Ok(())
    }

    fn is_mounted(&self, _path: &Path) -> Result<bool> {
        Ok(self.mounted)
    }

    fn bind_mount(&self, source: &Path, target: &Path) -> Result<()> {
        self.binds
            .lock()
            .expect("bind lock")
            .push((source.to_path_buf(), target.to_path_buf()));
        Ok(())
    }

    fn chown_tree(&self, path: &Path, uid: u32, gid: u32) -> Result<()> {
        self.chowns
            .lock()
            .expect("chown lock")
            .push((path.to_path_buf(), uid, gid));
        // The real implementation cannot chown to a foreign uid without root; the
        // recorded call is what these tests assert.
        let _ = (path, uid, gid);
        Ok(())
    }
}

#[test]
fn gpu_guest_selects_the_gguf_model_and_auto_device() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    // Injected, not probed: the test host has no `vulkaninfo`, and this test is
    // about the selection a detected device produces.
    export_selection_with(true, || true);

    assert_eq!(std::env::var(EMBEDDING_ENV).as_deref(), Ok(GPU_MODEL));
    assert_eq!(std::env::var(DEVICE_ENV).as_deref(), Ok(GPU_DEVICE));
}

#[test]
fn cpu_guest_selects_the_potion_model_and_the_cpu() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    export_selection(false);

    assert_eq!(std::env::var(EMBEDDING_ENV).as_deref(), Ok(CPU_MODEL));
    assert_eq!(std::env::var(DEVICE_ENV).as_deref(), Ok(CPU_DEVICE));
}

#[test]
fn seed_links_every_file_with_its_directories_into_the_cache() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let models = temp.path().join("image-models");
    std::fs::create_dir_all(models.join("model2vec/org--model/rev/tokenizer"))
        .expect("source tree should be created");
    std::fs::write(
        models.join("model2vec/org--model/rev/model.safetensors"),
        b"weights",
    )
    .expect("model file should be written");
    std::fs::write(
        models.join("model2vec/org--model/rev/tokenizer/tokenizer.json"),
        b"tokenizer",
    )
    .expect("tokenizer file should be written");
    std::fs::write(models.join("hf_model.gguf"), b"gguf").expect("gguf should be written");

    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let staging = temp.path().join("staging/models");
    let ops = FakeModelsOps::default();
    seed_models_with(Some(&models), true, &identity, &staging, &ops)
        .expect("seeding should succeed");

    for relative in [
        "model2vec/org--model/rev/model.safetensors",
        "model2vec/org--model/rev/tokenizer/tokenizer.json",
        "hf_model.gguf",
    ] {
        let link = staging.join(relative);
        let target = std::fs::read_link(&link).expect("staged entry should be a symlink");
        assert_eq!(target, models.join(relative));
        assert!(
            std::fs::metadata(&link)
                .expect("link target should resolve")
                .is_file(),
            "{relative} should resolve to a file"
        );
    }
    let cache = model_cache_dir(&identity);
    assert_eq!(
        ops.binds(),
        vec![(staging.clone(), cache)],
        "the image's models have to be stacked over the task user's cache"
    );
    assert_eq!(
        ops.chowns(),
        vec![(staging, identity.uid, identity.gid)],
        "the stacked directory is the task user's"
    );
}

#[test]
fn seed_stacks_over_a_host_entry_the_guest_cannot_resolve() {
    // This is the failure the stacking fixes: cang bind-mounts the host's
    // `~/.zvec-grep`, a host model is often a symlink into the host's own store,
    // and that store path does not exist inside a guest. The image's copy has to
    // be what the guest resolves, and the host's tree must not be written to.
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let models = temp.path().join("image-models");
    std::fs::create_dir_all(&models).expect("source tree should be created");
    std::fs::write(models.join("hf_model.gguf"), b"image copy").expect("image copy should exist");

    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let cache = model_cache_dir(&identity);
    std::fs::create_dir_all(&cache).expect("cache should be created");
    let host_target = Path::new("/nix/store/host-only-cang-zg-models/models/hf_model.gguf");
    std::os::unix::fs::symlink(host_target, cache.join("hf_model.gguf"))
        .expect("the host's symlink should be created");

    let staging = temp.path().join("staging/models");
    let ops = FakeModelsOps::default();
    seed_models_with(Some(&models), true, &identity, &staging, &ops)
        .expect("seeding should succeed");

    assert_eq!(
        std::fs::read_link(cache.join("hf_model.gguf")).expect("the host entry stays a symlink"),
        host_target,
        "the host's own tree is never rewritten"
    );
    assert_eq!(
        std::fs::read_link(staging.join("hf_model.gguf")).expect("the image's model is staged"),
        models.join("hf_model.gguf")
    );
    assert_eq!(ops.binds(), vec![(staging, model_cache_dir(&identity))]);
}

#[test]
fn seed_without_the_image_directory_still_stacks_a_writable_layer() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let cache = model_cache_dir(&identity);
    std::fs::create_dir_all(&cache).expect("cache should be created");
    std::fs::write(cache.join("own.gguf"), b"keep").expect("cache file should be written");

    let absent = temp.path().join("absent");
    let staging = temp.path().join("staging/models");
    let ops = FakeModelsOps::default();
    seed_models_with(Some(&absent), true, &identity, &staging, &ops)
        .expect("seeding without the image directory should succeed");

    assert!(
        staging.is_dir(),
        "the task user still needs a writable models directory, and it must not be the host's"
    );
    assert_eq!(
        std::fs::read_dir(&staging)
            .expect("staging should be readable")
            .count(),
        0
    );
    assert_eq!(ops.binds(), vec![(staging, model_cache_dir(&identity))]);
    assert!(
        cache.join("own.gguf").is_file(),
        "the host's tree keeps its own files"
    );
}

#[test]
fn seed_does_not_stack_twice() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let staging = temp.path().join("staging/models");

    let ops = FakeModelsOps::already_stacked();
    seed_models_with(None, true, &identity, &staging, &ops).expect("seeding should succeed");
    assert!(
        ops.binds().is_empty(),
        "an already stacked cache must not get another mount on top"
    );
}

#[test]
fn seed_is_a_no_op_for_a_non_root_call() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let staging = temp.path().join("staging/models");
    let ops = FakeModelsOps::default();
    seed_models_with(Some(temp.path()), false, &identity, &staging, &ops)
        .expect("non-root seeding should succeed");
    assert!(!staging.exists());
    assert!(!model_cache_dir(&identity).exists());
    assert!(ops.binds().is_empty());
}

// The identity's uid/gid are only used to chown the cache; a test that cannot
// chown (no root, or a filesystem that disagrees) still has to see the links.
#[test]
fn seeded_cache_is_owned_like_the_home() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let metadata = std::fs::metadata(temp.path()).expect("tempdir metadata should be readable");
    let models = temp.path().join("image-models");
    std::fs::create_dir_all(&models).expect("source tree should be created");
    std::fs::write(models.join("hf_model.gguf"), b"gguf").expect("gguf should be written");
    std::fs::create_dir_all(temp.path().join("home")).expect("home should be created");
    let identity = DevIdentity {
        uid: metadata.uid(),
        gid: metadata.gid(),
        home: temp.path().join("home"),
        shell: PathBuf::from("fish"),
    };

    let staging = temp.path().join("staging/models");
    let ops = FakeModelsOps::default();
    seed_models_with(Some(&models), true, &identity, &staging, &ops)
        .expect("seeding should succeed");

    assert_eq!(
        ops.chowns(),
        vec![(staging, identity.uid, identity.gid)],
        "the stacked layer is owned like the task user's home"
    );
}

#[test]
fn gpu_default_needs_a_real_vulkan_device() {
    let _guard = ENV_LOCK.lock().expect("env test lock");

    // `--gpu=drm` without a usable device (venus unavailable, or a software
    // implementation): the CPU model, not the GGUF one on the CPU.
    export_selection_with(true, || false);
    assert_eq!(std::env::var(EMBEDDING_ENV).as_deref(), Ok(CPU_MODEL));
    assert_eq!(std::env::var(DEVICE_ENV).as_deref(), Ok(CPU_DEVICE));

    export_selection_with(true, || true);
    assert_eq!(std::env::var(EMBEDDING_ENV).as_deref(), Ok(GPU_MODEL));
    assert_eq!(std::env::var(DEVICE_ENV).as_deref(), Ok(GPU_DEVICE));
}

#[test]
fn probe_accepts_a_hardware_device() {
    let summary = "==========\nVulkan Instance Version: 1.4.363\n\nDevices:\n==========\ndeviceName         = AMD Radeon RX 7600M XT (RADV NAVI33)\ndeviceType         = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\n";
    assert!(probe_vulkan_device_with(|| Some(summary.to_owned())));
}

#[test]
fn probe_rejects_a_software_device_and_a_failed_probe() {
    let llvmpipe = "Devices:\ndeviceName = llvmpipe (LLVM 19.1.7, 256 bits)\n";
    assert!(!probe_vulkan_device_with(|| Some(llvmpipe.to_owned())));
    let empty = "Vulkan Instance Version: 1.4.363\n";
    assert!(!probe_vulkan_device_with(|| Some(empty.to_owned())));
    assert!(!probe_vulkan_device_with(|| None));
}
