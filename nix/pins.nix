let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
{
  cangVersion = cargoToml.workspace.package.version;

  piCodingAgent = {
    version = "0.85.1";
    owner = "earendil-works";
    repo = "pi";
    rev = "v0.85.1";
    srcHash = "sha256-gU8BSiqqOYt2RRuQONHHGvZeSM5KFQVrwif9bmuUXUc=";
    npmDepsHash = "sha256-6/CE7cCSopNH7cUJDkRLunhhiFDgYkhKi6QRBx8zwes=";
    aiNpmTarballHash = "sha256-r30RmGF5RFzm/oizfVfeIvgjwP/TplyuMcVVt/XpklM=";
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
    version = "0.0.23";
    systems = {
      x86_64-linux = {
        asset = "monty-linux-x64-gnu-0.0.23.tgz";
        hash = "sha256-q1ftin57G3vAMydqTWbimaDtSlDmrErbTLgFCv4sCSA=";
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

  libkrunRelease = {
    owner = "zeroqn";
    repo = "libkrun";
    tag = "v1.19.5-cang.1";
    systems = {
      x86_64-linux = {
        asset = "libkrun-x86_64-linux-full.tgz";
        hash = "sha256-5/tfcESC6468v21iEFROtnSkZyUKPoHfj/7p1t41EMc=";
      };
      aarch64-linux = {
        asset = "libkrun-aarch64-linux-full.tgz";
        hash = "sha256-72IXB9VjJ8D2qGqIPKhporJaXF6Pn2zULeIahiZAkLA=";
      };
    };
  };

  libkrunfwRelease = {
    owner = "zeroqn";
    repo = "libkrunfw";
    tag = "v5.6.2-cang.1";
    systems = {
      x86_64-linux = {
        asset = "libkrunfw-x86_64-kvm-lto.tgz";
        hash = "sha256-xPXOwubYEqswiHpiBiCuBH+QnnxvRCvJdTBo8H5we/s=";
      };
      aarch64-linux = {
        asset = "libkrunfw-aarch64.tgz";
        hash = "sha256-J29n45sfIsCkv8XOWVMAmInIq3yFDqjcWIKwZYzhAfI=";
      };
      riscv64-linux = {
        asset = "libkrunfw-riscv64.tgz";
        hash = "sha256-OCdFrXmoqlauC0nnC3QWEmW5DS1PyA8uN/r/gBrSJlg=";
      };
    };
  };

  cangPrebuiltRelease = {
    owner = "zeroqn";
    repo = "cang";
    # Pinned by scripts/update-cang-prebuilt.sh, which rejects wrapper-script,
    # legacy flake-locked, and concrete /nix/store/<hash>-referencing cang
    # release payloads.
    tag = "sha-98933ee84614";
    systems = {
      x86_64-linux = {
        asset = "cang-x86_64-unknown-linux-gnu";
        hash = "sha256-9HBQ7tSZ+l3Cm5pSnpqiT5l0Xxnl1Lr8I+AHKogNqpM=";
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
