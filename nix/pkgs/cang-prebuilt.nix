{
  pkgs,
  pins,
  libkrunfw ? null,
  renderServerEnv,
}:
let
  cangVersion = pins.cangVersion;
  cangPrebuiltRelease = pins.cangPrebuiltRelease;
  prebuiltSystem = pkgs.stdenv.hostPlatform.system;
  supportedSystems = builtins.attrNames cangPrebuiltRelease.systems;
in
if builtins.hasAttr prebuiltSystem cangPrebuiltRelease.systems then
  let
    assetInfo = builtins.getAttr prebuiltSystem cangPrebuiltRelease.systems;
    legacyFlakeLockedAsset = pkgs.lib.hasSuffix "-linux-flake-locked" assetInfo.asset;
  in
  if legacyFlakeLockedAsset then
    throw ''
      cang-prebuilt is pinned to legacy asset ${assetInfo.asset} from ${cangPrebuiltRelease.tag}.
      Legacy pre-rename `*-linux-flake-locked` assets embed release-builder /nix/store references and are intentionally unsupported.
      Publish a neutral cang-<arch>-unknown-linux-gnu sha-* release asset, then rerun scripts/update-cang-prebuilt.sh.
    ''
  else
    let
      releaseUrl = "https://github.com/${cangPrebuiltRelease.owner}/${cangPrebuiltRelease.repo}/releases/download/${cangPrebuiltRelease.tag}/${assetInfo.asset}";
      runtimeTools = [
        pkgs.buildah
        pkgs.btrfs-progs
        pkgs.fuse-overlayfs
        pkgs.passt
        pkgs.util-linux
      ];
      # The values live once in nix/lib/render-server-env.nix (also published as
      # packages.cang-render-server-env) so the released wrapper and the
      # sourceable file the chromium GPU smoke uses cannot drift apart.
      renderServerWrapperArgs = pkgs.lib.concatMap (name: [
        "--set"
        name
        renderServerEnv.${name}
      ]) (pkgs.lib.attrNames renderServerEnv);
    in
    pkgs.stdenvNoCC.mkDerivation {
      pname = "cang";
      version = "${cangVersion}-prebuilt-${cangPrebuiltRelease.tag}";
      src = pkgs.fetchurl {
        url = releaseUrl;
        hash = assetInfo.hash;
      };
      dontUnpack = true;

      nativeBuildInputs = [
        pkgs.autoPatchelfHook
        pkgs.binutils
        pkgs.makeWrapper
      ];

      buildInputs = [
        pkgs.stdenv.cc.cc.lib
        pkgs.stdenv.cc.libc
        # Since cang links libkrun's Rust API, the released ELF has a
        # `libvirglrenderer.so.1` DT_NEEDED (its release-time rpath is stripped),
        # so autoPatchelfHook has to resolve it here. Consumers that run the bare
        # asset without Nix have to provide that library themselves; the release
        # notes say so.
        pkgs.virglrenderer
      ];

      propagatedUserEnvPkgs = runtimeTools;

      installPhase = ''
        runHook preInstall

        magic="$(dd if="$src" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
        if [ "$magic" != "7f454c46" ]; then
          echo "cang-prebuilt expected a neutral raw ELF payload, but ${assetInfo.asset} from ${cangPrebuiltRelease.tag} is not ELF" >&2
          echo "Do not pin wrapper-script release assets; rerun scripts/update-cang-prebuilt.sh after a neutral sha-* release is published." >&2
          exit 1
        fi
        readelf -h "$src" >/dev/null

        install -Dm755 "$src" "$out/bin/cang"
        install -Dm644 ${../../crates/cang/assets/seccomp/default.json} "$out/share/cang/seccomp/default.json"
        install -Dm644 ${../../crates/cang/assets/seccomp/render-server.json} "$out/share/cang/seccomp/render-server.json"
        mkdir -p "$out/libexec/cang-helpers" "$out/lib/cang"
        ln -s ${pkgs.buildah}/bin/buildah "$out/libexec/cang-helpers/buildah"
        ln -s ${pkgs.btrfs-progs}/bin/btrfs "$out/libexec/cang-helpers/btrfs"
        ln -s ${pkgs.btrfs-progs}/bin/mkfs.btrfs "$out/libexec/cang-helpers/mkfs.btrfs"
        ln -s ${pkgs.util-linux}/bin/blkid "$out/libexec/cang-helpers/blkid"
        ln -s ${pkgs.passt}/bin/pasta "$out/libexec/cang-helpers/pasta"
        ln -s ${pkgs.passt}/bin/passt "$out/libexec/cang-helpers/passt"
        ln -s ${pkgs.virglrenderer}/libexec/virgl_render_server "$out/libexec/cang-helpers/virgl_render_server"
        ${pkgs.lib.optionalString (libkrunfw != null) ''
          for library in ${pkgs.lib.getLib libkrunfw}/lib/libkrunfw.so*; do
            ln -s "$library" "$out/lib/cang/$(basename "$library")"
          done
        ''}
        wrapProgram "$out/bin/cang" \
          --prefix LD_LIBRARY_PATH : "$out/lib/cang" \
          ${pkgs.lib.escapeShellArgs renderServerWrapperArgs}

        runHook postInstall
      '';

      passthru = {
        inherit releaseUrl;
        releaseTag = cangPrebuiltRelease.tag;
      };

      meta = {
        description = "Prebuilt neutral dynamic cang binary patched with package-relative runtime helpers";
        homepage = "https://github.com/${cangPrebuiltRelease.owner}/${cangPrebuiltRelease.repo}";
        license = pkgs.lib.licenses.mit;
        mainProgram = "cang";
        platforms = supportedSystems;
        sourceProvenance = [ pkgs.lib.sourceTypes.binaryNativeCode ];
      };
    }
else
  throw ''
    cang-prebuilt is not pinned for ${prebuiltSystem}.
    Publish a neutral cang-<arch>-unknown-linux-gnu sha-* release asset and run scripts/update-cang-prebuilt.sh.
    Supported systems: ${pkgs.lib.concatStringsSep ", " supportedSystems}
  ''
