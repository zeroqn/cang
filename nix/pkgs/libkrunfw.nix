{
  lib,
  stdenv,
  stdenvNoCC,
  fetchurl,
  pins,
  bc,
  binutils,
  bison,
  cpio,
  curl,
  elfutils,
  flex,
  gawk,
  gnugrep,
  gnumake,
  gnused,
  gnutar,
  gzip,
  lz4,
  ncurses,
  openssl,
  patch,
  perl,
  pkg-config,
  python3,
  sccache,
  util-linux,
  xz,
  zlib,
  useLocalSource ? false,
  # The `deps/libkrunfw` fork checkout, passed in by the flake as its
  # `libkrunfw-src` input (see nix/pkgs/workspace-src.nix). Only the local
  # kernel build reads it.
  libkrunfwSrc,
  variant ? null,
}:

assert lib.elem variant [ null ];

let
  system = stdenv.hostPlatform.system;
  release = pins.libkrunfwRelease;
  systemPins = release.systems.${system} or (throw "unsupported libkrunfw system: ${system}");
  # A line's assets are published per branch: the newest-kernel line (`cang`)
  # carries x86_64 while the LTS line (`cang-lts`) carries the other
  # architectures, so a system may name its own release. `tag` is the primary
  # release otherwise - the one the cang release gate checks.
  systemTag = systemPins.tag or release.tag;

  prebuilt = stdenvNoCC.mkDerivation {
    pname = "libkrunfw";
    version = release.tag;

    src = fetchurl {
      url = "https://github.com/${release.owner}/${release.repo}/releases/download/${systemTag}/${systemPins.asset}";
      hash = systemPins.hash;
    };

    sourceRoot = ".";

    installPhase = ''
      runHook preInstall

      mkdir -p $out/lib
      if [ -d lib64 ]; then
        cp -a lib64/. $out/lib/
      else
        cp -a libkrunfw.so* $out/lib/
      fi

      runHook postInstall
    '';

    meta = {
      description = "Pinned prebuilt libkrunfw guest payload shared library for cang";
      homepage = "https://github.com/${release.owner}/${release.repo}";
      license = with lib.licenses; [
        lgpl2Only
        lgpl21Only
      ];
      platforms = lib.attrNames release.systems;
    };
  };

  kernelVersion = "linux-7.2.7";
  kernelHardenedVersion = "v7.2.7-hardened1";

  kernelTarball = fetchurl {
    url = "https://cdn.kernel.org/pub/linux/kernel/v7.x/${kernelVersion}.tar.xz";
    hash = "sha256-SsNMR9slQP+ycTlD+NiR/xcC4LppNFJaSTt9HK1DFFo=";
  };

  kernelHardenedPatch = fetchurl {
    url = "https://github.com/anthraxx/linux-hardened/releases/download/${kernelHardenedVersion}/linux-hardened-${kernelHardenedVersion}.patch";
    hash = "sha256-6AZcuBsr6Ax26Z0sdBUhYqwo8cndegrq+W7Jwt1x2MI=";
  };

  python = python3.withPackages (pythonPackages: [
    pythonPackages.pyelftools
  ]);

  localSource = stdenv.mkDerivation {
    pname = "libkrunfw";
    version = "${release.tag}-local";

    src = libkrunfwSrc;

    nativeBuildInputs = [
      bc
      binutils
      bison
      cpio
      curl
      elfutils
      flex
      gawk
      gnugrep
      gnumake
      gnused
      gnutar
      gzip
      lz4
      ncurses
      openssl
      patch
      perl
      pkg-config
      python
      sccache
      util-linux
      xz
      zlib
    ];

    preBuild = ''
            mkdir -p tarballs
            ln -sf ${kernelTarball} tarballs/${kernelVersion}.tar.xz
            ln -sf ${kernelHardenedPatch} tarballs/linux-hardened-${kernelHardenedVersion}.patch
            cp config-libkrunfw_x86_64-kvm config-libkrunfw_x86_64

            export SCCACHE_DIR="''${SCCACHE_DIR:-$NIX_BUILD_TOP/sccache}"
            mkdir -p "$SCCACHE_DIR"

            mkdir -p .nix-sccache-wrappers
            cat > .nix-sccache-wrappers/cc <<EOF
      #!${stdenv.shell}
      exec ${sccache}/bin/sccache ${stdenv.cc}/bin/cc "\$@"
      EOF
            cat > .nix-sccache-wrappers/cxx <<EOF
      #!${stdenv.shell}
      exec ${sccache}/bin/sccache ${stdenv.cc}/bin/c++ "\$@"
      EOF
            chmod +x .nix-sccache-wrappers/cc .nix-sccache-wrappers/cxx

            makeFlagsArray+=(
              "CC=$PWD/.nix-sccache-wrappers/cc"
              "HOSTCC=$PWD/.nix-sccache-wrappers/cc"
              "CXX=$PWD/.nix-sccache-wrappers/cxx"
              "HOSTCXX=$PWD/.nix-sccache-wrappers/cxx"
            )
    '';

    makeFlags = [
      "PREFIX=${placeholder "out"}"
    ];

    installPhase = ''
      runHook preInstall

      make PREFIX=$out install

      runHook postInstall
    '';

    meta = {
      description = "Local libkrunfw guest payload shared library for cang";
      homepage = "https://github.com/${release.owner}/${release.repo}";
      license = with lib.licenses; [
        lgpl2Only
        lgpl21Only
      ];
      platforms = [ "x86_64-linux" ];
    };
  };
in
if useLocalSource then
  if system == "x86_64-linux" then
    localSource
  else
    throw "local libkrunfw source build is only supported on x86_64-linux"
else
  prebuilt
