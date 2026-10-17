let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
{
  cangVersion = cargoToml.workspace.package.version;

  piCodingAgent = {
    version = "1.3.0";
    owner = "earendil-works";
    repo = "pi";
    rev = "v1.3.0";
    srcHash = "sha256-AsutbwnjiwwLkB5tqLW+1GdGZyLmXsU0W+SNdNhJu7U=";
    npmDepsHash = "sha256-vNklhOW/L4liXhVujy7XIy73GoBQ7Mu+p86Nnr4fsYk=";
    aiNpmTarballHash = "sha256-U0PhY5vzQ73DNE3zrWwxwFpQtoutB6xrzhCJDUtrimE=";
  };

  # Pinned by scripts/update-herdr.sh (tag + per-system asset hashes).
  # Prebuilt mesa carrying cang's VA-API encode fixes (nix/lib/mesa-patched.nix).
  # `.github/workflows/build-mesa.yml` builds it and fills `systems` with the
  # published asset per platform; while a platform is absent, nix/lib/mesa-cang.nix
  # falls back to building the same patches from source.
  mesaPrebuiltRelease = {
    owner = "zeroqn";
    repo = "cang";
    tag = "alpha";
    version = "26.1.8";
    revision = "mesa-26.1.8";
    systems = {
      x86_64-linux = {
        asset = "mesa-26.1.8-x86_64-linux.tar.gz";
        hash = "sha256-5gR4Laz5yRvvYZYNIdFFiY8XzFKk4bTLcmPVPLYeWCk=";
      };
    };
  };

  # cang's virglrenderer source (nix/lib/virglrenderer-patched.nix): a post-1.3.0
  # upstream revision plus cang's host-side patches. The revision is main rather
  # than a release because nixpkgs still pins 1.3.0, and it drags in the vrend
  # robustness fixes since (shader/resource bounds and NULL checks, sampler-view
  # swizzle, sync-thread teardown), the glibc 2.43 build fix, and the amdgpu
  # `inttypes` fix nixpkgs otherwise carries as its own patch. Note that the
  # panfrost `res_id_count` fix in it is NOT reachable from cang: cang does not
  # support ARM/Mali hosts and the nixpkgs build's `-Ddrm-renderers` list
  # (amdgpu-experimental, asahi, msm) does not compile the panfrost renderer in.
  #
  # The revision builds venus-protocol as a meson subproject; a Nix build has no
  # network, so the wrap's checkout is pinned and vendored by
  # nix/lib/virglrenderer-patched.nix.
  virglrendererSource = {
    rev = "c8e3fc3bd2f367d92fb1027061fe51afeec5136e";
    hash = "sha256-Jo1W/6MODXBwuKio97xohfRtMMzbnUNQxUgzFTXI6bA=";
    venusProtocolTag = "v1.1.3";
    venusProtocolHash = "sha256-epSb26uLAbHYBDr+ju6rzFu4ozD/LUpRuou6hA/BFRM=";
  };

  # Prebuilt virglrenderer carrying the same patches.
  # `.github/workflows/build-virglrenderer.yml` builds it and fills `systems` with
  # the published asset per platform; while a platform is absent,
  # nix/lib/virglrenderer-cang.nix falls back to building the same source.
  # `revision` records the virglrendererSource.rev the assets were built from -
  # the upstream version does not move when the revision does.
  virglrendererPrebuiltRelease = {
    owner = "zeroqn";
    repo = "cang";
    tag = "alpha";
    version = "1.3.0-c8e3fc3b-peabcc329";
    revision = "c8e3fc3bd2f367d92fb1027061fe51afeec5136e";
    systems = {
      x86_64-linux = {
        asset = "virglrenderer-1.3.0-c8e3fc3b-peabcc329-x86_64-linux.tar.gz";
        hash = "sha256-nshbUuBPoxsr6yv9bOamMw2RVvVVI4zhTZ4venm9YdA=";
      };
    };
  };

  # The prebuilt waypipe the guest image ships. It is zeroqn/headless's published
  # `waypipe-bin` release asset - that repository's `waypipe/prebuilt-package.nix`
  # recipe, built from the upstream revision below with the fork's
  # vsock-buffer-128m patch - taken from its rolling `main-build` prerelease, so
  # the asset name (not the tag) carries the revision. The guest's waypipe server
  # and a host client built from the same asset then speak the same waypipe
  # protocol. `systems` mirrors that repository's `release-assets.json`, which
  # `scripts/update-waypipe-prebuilt.sh` refreshes.
  waypipePrebuiltRelease = {
    owner = "zeroqn";
    repo = "headless";
    tag = "main-build";
    version = "0.11.2-unstable-2026-09-26";
    revision = "6cccb84b74f2e9e5e88a77d004a58c7a87f5b97e";
    systems = {
      x86_64-linux = {
        asset = "waypipe-6cccb84b-x86_64-linux.tar.gz";
        hash = "sha256-Kea6UcUGiVgFszJeHWMluECRHFeB/sqnXfuiDxvtomw=";
      };
    };
  };

  herdrPrebuiltRelease = {
    owner = "herdrdev";
    repo = "herdr";
    tag = "v0.9.5";
    systems = {
      x86_64-linux = {
        asset = "herdr-linux-x86_64";
        hash = "sha256-FNnovItKngEY13wyJm921GHwm0A0LrI3UpYL2J9Zkp0=";
      };
      aarch64-linux = {
        asset = "herdr-linux-aarch64";
        hash = "sha256-F9dTby2lCFlSNoDvJ53/ysjkojX05G/o2mwuxEFBiXw=";
      };
    };
  };

  # Refreshed by scripts/update-fresh-prebuilt.sh (tag + per-system asset
  # hashes); that updater refuses to pin a release younger than two days.
  freshPrebuiltRelease = {
    owner = "sinelaw";
    repo = "fresh";
    tag = "v0.5.3";
    systems = {
      x86_64-linux = {
        asset = "fresh-editor-x86_64-unknown-linux-musl.tar.gz";
        hash = "sha256-Ad1A+HO5Qg35tfCicaEs7WcUDb1KmQ6egmcJHF91zbU=";
      };
      aarch64-linux = {
        asset = "fresh-editor-aarch64-unknown-linux-musl.tar.gz";
        hash = "sha256-ISlHWXilDdgQa7n1ij1o6GdAplVFBMPfNSGAhu5licY=";
      };
    };
  };

  rmuxPrebuiltRelease = {
    owner = "Helvesec";
    repo = "rmux";
    tag = "v0.10.0";
    systems = {
      x86_64-linux = {
        asset = "rmux-0.10.0-linux-x86_64.tar.gz";
        hash = "sha256-G+wR7/CMMxPDpAAZbnqT0AuK1KJPge8T3rsDNVwmlsU=";
      };
      aarch64-linux = {
        asset = "rmux-0.10.0-linux-aarch64.tar.gz";
        hash = "sha256-fpFlYOoPuQhkuMJOXQ+BtOPgsBO4qtWrU4Odfo5eGSY=";
      };
    };
  };

  # Pinned by scripts/update-dolt-prebuilt.sh (tag + per-system asset hashes).
  doltPrebuiltRelease = {
    owner = "dolthub";
    repo = "dolt";
    tag = "v2.3.2";
    systems = {
      x86_64-linux = {
        asset = "dolt-linux-amd64.tar.gz";
        hash = "sha256-eilJ+isrN5nuHlfm1kUZqNZdZ1/YMvZGnU4H5aHHKxQ=";
      };
      aarch64-linux = {
        asset = "dolt-linux-arm64.tar.gz";
        hash = "sha256-siMehOBq35XqgcboiUCe56ct5alssDu/G/NDOsdjz5w=";
      };
    };
  };

  # Pinned by scripts/update-beads-prebuilt.sh (tag + per-system asset hashes).
  beadsPrebuiltRelease = {
    owner = "gastownhall";
    repo = "beads";
    tag = "v1.3.0-rc.1";
    systems = {
      x86_64-linux = {
        asset = "beads_1.3.0-rc.1_linux_amd64.tar.gz";
        hash = "sha256-8CO25ild0W82hli6XUv/6ZHDaF3kDm3OknX6WGmr9s4=";
      };
      aarch64-linux = {
        asset = "beads_1.3.0-rc.1_linux_arm64.tar.gz";
        hash = "sha256-NMpPH3ij0nyNgzAu9mydY5bgCfcbGHB6giVTjP31DYw=";
      };
    };
  };

  # Pinned to the published `@pydantic/monty-linux-x64-gnu` npm tarball. Keep
  # the version in sync with the `@pydantic/monty` JS client the RLM extension
  # installs: client and worker reject each other over a protocol-version
  # mismatch, and upstream may build a newer protocol than the published
  # client speaks.
  montyPrebuiltRelease = {
    version = "1.1.0";
    systems = {
      x86_64-linux = {
        asset = "monty-linux-x64-gnu-1.1.0.tgz";
        hash = "sha256-o8roiBv2mqsGkBMSCFhMbIthHnGXtUcAF5pXPS/vFco=";
      };
    };
  };

  containerLibPolicySeccompJson = {
    owner = "containers";
    repo = "container-libs";
    rev = "8840603a8795210e1cc80aac1b81eb7acfa9dbee";
    path = "common/pkg/seccomp/seccomp.json";
    hash = "sha256-m3VSAlFq7ktF2dQRq4AMIP5PevlxZqk7fwfVsWwaTs0=";
  };

  libkrunfwRelease = {
    owner = "zeroqn";
    repo = "libkrunfw";
    tag = "v5.6.2-cang.4";
    systems = {
      x86_64-linux = {
        asset = "libkrunfw-x86_64-kvm-lto.tgz";
        hash = "sha256-HXGOcPCPvXcF8oiO6mdHBMnftpuTFQi5dDZ3ih57a8M=";
      };
      aarch64-linux = {
        tag = "v5.6.2-cang-lts.1";
        asset = "libkrunfw-aarch64.tgz";
        hash = "sha256-CdELT909Nf+RvGaRKHbs8+jhKZG9esuMqIBKNHnUZ6c=";
      };
      riscv64-linux = {
        tag = "v5.6.2-cang-lts.1";
        asset = "libkrunfw-riscv64.tgz";
        hash = "sha256-AF7SB+Q5ckfYt0GSviF7zTrqVzo9kqFQ4MDKeQcnc0I=";
      };
    };
  };

  cangPrebuiltRelease = {
    owner = "zeroqn";
    repo = "cang";
    # Pinned by scripts/update-cang-prebuilt.sh, which rejects wrapper-script,
    # legacy flake-locked, and concrete /nix/store/<hash>-referencing cang
    # release payloads.
    tag = "v0.14.0";
    systems = {
      x86_64-linux = {
        asset = "cang-v0.14.0-x86_64-unknown-linux-gnu";
        # Computed from `nix build .#cang-ci-sccache` normalized exactly like the
        # release workflow (`patchelf --set-interpreter
        # /lib64/ld-linux-x86-64.so.2 --set-rpath ""`), so it is the byte-identical
        # asset the tag push uploads. Recompute it the same way for the next
        # release; see the cang release scheme in docs/maintenance.md.
        hash = "sha256-dzqQucmyJcQv9iQJSf4rGgU7c1vg3DwerJR3JHmyRi8=";
      };
    };
  };

  rtkPrebuiltRelease = {
    owner = "rtk-ai";
    repo = "rtk";
    tag = "v1.0.1";
    systems = {
      x86_64-linux = {
        asset = "rtk-x86_64-unknown-linux-musl.tar.gz";
        binary = "rtk";
        hash = "sha256-W5GcO8y3mbZ6YYZtDmF0EKT83xc+JhREDlneL6IANDY=";
      };
    };
  };

  # Pinned by scripts/update-zvec-grep-source.sh (rev + srcHash). The revision
  # carries the Rust rewrite under `rust/`, which is what the image and downstream
  # consumers get as `zg`; upstream publishes no Rust binary (its release assets
  # and platform npm packages are the TypeScript build), so
  # nix/pkgs/zvec-grep.nix compiles it.
  # The Rust workspace never bumps `[workspace.package] version`, so `version` is
  # the stamp the package applies; `zvecRustLib` below is pinned to the crate
  # version that workspace's `zvec-rust`/`zvec-rust-build` deps require.
  zvecGrep = {
    version = "0.3.0";
    owner = "zvec-ai";
    repo = "zvec-grep";
    rev = "88777bd1f722a77f96ebc378897fcc262470dac9";
    srcHash = "sha256-7qSuEsfiiEGdieotYNLQ1JcWEIpPLaG0OyydzMrcPFI=";
  };

  # zvec's C API shared library (`libzvec_c_api.so` plus the jieba dictionary the
  # Rust bindings load relative to it), taken from zvec-ai/zvec-rust's own prebuilt
  # release asset for the crate version the zvec-grep workspace pins.
  # `zvec-rust-build` resolves `ZVEC_LIB_DIR` before downloading or CMake-building
  # the C++ vector database, so pointing it here keeps the ABI matched and the Nix
  # build offline.
  zvecRustLib = {
    version = "0.7.2";
    owner = "zvec-ai";
    repo = "zvec-rust";
    asset = "zvec-prebuilt-x86_64-unknown-linux-gnu.tar.gz";
    hash = "sha256-EjveZO2LquXqgTkHr1qzV1ETKB64H8pTEPKKD+T/uSs=";
  };

  # Prebuilt zg built from the zvecGrep revision above.
  # `.github/workflows/build-zg.yml` builds it and fills `systems` with the
  # published asset per platform; while a platform is absent,
  # nix/lib/zvec-grep-cang.nix falls back to building the same source, so the
  # container build stays reproducible before an asset exists.
  zgPrebuiltRelease = {
    owner = "zeroqn";
    repo = "cang";
    tag = "alpha";
    version = "0.3.0";
    revision = "88777bd1f722a77f96ebc378897fcc262470dac9";
    systems = {
      x86_64-linux = {
        asset = "zg-0.3.0-88777bd1-x86_64-linux.tar.gz";
        hash = "sha256-+0NQkfRta920mRsF70GTMQBE5j0ycn6ILrNIMQ4TZis=";
      };
    };
  };
}
