{
  pkgs,
  cangMuslPackage,
  configPayloads,
  layers,
  onnxruntimeShim,
}:

let
  nixConfig = import ./nix-config.nix;
  commonEnv = [
    "HOME=/home/dev"
    "USER=dev"
    "SHELL=${pkgs.fish}/bin/fish"
    "LIBCLANG_PATH=${pkgs.libclang.lib}/lib"
    "PATH=/home/dev/.codex/bin:/home/dev/.nix-profile/bin:/nix/var/nix/profiles/default/bin:${layers.imagePath}:${cangMuslPackage}/bin"
    "NIX_CONFIG=${nixConfig}"
    "NIX_SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=${layers.clangWildWrapper}/bin/clang_wild_wrapper"
    "RUST_SRC_PATH=${layers.rustSourceImage}/share/rust-src"
    "RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache"
    "CMAKE_C_COMPILER_LAUNCHER=${pkgs.sccache}/bin/sccache"
    "CMAKE_CXX_COMPILER_LAUNCHER=${pkgs.sccache}/bin/sccache"
  ]
  ++ montyEnv;

  # The RLM extension resolves the monty worker from MONTY_BIN first, then from
  # the @pydantic/monty platform package in node_modules, and only then from
  # PATH. Pin it to the packaged worker so a stale node_modules payload cannot
  # shadow it (worker and JS client reject a protocol-version mismatch).
  montyEnv = pkgs.lib.optionals (layers.montyPackage != null) [
    "MONTY_BIN=${layers.montyPackage}/bin/monty"
  ];

  # The ONNX Runtime the ABI shim in the pi wrapper (`libonnxruntime.so.1` /
  # `libonnxrtabi24.so.1`) loads. The shim falls back to the same directory when
  # the variable is not exported, so this is what makes the choice inspectable
  # and replaceable in the guest - and it is the reason the variable is on
  # cang's image-env allowlist. See ADR 0009.
  #
  # `ONNXRUNTIME_NODE_INSTALL=skip` keeps the published `onnxruntime-node`
  # postinstall from fetching its ~301 MiB of CUDA/TensorRT providers, which
  # nothing in this image requests (the binding runs CPU-only). The image's own
  # build already installs npm dependencies with `--ignore-scripts`, so this is
  # for a later `npm install` of that package inside the guest.
  onnxRuntimeEnv = [
    "ONNXRUNTIME_LIB_PATH=${onnxruntimeShim.onnxruntimeLibDir}"
    "ONNXRUNTIME_NODE_INSTALL=skip"
  ];

  cangEnv = [
    "CANG_FISH_CONFIG_SOURCE=${configPayloads.fishConfig}/share/cang/fish/conf.d/cang-starship.fish"
    "CANG_STARSHIP_CONFIG_SOURCE=${configPayloads.starshipConfig}/share/cang/starship.toml"
    "CANG_MIMALLOC_LIB=${layers.mimallocLib}"
    "CANG_GRAPHENE_HARDENED_MALLOC_LIB=${layers.hardenedMallocLib}"
    "CANG_REAL_PODMAN=${layers.realPodmanBin}"
  ];
in
{
  Entrypoint = [
    "${cangMuslPackage}/bin/cang-guest-init"
    "enter"
    "--"
  ];
  WorkingDir = "/workspace";
  Env = commonEnv ++ cangEnv ++ onnxRuntimeEnv;
}
