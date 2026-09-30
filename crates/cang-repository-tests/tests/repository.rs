const FLAKE_NIX: &str = include_str!("../../../flake.nix");
const ADR_0005_NEUTRAL_CANG_PREBUILT_ASSETS_MD: &str =
    include_str!("../../../docs/adr/0005-neutral-cang-prebuilt-assets.md");
const ADR_0008_CANG_LINKS_LIBKRUN_RUST_API_MD: &str =
    include_str!("../../../docs/adr/0008-cang-links-libkrun-rust-api.md");
const CONTEXT_MD: &str = include_str!("../../../CONTEXT.md");
const MAINTENANCE_MD: &str = include_str!("../../../docs/maintenance.md");
const LAYERS: &str = include_str!("../../../nix/image/layers.nix");
const CONTAINER_NIX: &str = include_str!("../../../nix/image/container.nix");
const IMAGE_CONFIG_NIX: &str = include_str!("../../../nix/image/config.nix");
const IMAGE_CHECKS_NIX: &str = include_str!("../../../nix/image/checks.nix");
const NIX_STORE_DB_CHECK_NIX: &str = include_str!("../../../nix/image/nix-store-db-check.nix");
const PINS_NIX: &str = include_str!("../../../nix/pins.nix");
const WORKSPACE_SRC_NIX: &str = include_str!("../../../nix/pkgs/workspace-src.nix");
const SECCOMP_JSON_NIX: &str =
    include_str!("../../../nix/pkgs/container-lib-policy-seccomp-json.nix");
const CANG_RUST_NIX: &str = include_str!("../../../nix/pkgs/cang-rust.nix");
const CANG_PREBUILT_NIX: &str = include_str!("../../../nix/pkgs/cang-prebuilt.nix");
const TEST_YML: &str = include_str!("../../../.github/workflows/test.yml");
const PUBLISH_RELEASE_YML: &str = include_str!("../../../.github/workflows/publish_release.yml");
const PUBLISH_IMAGE_YML: &str = include_str!("../../../.github/workflows/publish_image.yml");
const PUBLISH_DEV_IMAGE_YML: &str =
    include_str!("../../../.github/workflows/publish_dev_image.yml");
fn nix_list_body<'a>(source: &'a str, list_name: &str) -> &'a str {
    source
        .split(&format!("{list_name} = ["))
        .nth(1)
        .and_then(|tail| tail.split("];").next())
        .unwrap_or_else(|| panic!("{list_name} list should exist"))
}

fn nix_top_level_attr_body<'a>(source: &'a str, attr_name: &str) -> &'a str {
    source
        .split(&format!("  {attr_name} = {{\n"))
        .nth(1)
        .and_then(|tail| tail.split("\n  };").next())
        .unwrap_or_else(|| panic!("{attr_name} attrset should exist"))
}

fn is_numeric(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

/// A permanent fork release tag of the form `v<upstream version>-cang.<n>`, or
/// `v<upstream version>-cang-lts.<n>` for the LTS kernel line. The fork CI
/// publishes these on demand and never prunes them; its rolling `<line>-<sha>`
/// prereleases are deleted once ten newer ones exist per line.
fn is_versioned_fork_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let rest = match rest.rsplit_once("-cang-lts.") {
        Some((base, counter)) => (base, counter),
        None => match rest.rsplit_once("-cang.") {
            Some((base, counter)) => (base, counter),
            None => return false,
        },
    };
    let (base, counter) = rest;
    let parts: Vec<&str> = base.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| is_numeric(part)) && is_numeric(counter)
}

/// A permanent cang release tag of the form `v<major>.<minor>.<patch>`. The
/// release workflow never prunes versioned releases, while it keeps only the 20
/// newest rolling `sha-<revision>` prereleases.
fn is_cang_version_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| is_numeric(part))
}

/// Every `tag = "<release>";` inside a pinned release attrset, in order.
fn pinned_release_tags(attr_name: &str) -> Vec<(usize, String)> {
    let body = nix_top_level_attr_body(PINS_NIX, attr_name);
    body.split("tag = \"")
        .skip(1)
        .enumerate()
        .filter_map(|(index, tail)| tail.split('"').next().map(|tag| (index, tag.to_owned())))
        .collect()
}

fn pinned_release_tag(attr_name: &str) -> String {
    let body = nix_top_level_attr_body(PINS_NIX, attr_name);
    body.split("tag = \"")
        .nth(1)
        .and_then(|tail| tail.split('"').next())
        .unwrap_or_else(|| panic!("{attr_name} should pin a release tag"))
        .to_owned()
}

fn heredoc_bodies<'a>(source: &'a str, start: &str, end: &str) -> Vec<&'a str> {
    source
        .split(start)
        .skip(1)
        .filter_map(|tail| tail.split(end).next())
        .collect()
}

fn all_heredoc_bodies(source: &str) -> Vec<&str> {
    heredoc_bodies(source, "cat > release-notes.md <<EOF_NOTES", "EOF_NOTES")
}

