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

#[test]
fn gpu_guest_selects_the_gguf_model_and_auto_device() {
    let _guard = ENV_LOCK.lock().expect("env test lock");
    export_selection(true);

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
    unsafe { std::env::set_var(MODELS_DIR_ENV, &models) };
    seed_models(true, &identity).expect("seeding should succeed");
    unsafe { std::env::remove_var(MODELS_DIR_ENV) };

    let cache = model_cache_dir(&identity);
    for relative in [
        "model2vec/org--model/rev/model.safetensors",
        "model2vec/org--model/rev/tokenizer/tokenizer.json",
        "hf_model.gguf",
    ] {
        let link = cache.join(relative);
        let target = std::fs::read_link(&link).expect("cache entry should be a symlink");
        assert_eq!(target, models.join(relative));
        assert!(
            std::fs::metadata(&link)
                .expect("link target should resolve")
                .is_file(),
            "{relative} should resolve to a file"
        );
    }
}

#[test]
fn seed_keeps_a_model_the_user_already_downloaded() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let models = temp.path().join("image-models");
    std::fs::create_dir_all(&models).expect("source tree should be created");
    std::fs::write(models.join("hf_model.gguf"), b"image copy").expect("image copy should exist");

    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let cache = model_cache_dir(&identity);
    std::fs::create_dir_all(&cache).expect("cache should be created");
    std::fs::write(cache.join("hf_model.gguf"), b"user copy").expect("user copy should be written");

    unsafe { std::env::set_var(MODELS_DIR_ENV, &models) };
    seed_models(true, &identity).expect("seeding should succeed");
    unsafe { std::env::remove_var(MODELS_DIR_ENV) };

    let contents =
        std::fs::read(cache.join("hf_model.gguf")).expect("cached model should be readable");
    assert_eq!(contents, b"user copy");
    assert!(
        std::fs::symlink_metadata(cache.join("hf_model.gguf"))
            .expect("cached model metadata should be readable")
            .file_type()
            .is_file(),
        "the user's copy has to stay a regular file, not become a link"
    );
}

#[test]
fn seed_without_the_image_directory_leaves_the_cache_alone() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    let cache = model_cache_dir(&identity);
    std::fs::create_dir_all(&cache).expect("cache should be created");
    std::fs::write(cache.join("own.gguf"), b"keep").expect("cache file should be written");

    unsafe { std::env::set_var(MODELS_DIR_ENV, temp.path().join("absent")) };
    seed_models(true, &identity).expect("seeding without the image directory should succeed");
    unsafe { std::env::remove_var(MODELS_DIR_ENV) };

    assert!(cache.join("own.gguf").is_file());
    assert_eq!(
        std::fs::read_dir(&cache)
            .expect("cache should be readable")
            .count(),
        1
    );
}

#[test]
fn seed_is_a_no_op_for_a_non_root_call() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("home should be created");
    let identity = identity_under(&home);
    seed_models(false, &identity).expect("non-root seeding should succeed");
    assert!(!model_cache_dir(&identity).exists());
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

    unsafe { std::env::set_var(MODELS_DIR_ENV, &models) };
    seed_models(true, &identity).expect("seeding should succeed");
    unsafe { std::env::remove_var(MODELS_DIR_ENV) };

    let cache = model_cache_dir(&identity);
    let owner = std::fs::metadata(&cache).expect("cache metadata should be readable");
    assert_eq!(owner.uid(), metadata.uid());
    assert_eq!(owner.gid(), metadata.gid());
}
