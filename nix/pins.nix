let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
{
  cangVersion = cargoToml.workspace.package.version;

  piCodingAgent = {
    version = "0.87.1";
    owner = "earendil-works";
    repo = "pi";
    rev = "v0.87.1";
    srcHash = "sha256-GUhlq6t+l6iiViOZ0bkV28v3ZDqcLvEwpZpYZ5JAyDk=";
    npmDepsHash = "sha256-Pktim/DwSKoKi+p9C2ID+ioma9RWWb3hgDSF5zfp9o4=";
    aiNpmTarballHash = "sha256-NbRDLyfMJmX4a+67mvajmxJRlwiDwwRL2L5PToxzHKA=";
  };

  # Pinned by scripts/update-herdr.sh (tag + per-system asset hashes).
  herdrPrebuiltRelease = {
    owner = "herdrdev";
    repo = "herdr";
    tag = "v0.9.1";
    systems = {
      x86_64-linux = {
        asset = "herdr-linux-x86_64";
        hash = "sha256-KgL+0WvrZR7wBuHUPwSPZSyk3FitBTzS1ERQVj1cVLc=";
      };
      aarch64-linux = {
        asset = "herdr-linux-aarch64";
        hash = "sha256-9Mz03nRfLLmjmpg+m6NwPa1Q7CpY3qgwJs6rchu9jZ4=";
      };
    };
  };

  # Refreshed by scripts/update-fresh-prebuilt.sh (tag + per-system asset
  # hashes); that updater refuses to pin a release younger than two days.
  freshPrebuiltRelease = {
    owner = "sinelaw";
    repo = "fresh";
    tag = "v0.5.2";
    systems = {
      x86_64-linux = {
        asset = "fresh-editor-x86_64-unknown-linux-musl.tar.gz";
        hash = "sha256-Ld4dTn9ZVv2/9cNlYVJcmM7OJCLihJN8iX/y6xfEA30=";
      };
      aarch64-linux = {
        asset = "fresh-editor-aarch64-unknown-linux-musl.tar.gz";
        hash = "sha256-pg+h6xm8SDt1hu//Ig03+zz9B58bHLNKSKGhmwnfXwg=";
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
    version = "1.0.0";
    systems = {
      x86_64-linux = {
        asset = "monty-linux-x64-gnu-1.0.0.tgz";
        hash = "sha256-JR2VG0JYPJw0I12P7hO3Ig5sXIaeDGSNz9XA/itlaMw=";
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
    tag = "v5.6.2-cang.3";
    systems = {
      x86_64-linux = {
        asset = "libkrunfw-x86_64-kvm-lto.tgz";
        hash = "sha256-a4/E+cnMko5ztbnb4GxA2dsObv/C3EsF2j2oTpzmiCo=";
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
    tag = "v0.11.2";
    systems = {
      x86_64-linux = {
        asset = "cang-v0.11.2-x86_64-unknown-linux-gnu";
        # Computed from `nix build .#cang-ci-sccache` normalized exactly like the
        # release workflow (`patchelf --set-interpreter
        # /lib64/ld-linux-x86-64.so.2 --set-rpath ""`), so it is the byte-identical
        # asset the tag push uploads. Recompute it the same way for the next
        # release; see the cang release scheme in docs/maintenance.md.
        hash = "sha256-KQ8QPOCZnwoKvzhKjdDriKmPHtEKz/zfvrqGTFF0CJ0=";
      };
    };
  };

  rtkPrebuiltRelease = {
    owner = "rtk-ai";
    repo = "rtk";
    tag = "v0.49.0";
    systems = {
      x86_64-linux = {
        asset = "rtk-x86_64-unknown-linux-musl.tar.gz";
        binary = "rtk";
        hash = "sha256-cngjHf1+anMKSrf4R7GVvPAiicLVdiKw2rdaZBEQDI8=";
      };
    };
  };

  # Pinned by scripts/update-zvec-grep.sh (srcHash + npmDepsHash).
  zvecGrep = {
    version = "0.2.0";
    owner = "zvec-ai";
    repo = "zvec-grep";
    rev = "v0.2.0";
    srcHash = "sha256-2o/6QWyeZqOy7O8ikO8puqMXmtvWdjS9Y1rNW/SD/Bc=";
    npmDepsHash = "sha256-xEK245edmpn5yG2cT0b8/X6ONs4KmLNxxN1jRt5RZe0=";
  };
}