fn assert_no_unescaped_backticks(source: &str) {
    let mut escaped = false;
    for character in source.chars() {
        if character == '`' && !escaped {
            panic!("unescaped backtick would execute as shell command substitution");
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
}

#[test]
fn flake_exposes_container_lib_seccomp_policy_package() {
    for required in [
        "containerLibPolicySeccompJson = import ./nix/pkgs/container-lib-policy-seccomp-json.nix",
        "container-lib-policy-seccomp-json = containerLibPolicySeccompJson;",
        "containerLibPolicySeccompJson",
    ] {
        assert!(FLAKE_NIX.contains(required), "missing {required}");
    }
}

#[test]
fn flake_exposes_cang_outputs() {
    for required in [
        "cangImage = mkImage rustPackages.cangMuslPackage;",
        "cang = rustPackages.rustPackage;",
        "prebuiltCang = import ./nix/pkgs/cang-prebuilt.nix",
        "cang-prebuilt = prebuiltCang;",
        "cang-musl = rustPackages.cangMuslPackage;",
        "cang-musl-ci-sccache = rustPackagesCiSccache.cangMuslPackage;",
        "container = cangImage;",
        "cang-ci-sccache = rustPackagesCiSccache.rustPackage;",
        "container-ci-sccache = cangImageCiSccache;",
        "container-nix-db-metadata = cangImageChecks.imageConfigNixDbRefs;",
    ] {
        assert!(FLAKE_NIX.contains(required), "missing {required}");
    }
}

/// The fork checkouts a build compiles arrive as flake inputs, not as Git
/// submodule contents: a flake's own source cannot carry those, and
/// `inputs.self.submodules = true` only records `submodules = true` on the
/// ref a downstream `github:` lock writes, which that scheme then rejects with
/// "input attribute 'submodules' not supported by scheme 'github'"
/// (NixOS/nix#13571).
#[test]
fn fork_sources_are_flake_inputs_grafted_into_the_workspace() {
    for required in [
        "libkrun-src = {",
        "libkrunfw-src = {",
        "url = \"github:zeroqn/libkrun/cang\";",
        "url = \"github:zeroqn/libkrunfw/cang\";",
        "flake = false;",
        "workspaceSrc = import ./nix/pkgs/workspace-src.nix {",
    ] {
        assert!(FLAKE_NIX.contains(required), "missing {required}");
    }
    assert!(
        !FLAKE_NIX.contains("self.submodules = true;"),
        "inputs.self.submodules poisons a downstream github: lock ref without \
         carrying submodule contents; the fork inputs do that instead"
    );
    for required in [
        "cp -r --no-preserve=mode,ownership ${libkrunSrc} $out/deps/libkrun",
        "cp -r --no-preserve=mode,ownership ${libkrunfwSrc} $out/deps/libkrunfw",
    ] {
        assert!(WORKSPACE_SRC_NIX.contains(required), "missing {required}");
    }
}

/// `cang-dev` used to live in the `nix/dev` sub-flake, whose source was the
/// same submodule-less tree as the root flake's. It is a root output now, and
/// every build compiles the grafted workspace source instead of `self`.
#[test]
fn flake_exposes_cang_dev_output_from_the_grafted_source() {
    for required in [
        "cang-dev = rustPackagesDev.rustPackage;",
        "rustPackagesDev = import ./nix/pkgs/cang-rust.nix {",
        "libkrunfw = libkrunfwLocal;",
        "libkrunfwSrc = libkrunfw-src;",
    ] {
        assert!(FLAKE_NIX.contains(required), "missing {required}");
    }
    assert_eq!(
        FLAKE_NIX.matches("src = workspaceSrc;").count(),
        3,
        "cang, cang-ci-sccache and cang-dev should all build the grafted workspace source"
    );
}

#[test]
fn crun_fork_is_removed_from_the_flake_and_image_modules() {
    for (label, source) in [
        ("flake.nix", FLAKE_NIX),
        ("nix/image/layers.nix", LAYERS),
        ("nix/image/container.nix", CONTAINER_NIX),
        ("nix/image/checks.nix", IMAGE_CHECKS_NIX),
    ] {
        for removed in [
            "nix/pkgs/crun.nix",
            "crun = import",
            "crun = crun;",
            "crun ? pkgs.crun",
            "podman.override",
            "podman ? pkgs.podman",
        ] {
            assert!(
                !source.contains(removed),
                "{label} still contains {removed}"
            );
        }
    }
    assert!(
        LAYERS.contains("pkgs.crun"),
        "the image should still ship the nixpkgs crun for guest podman"
    );
    assert!(
        FLAKE_NIX.contains("podman = pkgs.podman;"),
        "the flake should expose the plain nixpkgs podman"
    );
}

#[test]
fn dirge_and_omp_surface_is_removed() {
    for removed in [
        "dirgeSandboxPrebuiltRelease",
        "ompPrebuiltRelease",
        "dirgePrebuilt",
        "ompPrebuilt",
        "dirge-prebuilt",
        "omp-prebuilt",
        "nix/pkgs/dirge.nix",
        "nix/pkgs/omp-prebuilt.nix",
    ] {
        for (label, source) in [("flake.nix", FLAKE_NIX), ("nix/pins.nix", PINS_NIX)] {
            assert!(
                !source.contains(removed),
                "{label} still contains {removed}"
            );
        }
    }
}

#[test]
fn publish_image_workflows_publish_only_cang() {
    for workflow in [PUBLISH_IMAGE_YML, PUBLISH_DEV_IMAGE_YML] {
        for required in [
            "image_name: cang",
            "flake_attr: container-ci-sccache",
            "local_image: localhost/cang:latest",
            "init_binary: /bin/cang-guest-init",
            "uses: actions/cache@v6",
            r#"docker load --input "$(cat "${{ matrix.image_name }}-container-path.txt")""#,
            r#"docker run --rm --entrypoint "${{ matrix.init_binary }}" "${{ matrix.local_image }}" --help > /dev/null"#,
            r#"docker tag "${{ matrix.local_image }}" "${{ steps.image_meta.outputs.target_image }}:${{ steps.image_meta.outputs.tag1 }}""#,
            r#"docker tag "${{ matrix.local_image }}" "${{ steps.image_meta.outputs.target_image }}:${{ steps.image_meta.outputs.tag2 }}""#,
        ] {
            assert!(workflow.contains(required), "missing {required}");
        }
    }

    for required in [
        r#"if [ "${GITHUB_REF_TYPE}" = "tag" ]; then"#,
        "tag1=${GITHUB_REF_NAME}",
        "tag1=latest",
        "tag2=sha-${short_sha}",
    ] {
        assert!(PUBLISH_IMAGE_YML.contains(required), "missing {required}");
    }

    for required in ["tag1=dev", "tag2=sha-${short_sha}"] {
        assert!(
            PUBLISH_DEV_IMAGE_YML.contains(required),
            "missing {required}"
        );
    }
}

#[test]
fn publish_release_uploads_only_neutral_cang_assets() {
    for required in [
        "uses: actions/cache@v6",
        "path: /tmp/cang-ci-sccache-release",
        "nix build --option extra-sandbox-paths \"${sccache_sandbox_path}\" .#cang-ci-sccache -o result-cang",
        "cang_asset_name=\"cang-${arch}-unknown-linux-gnu\"",
        "raw_cang_path=\"result-cang/bin/cang\"",
        "readelf -h \"${raw_cang_path}\" > /dev/null",
        "refusing to publish cang wrapper script",
        "refusing to publish non-ELF cang asset",
        "patchelf --set-interpreter",
        "grep -aEq '/nix/store/[0-9a-df-np-sv-z]{32}-'",
        "result-cang/bin/cang --help > /dev/null",
        "CANG_ASSET_PATH",
        "CANG_CHECKSUM_PATH",
        "neutral dynamically linked cang ELF packaging input",
    ] {
        assert!(PUBLISH_RELEASE_YML.contains(required), "missing {required}");
    }
}

#[test]
fn test_workflow_runs_the_documented_gates() {
    for required in [
        "cargo test \\",
        "--package cang-repository-tests",
        "cargo fmt --check",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo deny check",
        "nix flake check -L",
    ] {
        assert!(TEST_YML.contains(required), "missing {required}");
    }
}

#[test]
fn publish_release_prunes_only_dev_releases() {
    for required in [
        r#"map(select(.tag_name | startswith("sha-")))"#,
        "refusing to prune non-dev release",
        r#"case "${tag}" in"#,
    ] {
        assert!(PUBLISH_RELEASE_YML.contains(required), "missing {required}");
    }
    assert_eq!(
        PUBLISH_RELEASE_YML.matches("gh release delete").count(),
        1,
        "the prune step should delete through one guarded call site"
    );
}

#[test]
fn pinned_fork_releases_use_permanent_version_tags() {
    // libkrunRelease left with the prebuilt C-ABI libkrun pipeline (ADR 0008);
    // libkrunfw is the fork pin a cang release still depends on.
    let attr_name = "libkrunfwRelease";
    // The x86_64 assets come from the newest-kernel line and the other
    // architectures from the LTS line, so a system may carry its own tag.
    for (_line_number, tag) in pinned_release_tags(attr_name) {
        assert!(
            is_versioned_fork_release_tag(&tag),
            "{attr_name} pins rolling {tag}; pin a permanent \
             v<version>-cang.<n> / v<version>-cang-lts.<n> release so a tagged \
             cang release cannot reference a pruned artifact"
        );
    }
    let tag = pinned_release_tag(attr_name);
    assert!(
        is_versioned_fork_release_tag(&tag),
        "{attr_name} pins rolling {tag}; pin a permanent v<version>-cang.<n> \
         release so a tagged cang release cannot reference a pruned artifact"
    );
}

#[test]
fn versioned_fork_release_tag_shape_is_enforced() {
    for accepted in [
        "v1.19.5-cang.1",
        "v5.6.2-cang.42",
        "v10.0.0-cang.7",
        "v5.6.2-cang-lts.1",
        "v10.0.0-cang-lts.7",
    ] {
        assert!(
            is_versioned_fork_release_tag(accepted),
            "should accept {accepted}"
        );
    }
    for rejected in [
        "cang-8390691dec6e",
        "cang-lts-8390691dec6e",
        "v1.19.5",
        "1.19.5-cang.1",
        "v1.19-cang.1",
        "v1.19.5.1-cang.1",
        "v1.19.5-cang",
        "v1.19.5-cang.x",
        "v1.19.5-cang-lts",
        "v1.19.5-canglts.1",
        "v1.19.5-cang-lts.x",
    ] {
        assert!(
            !is_versioned_fork_release_tag(rejected),
            "should reject {rejected}"
        );
    }
}

#[test]
fn cang_version_tag_shape_is_enforced() {
    for accepted in ["v0.7.1", "v1.0.0", "v10.20.30"] {
        assert!(is_cang_version_tag(accepted), "should accept {accepted}");
    }
    for rejected in ["sha-3300a11ece86", "0.7.1", "v0.7", "v0.7.1.2", "v0.7.x"] {
        assert!(!is_cang_version_tag(rejected), "should reject {rejected}");
    }
}

#[test]
fn publish_release_gates_tagged_releases_on_permanent_fork_pins() {
    for required in [
        "Require a versioned libkrunfw pin",
        "refusing to publish a tagged cang release with a rolling fork pin",
        "libkrunfwRelease",
    ] {
        assert!(PUBLISH_RELEASE_YML.contains(required), "missing {required}");
    }
    // cang links libkrun's Rust API, so the pinned prebuilt C-ABI library is no
    // longer something a cang release depends on.
    assert!(
        !PUBLISH_RELEASE_YML.contains("libkrunRelease"),
        "the release pin gate should no longer mention the retired prebuilt libkrun pin"
    );
}

/// The prebuilt C-ABI libkrun pipeline existed to give the dlopen binding
/// something to load. cang now compiles libkrun's Rust API out of
/// `deps/libkrun`, so the packager, its pin and its updater have no consumer,
/// and the image must not carry a `libkrun.so` that nothing links.
#[test]
fn prebuilt_libkrun_pipeline_stays_retired() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for removed in ["nix/pkgs/libkrun.nix", "scripts/update-libkrun.sh"] {
        assert!(
            !repo_root.join(removed).exists(),
            "{removed} should stay deleted: cang links libkrun's Rust API from deps/libkrun, so the prebuilt C-ABI library has no consumer"
        );
    }
    // `packages.libkrunfw` and `libkrun-loadable`-style neighbours make a bare
    // `libkrun` needle useless: assert the whole retired form instead.
    for (source, absent) in [
        (PINS_NIX, "libkrunRelease = {"),
        (FLAKE_NIX, "libkrun-loadable"),
        (FLAKE_NIX, "packages.libkrun;"),
        (FLAKE_NIX, "nix/pkgs/libkrun.nix"),
    ] {
        assert!(
            !source.contains(absent),
            "the retired prebuilt libkrun pipeline should not reappear: found {absent}"
        );
    }
    assert!(
        !LAYERS.contains("\n    libkrun\n") && LAYERS.contains("\n    libkrunfw\n"),
        "the image tooling layer should carry libkrunfw (the firmware), not libkrun.so"
    );
}

#[test]
fn publish_release_notes_escape_markdown_backticks_for_shell_heredoc() {
    let bodies = all_heredoc_bodies(PUBLISH_RELEASE_YML);
    assert_eq!(
        bodies.len(),
        2,
        "expected tag and branch release notes heredocs"
    );
    let combined: String = bodies.join("\n");

    assert_no_unescaped_backticks(&combined);
    for required in [
        r"\`${{ steps.prep.outputs.cang_asset_name }}\`",
        r"\`nix build .#cang\`",
        r"\`nix build .#cang-prebuilt\`",
        r"\`ghcr.io/<repo-owner>/cang\`",
        r"\`alpha\`",
        r"\`${{ steps.prep.outputs.sha_tag }}\`",
    ] {
        assert!(combined.contains(required), "missing {required}");
    }
}

#[test]
fn cang_package_exposes_stable_raw_elf_payload_for_release_workflow() {
    for required in [
        "enableCiSccache ? false,",
        "ciSccacheNativeBuildInputs = pkgs.lib.optionals enableCiSccache [ pkgs.sccache ];",
        r#"RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";"#,
        r#"SCCACHE_DIR = "/nix/var/cache/sccache";"#,
        r#"SCCACHE_IGNORE_SERVER_IO_ERROR = "1";"#,
        r#"mkdir -p "$out/libexec/cang-helpers" "$out/lib/cang""#,
        r#"ln -s ${pkgs.buildah}/bin/buildah "$out/libexec/cang-helpers/buildah""#,
        r#"ln -s ${pkgs.btrfs-progs}/bin/btrfs "$out/libexec/cang-helpers/btrfs""#,
        r#"ln -s ${pkgs.btrfs-progs}/bin/mkfs.btrfs "$out/libexec/cang-helpers/mkfs.btrfs""#,
        r#"ln -s ${pkgs.util-linux}/bin/blkid "$out/libexec/cang-helpers/blkid""#,
        r#"ln -s ${pkgs.passt}/bin/pasta "$out/libexec/cang-helpers/pasta""#,
        r#"ln -s ${pkgs.passt}/bin/passt "$out/libexec/cang-helpers/passt""#,
        "${pkgs.lib.getLib libkrunfw}/lib/libkrunfw.so*",
    ] {
        assert!(CANG_RUST_NIX.contains(required), "missing {required}");
    }

    for required in [
        // cang compiles libkrun's Rust API from the fork source grafted into
        // `deps/`, so the packager has to carry libkrun's build inputs and the
        // musl guest init blob.
        "pkgs.rustPlatform.bindgenHook",
        "pkgs.pkg-config",
        "pkgs.rustfmt",
        "pkgs.virglrenderer",
        "pkgs.libgbm",
        "KRUN_INIT_BINARY_PATH",
        "pkgs.rustPlatform.fetchCargoVendor",
    ] {
        assert!(CANG_RUST_NIX.contains(required), "missing {required}");
    }

    for removed in [
        r#"install -Dm755 "$out/bin/cang" "$out/libexec/cang""#,
        // The release workflow publishes $out/bin/cang as the neutral asset and
        // refuses a wrapper script, so the package must stay wrapper-free.
        r#"wrapProgram "$out/bin/cang""#,
        "pkgs.makeWrapper",
        // libkrun is linked into the binary; the shared object symlinks and the
        // pre-libkrun vendoring must not come back.
        r#"${pkgs.lib.getLib libkrun}/lib/libkrun.so*"#,
        r#"${pkgs.lib.getLib libkrun}/lib/libkrun_init.so*"#,
        "cargoLock = {",
    ] {
        assert!(!CANG_RUST_NIX.contains(removed), "still contains {removed}");
    }

    let musl_build_flags = nix_list_body(CANG_RUST_NIX, "cargoBuildFlags");
    assert!(musl_build_flags.contains("cang-guest-init"));
    let musl_test_flags = nix_list_body(CANG_RUST_NIX, "cargoTestFlags");
    assert!(musl_test_flags.contains("cang-guest-init"));
}

#[test]
fn seccomp_policy_package_fetches_pinned_raw_container_libs_file() {
    for required in [
        "containerLibPolicySeccompJson = {",
        "owner = \"containers\";",
        "repo = \"container-libs\";",
        "path = \"common/pkg/seccomp/seccomp.json\";",
        "hash = \"sha256-m3VSAlFq7ktF2dQRq4AMIP5PevlxZqk7fwfVsWwaTs0=\";",
    ] {
        assert!(PINS_NIX.contains(required), "missing {required}");
    }

    for required in [
        "pkgs.fetchurl",
        "raw.githubusercontent.com/${pin.owner}/${pin.repo}/${pin.rev}/${pin.path}",
        "$out/share/containers/seccomp.json",
    ] {
        assert!(SECCOMP_JSON_NIX.contains(required), "missing {required}");
    }
}

#[test]
fn image_writes_global_seccomp_profile_config() {
    for required in [
        "./etc/containers",
        "cat > ./etc/containers/containers.conf <<'EOF_CONTAINERS_CONF'",
        "[containers]",
        "seccomp_profile = \"${containerLibPolicySeccompJson}/share/containers/seccomp.json\"",
        "chmod 0644 ./etc/containers/containers.conf",
    ] {
        assert!(CONTAINER_NIX.contains(required), "missing {required}");
    }
}

#[test]
fn image_includes_seccomp_policy_data_without_adding_it_to_path() {
    let image_contents = LAYERS
        .split("imageContents =\n")
        .nth(1)
        .and_then(|tail| tail.split("];").next())
        .expect("imageContents should exist");
    assert!(image_contents.contains("containerLibPolicySeccompJson"));

    let image_path_start = LAYERS.find("imagePath =").expect("imagePath should exist");
    let image_path_end = LAYERS[image_path_start..]
        .find("cangImageMaxLayers")
        .map(|offset| image_path_start + offset)
        .expect("imagePath section should end before maxLayers");
    assert!(!LAYERS[image_path_start..image_path_end].contains("containerLibPolicySeccompJson"));
}

#[test]
fn image_config_defines_cang_entrypoint() {
    for required in [
        r#""${cangMuslPackage}/bin/cang-guest-init""#,
        r#""enter""#,
        r#"Entrypoint = ["#,
        r#"name = "localhost/cang";"#,
    ] {
        assert!(
            IMAGE_CONFIG_NIX.contains(required) || CONTAINER_NIX.contains(required),
            "missing {required}"
        );
    }
    assert!(!IMAGE_CONFIG_NIX.contains("variants = {"));
    assert!(CONTAINER_NIX.contains("builtins.toJSON imageConfig"));
}

#[test]
fn image_env_exposes_guest_init_runtime_payloads() {
    for required in [
        r#"SHELL=${pkgs.fish}/bin/fish"#,
        r#"CANG_FISH_CONFIG_SOURCE=${configPayloads.fishConfig}/share/cang/fish/conf.d/cang-starship.fish"#,
        r#"CANG_STARSHIP_CONFIG_SOURCE=${configPayloads.starshipConfig}/share/cang/starship.toml"#,
        r#"CANG_REAL_PODMAN=${layers.realPodmanBin}"#,
    ] {
        assert!(IMAGE_CONFIG_NIX.contains(required), "missing {required}");
    }
}

#[test]
fn image_exports_real_podman_path_for_guest_init_service_start() {
    assert!(LAYERS.contains(r#"realPodmanBin = "${pkgs.podman}/bin/podman";"#));
    assert!(LAYERS.contains("realPodmanBin"));
    assert!(IMAGE_CONFIG_NIX.contains(r#"CANG_REAL_PODMAN=${layers.realPodmanBin}"#));
}

#[test]
fn cang_prebuilt_package_pins_and_patches_neutral_elf() {
    let cang_pin = nix_top_level_attr_body(PINS_NIX, "cangPrebuiltRelease");

    for required in [
        "owner = \"zeroqn\";",
        "repo = \"cang\";",
        "systems = {",
        "x86_64-linux = {",
        "hash = \"sha256-",
    ] {
        assert!(cang_pin.contains(required), "missing {required}");
    }

    // A rolling sha-<revision> prerelease carries the unversioned neutral asset
    // name, a permanent v<version> release the versioned one; the tag and the
    // asset name have to agree or pkgs.fetchurl cannot resolve the pin.
    let tag = pinned_release_tag("cangPrebuiltRelease");
    assert!(
        tag.starts_with("sha-") || is_cang_version_tag(&tag),
        "cangPrebuiltRelease pins {tag}; pin a rolling sha-<revision> prerelease \
         or a permanent v<version> release"
    );
    let expected_asset = if tag.starts_with("sha-") {
        "cang-x86_64-unknown-linux-gnu".to_owned()
    } else {
        format!("cang-{tag}-x86_64-unknown-linux-gnu")
    };
    assert!(
        cang_pin.contains(&format!("asset = \"{expected_asset}\";")),
        "cangPrebuiltRelease tag {tag} should pin the {expected_asset} asset"
    );
    assert!(
        !cang_pin.contains("systems = { };"),
        "cang prebuilt pin should not remain in bootstrap-empty state"
    );

    for required in [
        "libkrunfw ? null,",
        "cangPrebuiltRelease = pins.cangPrebuiltRelease;",
        "throw ''",
        "cang-<arch>-unknown-linux-gnu",
        "pkgs.autoPatchelfHook",
        "pkgs.stdenv.cc.cc.lib",
        "pkgs.stdenv.cc.libc",
        "pkgs.buildah",
        "pkgs.btrfs-progs",
        "pkgs.fuse-overlayfs",
        "pkgs.util-linux",
        "pkgs.lib.getLib libkrunfw",
        // Since cang links libkrun's Rust API the released asset has a
        // libvirglrenderer DT_NEEDED and a stripped rpath, so the packager has to
        // resolve it with autoPatchelfHook from its own inputs.
        "pkgs.virglrenderer",
        r#"magic="$(dd if="$src" bs=4 count=1"#,
        r#""7f454c46""#,
        r#"readelf -h "$src" >/dev/null"#,
        r#"install -Dm755 "$src" "$out/bin/cang""#,
        r#"mkdir -p "$out/libexec/cang-helpers" "$out/lib/cang""#,
        r#"ln -s ${pkgs.buildah}/bin/buildah "$out/libexec/cang-helpers/buildah""#,
        r#"ln -s ${pkgs.util-linux}/bin/blkid "$out/libexec/cang-helpers/blkid""#,
        "Do not pin wrapper-script release assets",
        "after a neutral sha-* release is published",
        r#"mainProgram = "cang";"#,
        "sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];",
    ] {
        assert!(CANG_PREBUILT_NIX.contains(required), "missing {required}");
    }

    for removed in [
        r#"install -Dm755 "$src" "$out/libexec/cang""#,
        r#"makeWrapper "$out/libexec/cang" "$out/bin/cang""#,
        "runtimeWrapperArgs",
        r#""LD_LIBRARY_PATH""#,
    ] {
        assert!(
            !CANG_PREBUILT_NIX.contains(removed),
            "still contains {removed}"
        );
    }
}

#[test]
fn cang_prebuilt_adr_records_neutral_asset_decision() {
    for required in [
        "# Neutral cang prebuilt release assets",
        "Status: accepted",
        "cang-<arch>-unknown-linux-gnu",
        "not standalone portable executables",
        "autoPatchelfHook",
        "concrete
  `/nix/store/<hash>-...` references",
        "Use `unsafeDiscardReferences`: rejected",
    ] {
        assert!(
            ADR_0005_NEUTRAL_CANG_PREBUILT_ASSETS_MD.contains(required),
            "missing {required}"
        );
    }
}

/// cang used to `dlopen` a pinned prebuilt libkrun; it now compiles libkrun's
/// Rust API out of the submodule. The rationale and the parts of the old design
/// that must not come back are recorded in ADR 0008, and CONTEXT.md's vocabulary
/// has to keep saying so.
#[test]
fn cang_links_libkrun_adr_records_the_binding_decision() {
    for required in [
        "# cang links libkrun's Rust API instead of loading a shared library",
        "Status: accepted",
        "cang-libkrun",
        "deps/libkrun",
        "krun-init-blob",
        "secure-execution mode",
        "Retire the prebuilt C-ABI libkrun pipeline",
        "libvirglrenderer.so.1",
    ] {
        assert!(
            ADR_0008_CANG_LINKS_LIBKRUN_RUST_API_MD.contains(required),
            "missing {required}"
        );
    }
    // The amending note keeps ADR 0005 from contradicting 0008.
    assert!(
        ADR_0005_NEUTRAL_CANG_PREBUILT_ASSETS_MD.contains("0008-cang-links-libkrun-rust-api.md"),
        "ADR 0005 should point at the amendment"
    );
    for required in [
        "links libkrun's Rust API into itself and opens only the firmware",
        "including the `libvirglrenderer` the linked libkrun needs",
    ] {
        assert!(
            CONTEXT_MD.contains(required),
            "CONTEXT.md missing {required}"
        );
    }
}

#[test]
fn cang_release_workflow_is_documented() {
    for required in [
        // The prebuilt updater only re-pins a published asset, so the procedure
        // that computes the SRI before the release exists has to stay written
        // down next to it.
        "follow the [cang release scheme](#cang-release-scheme)",
        "### cang release scheme",
        "cang-v<version>-<arch>-unknown-linux-gnu",
        "`rolling-alpha-release` concurrency group",
        "nix build .#cang-ci-sccache",
        "--set-interpreter /lib64/ld-linux-x86-64.so.2",
        r#"sha256sum "/tmp/cang-v<version>-x86_64-unknown-linux-gnu""#,
        "Tag **the pin commit**",
        "leaves the pinned hash",
        r#"git push origin main "v<version>""#,
        "nix build .#cang-prebuilt",
    ] {
        assert!(MAINTENANCE_MD.contains(required), "missing {required}");
    }
}

#[test]
fn image_layers_include_guest_init_config_payloads() {
    assert!(LAYERS.contains("fishConfig"));
    assert!(LAYERS.contains("starshipConfig"));
    assert!(LAYERS.contains("cangMuslPackage"));
}

#[test]
fn cang_wrappers_use_cang_internal_wait_contracts() {
    for required in [
        "cangNixCommandCompat = pkgs.writeShellScriptBin \"nix\"",
        "CANG_NIX_OVERLAY",
        "cang-guest-init internal nix wait",
        "cangPodmanCommandCompat = pkgs.writeShellScriptBin \"podman\"",
        "CANG_CONTAINERS_STORAGE",
        "cang-guest-init internal podman wait",
        "cangDockerCommandCompat = pkgs.writeShellScriptBin \"docker\"",
        "cang-guest-init internal podman service-wait",
        "cangDockerComposeCommandCompat = pkgs.writeShellScriptBin \"docker-compose\"",
        "cang_nix_ready_marker",
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn cang_image_includes_root_only_as_dev_helper() {
    for required in [
        r#"cangAsDevCommandCompat = pkgs.writeShellScriptBin "cang-as-dev""#,
        "cang-guest-init as-dev",
        "cangOnlyCommandCompat = [ cangAsDevCommandCompat ]",
        "++ imagePathPackages",
        "++ cangOnlyCommandCompat",
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn image_materializes_mimalloc_default_and_hardened_allocator_metadata() {
    for required in [
        r#"mimallocLib = "${pkgs.mimalloc}/lib/libmimalloc.so""#,
        r#"hardenedMallocLib = "${grapheneHardenedMalloc}/lib/libhardened_malloc.so""#,
        r#"printf '%s\n' '${layers.mimallocLib}' > ./etc/ld-nix.so.preload"#,
        "cat > ./etc/nix-allocator-libs <<EOF_NIX_ALLOCATOR_LIBS",
        "mimalloc=${layers.mimallocLib}",
        "hardened=${layers.hardenedMallocLib}",
        "chmod 0644 ./etc/ld-nix.so.preload",
        "chmod 0644 ./etc/nix-allocator-libs",
        r#"CANG_MIMALLOC_LIB=${layers.mimallocLib}"#,
        r#"CANG_GRAPHENE_HARDENED_MALLOC_LIB=${layers.hardenedMallocLib}"#,
    ] {
        assert!(
            LAYERS.contains(required)
                || CONTAINER_NIX.contains(required)
                || IMAGE_CONFIG_NIX.contains(required),
            "missing {required}"
        );
    }
    assert!(!CONTAINER_NIX.contains("LD_PRELOAD=${layers.hardenedMallocLib}"));
    assert!(!CONTAINER_NIX.contains("LD_PRELOAD=${layers.mimallocLib}"));
}

#[test]
fn image_nix_db_roots_include_libclang_config_reference() {
    for required in [
        r#"LIBCLANG_PATH=${pkgs.libclang.lib}/lib"#,
        "pkgs.libclang.lib",
        "cToolchainImagePackages = cToolchainPathPackages ++ [",
    ] {
        assert!(
            LAYERS.contains(required) || IMAGE_CONFIG_NIX.contains(required),
            "missing {required}"
        );
    }
}

#[test]
fn image_retains_graphene_hardened_malloc_for_hardened_mode() {
    for required in [
        "grapheneHardenedMalloc = pkgs.graphene-hardened-malloc.overrideAttrs",
        r#"version = "14";"#,
        r#"tag = "14";"#,
        r#"hash = "sha256-QUGDJyTnD5MuBUMlc4PZOZSAfevVUB6QbncVyXIAgb8=";"#,
        r#"hardenedMallocLib = "${grapheneHardenedMalloc}/lib/libhardened_malloc.so""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn image_includes_real_tmux_and_keeps_rmux() {
    for required in [
        "rmuxPrebuilt = import ./nix/pkgs/rmux-prebuilt.nix",
        "rmux-prebuilt = rmuxPrebuilt;",
        "rmuxPrebuilt",
        "pkgs.tmux",
        "test -x ${pkgs.tmux}/bin/tmux",
        "rmuxPrebuiltRelease = {",
        r#"owner = "Helvesec";"#,
        r#"repo = "rmux";"#,
        "./etc/rmux.conf",
        "set -g mouse off",
        r##"bind T if-shell -F '#{mouse}' 'set -g mouse off ; display-message "mouse OFF: native terminal selection enabled"' 'set -g mouse on ; display-message "mouse ON: pane mouse mode enabled"'"##,
        "set -g history-limit 100000",
        "set -g renumber-windows on",
        "set -g base-index 1",
        "setw -g pane-base-index 1",
        "setw -g mode-keys vi",
        "set -g status-keys vi",
        r##"bind | split-window -h -c "#{pane_current_path}""##,
        r##"bind - split-window -v -c "#{pane_current_path}""##,
        r##"bind c new-window -c "#{pane_current_path}""##,
        "./etc/tmux.conf",
        "bind-key | split-window -h",
        "bind-key - split-window -v",
        "bind-key h select-pane -L",
        "bind-key l select-pane -R",
        "bind-key j select-pane -D",
        "bind-key k select-pane -U",
    ] {
        assert!(
            FLAKE_NIX.contains(required)
                || LAYERS.contains(required)
                || CONTAINER_NIX.contains(required)
                || IMAGE_CHECKS_NIX.contains(required)
                || PINS_NIX.contains(required),
            "missing {required}"
        );
    }
    assert!(!LAYERS.contains("rmuxTmuxCommandCompat"));
    assert!(!LAYERS.contains("rmux-tmux-command-compat"));
}

#[test]
fn image_includes_hardening_run_for_foreign_binary_allocator_opt_in() {
    for required in [
        r#"pkgs.writeShellScriptBin "hardening-run""#,
        "allocator_lib=${hardenedMallocLib}",
        r#"export LD_PRELOAD="$allocator_lib""#,
        r#"export LD_PRELOAD="$allocator_lib:$LD_PRELOAD""#,
        r#"exec "$@""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn rust_tool_wrappers_mask_nix_loader_preload() {
    for required in [
        "rustcCommandCompat",
        r#"pkgs.writeShellScriptBin "rustc""#,
        "cang-empty-ld-nix-so-preload",
        "--ro-bind ${emptyLdNixSoPreload} /etc/ld-nix.so.preload",
        "--unsetenv LD_PRELOAD",
        "--unsetenv NSS_WRAPPER_PASSWD",
        "--unsetenv NSS_WRAPPER_GROUP",
        r#"${pkgs.rustc}/bin/rustc "$@""#,
        "rustAnalyzerCommandCompat",
        r#"pkgs.writeShellScriptBin "rust-analyzer""#,
        "cang-empty-ld-nix-so-preload",
        "--ro-bind ${emptyLdNixSoPreload} /etc/ld-nix.so.preload",
        "--unsetenv LD_PRELOAD",
        "--unsetenv NSS_WRAPPER_PASSWD",
        "--unsetenv NSS_WRAPPER_GROUP",
        r#"${pkgs.rust-analyzer}/bin/rust-analyzer "$@""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn nix_wrapper_waits_and_probes_only_for_cang_nix_overlay() {
    assert!(LAYERS.contains(r#"if [ "''${CANG_NIX_OVERLAY:-}" = "1" ]; then"#));
    assert!(LAYERS.contains(
        r#"export NIX_REMOTE="''${NIX_REMOTE:-unix:///nix/var/nix/daemon-socket/socket}""#
    ));
    assert!(LAYERS.contains("cang-guest-init internal nix wait"));
    assert!(LAYERS.contains(r#"${pkgs.nix}/bin/nix store info --store "$NIX_REMOTE" --json"#));
    assert!(LAYERS.contains("cang_nix_ready_marker"));

    let gate = LAYERS
        .find("CANG_NIX_OVERLAY")
        .expect("libkrun nix overlay gate should exist");
    let wait = LAYERS
        .find("cang-guest-init internal nix wait")
        .expect("nix wait should exist");
    let probe = LAYERS
        .find(r#"${pkgs.nix}/bin/nix store info --store "$NIX_REMOTE" --json"#)
        .expect("real nix connectivity probe should exist");
    let exec = LAYERS
        .find(r#"exec ${pkgs.nix}/bin/nix "$@""#)
        .expect("real nix exec should exist");
    assert!(gate < wait);
    assert!(wait < probe);
    assert!(probe < exec);
}

#[test]
fn nix_wrapper_uses_real_nix_path_for_probe_and_exec() {
    for required in [
        "nixCommandCompat",
        "pkgs.writeShellScriptBin \"nix\"",
        "unset LD_PRELOAD",
        "unset NSS_WRAPPER_PASSWD",
        "unset NSS_WRAPPER_GROUP",
        r#"${pkgs.nix}/bin/nix store info --store "$NIX_REMOTE" --json"#,
        r#"exec ${pkgs.nix}/bin/nix "$@""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn nix_wrapper_uses_marker_to_probe_connectivity_once_per_guest() {
    let marker = LAYERS
        .find("cang_nix_ready_marker")
        .expect("nix ready marker should exist");
    let probe = LAYERS
        .find(r#"${pkgs.nix}/bin/nix store info --store "$NIX_REMOTE" --json"#)
        .expect("real nix connectivity probe should exist");
    let marker_write = LAYERS
        .find(r#": > "$cang_nix_ready_marker""#)
        .expect("marker write should exist");

    assert!(marker < probe);
    assert!(probe < marker_write);
}

#[test]
fn image_static_nix_db_metadata_check_is_flake_exposed() {
    for required in [
        "checks = systems.forAllSystems",
        "import ./nix/image/checks.nix",
        "container-nix-db-metadata = cangImageChecks.imageConfigNixDbRefs;",
    ] {
        assert!(FLAKE_NIX.contains(required), "missing {required}");
    }

    for required in [
        "storeRefsIn =",
        "pkgs.lib.splitString \"/nix/store/\" text",
        "imageConfigText = builtins.unsafeDiscardStringContext",
        "imageNixDbClosureInfo = pkgs.closureInfo",
        "rootPaths = layers.imageContents;",
        "imageNixDbStorePathsText = builtins.readFile",
        "missingImageConfigNixDbRefs = builtins.filter",
        "Missing from pkgs.closureInfo { rootPaths = layers.imageContents; }:",
        "It does not inspect, repair, or mutate the host Nix DB.",
        "cat ${missingRefsMessageFile} >&2",
    ] {
        assert!(IMAGE_CHECKS_NIX.contains(required), "missing {required}");
    }

    assert!(!LAYERS.contains("imageMetadataNixDbRoots"));

    for required in [
        "imageChecks = import ./checks.nix",
        "image = pkgs.dockerTools.buildLayeredImage",
        "config = builtins.fromJSON (",
        "builtins.unsafeDiscardStringContext (builtins.toJSON imageConfig)",
        "if imageChecks.missingImageConfigNixDbRefs != [ ] then",
        "builtins.throw imageChecks.missingRefsMessage",
        "image.overrideAttrs",
        "checking cang image config Nix DB metadata coverage",
        "test -e ${imageChecks.imageConfigNixDbRefs}/passed",
        "(old.buildCommand or \"\");",
    ] {
        assert!(CONTAINER_NIX.contains(required), "missing {required}");
    }
}

#[test]
fn image_includes_manual_nix_store_db_checker() {
    for required in [
        r#"toolName = "cang-nix-store-db-check";"#,
        "nix path-info --all",
        "nix-store --verify-path",
        "! -name .links",
        "! -name '*.lock'",
        r#"runDir = "/run/cang";"#,
        r#"libkrun_upper_dir="${runDir}/nix-disk/upper""#,
        "/store/",
        "/var/nix",
        "store object present in libkrun upperdir",
        "store object not found in libkrun upperdir; may come from lower image or another mounted view",
        "upperdir unavailable; overlay source evidence not inspected",
        "upper store subdir unavailable/empty",
        "store-layer evidence only; not root-cause evidence",
        "metadata-shadow context only",
        "no repair was attempted",
    ] {
        assert!(
            NIX_STORE_DB_CHECK_NIX.contains(required),
            "missing {required}"
        );
    }

    for forbidden in ["caused by upperdir", "lower image is at fault"] {
        assert!(
            !NIX_STORE_DB_CHECK_NIX.contains(forbidden),
            "misleading causal wording present: {forbidden}"
        );
    }

    for required in [
        "nixStoreDbCheck = import ./nix-store-db-check.nix { inherit pkgs; };",
        "nixStoreDbCheck",
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn podman_wrapper_unsets_compat_env_before_execing_real_podman() {
    for required in [
        "podmanCommandCompat",
        "pkgs.writeShellScriptBin \"podman\"",
        "unset LD_PRELOAD",
        "unset NSS_WRAPPER_PASSWD",
        "unset NSS_WRAPPER_GROUP",
        r#"exec ${pkgs.podman}/bin/podman "$@""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn docker_wrapper_unsets_compat_env_before_execing_podman() {
    for required in [
        "dockerCommandCompat",
        "pkgs.writeShellScriptBin \"docker\"",
        "unset LD_PRELOAD",
        "unset NSS_WRAPPER_PASSWD",
        "unset NSS_WRAPPER_GROUP",
        r#"exec ${pkgs.podman}/bin/podman "$@""#,
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
}

#[test]
fn image_places_cargo_deny_in_tooling_layer_without_symposium() {
    let rust_toolchain = nix_list_body(LAYERS, "stableRustToolchainPackages");
    let tooling = nix_list_body(LAYERS, "toolingImagePackages");

    assert!(!rust_toolchain.contains("pkgs.cargo-deny"));
    assert!(tooling.contains("pkgs.cargo-deny"));
    assert!(!LAYERS.contains("symposium"));
}

#[test]
fn image_roots_musl_bin_output_exposed_by_image_path() {
    let c_toolchain_path = nix_list_body(LAYERS, "cToolchainPathPackages");

    assert!(c_toolchain_path.contains("pkgs.clang"));
    assert!(c_toolchain_path.contains("pkgs.gcc"));
    assert!(c_toolchain_path.contains("muslBin"));
    assert!(LAYERS.contains("imagePathPackages"));
    assert!(LAYERS.contains("cangOnlyCommandCompat"));
    assert!(LAYERS.contains("++ imagePathPackages"));
    assert!(LAYERS.contains("muslBin = pkgs.lib.getBin pkgs.musl;"));
    assert!(LAYERS.contains("cToolchainImagePackages = cToolchainPathPackages ++ ["));
    assert!(LAYERS.contains("pkgs.musl"));
}

#[test]
fn image_does_not_wire_symposium_package_into_container_layers() {
    for retained_package_output in [
        "symposium = import ./nix/pkgs/symposium.nix",
        "symposium = symposium;",
    ] {
        assert!(
            FLAKE_NIX.contains(retained_package_output),
            "missing {retained_package_output}"
        );
    }

    assert!(!FLAKE_NIX.contains("symposium = packages.symposium;"));
    assert!(!FLAKE_NIX.contains("\n              symposium\n"));
    assert!(!CONTAINER_NIX.contains("symposium"));
    assert!(!IMAGE_CHECKS_NIX.contains("symposium"));
    assert!(!LAYERS.contains("symposium"));
}

#[test]
fn image_includes_btrfs_progs_for_guest_bootstrap() {
    assert!(LAYERS.contains("pkgs.btrfs-progs"));
}

#[test]
fn image_includes_rootless_container_stacks_without_fuse_overlayfs() {
    for required in [
        "rootlessPodmanImagePackages",
        "podmanCommandCompat",
        "podman",
        "crun",
        "pkgs.conmon",
        "pkgs.netavark",
        "pkgs.aardvark-dns",
        "pkgs.passt",
        "pkgs.shadow",
        "dockerCommandCompat",
        "dockerComposeCommandCompat",
        "pkgs.docker-compose",
    ] {
        assert!(LAYERS.contains(required), "missing {required}");
    }
    for forbidden in [
        "rootlessDockerImagePackages",
        "docker ? pkgs.docker",
        "pkgs.rootlesskit",
        "pkgs.slirp4netns",
        "pkgs.nftables",
        "dockerdRootlessCompat",
        "pkgs.writeShellScriptBin \"dockerd-rootless.sh\"",
        r#"exec ${docker}/bin/docker "$@""#,
    ] {
        assert!(!LAYERS.contains(forbidden), "unexpected {forbidden}");
    }
    assert!(!LAYERS.contains("fuse-overlayfs"));
}
