{
  pkgs,
  piCodingAgent,
  rioBin,
  herdrPrebuilt,
  montyPrebuilt,
  rmuxPrebuilt,
  rtkPrebuilt,
  zvecGrep,
  doltPrebuilt,
  beadsPrebuilt,
  containerLibPolicySeccompJson,
  libkrunfw,
  wl-cross-domain-proxy,
  bun,
  cangMuslPackage,
}:
let
  nixConfig = import ./nix-config.nix;
  configPayloads = import ./config-payloads.nix { inherit pkgs; };
  layers = import ./layers.nix {
    inherit
      pkgs
      piCodingAgent
      rioBin
      herdrPrebuilt
      montyPrebuilt
      rmuxPrebuilt
      rtkPrebuilt
      zvecGrep
      doltPrebuilt
      beadsPrebuilt
      containerLibPolicySeccompJson
      libkrunfw
      wl-cross-domain-proxy
      bun
      cangMuslPackage
      ;
    fishConfig = configPayloads.fishConfig;
    starshipConfig = configPayloads.starshipConfig;
  };
  imageConfig = import ./config.nix {
    inherit
      pkgs
      cangMuslPackage
      configPayloads
      layers
      ;
  };
  imageChecks = import ./checks.nix {
    inherit
      pkgs
      piCodingAgent
      rioBin
      herdrPrebuilt
      montyPrebuilt
      rmuxPrebuilt
      rtkPrebuilt
      zvecGrep
      doltPrebuilt
      beadsPrebuilt
      containerLibPolicySeccompJson
      libkrunfw
      wl-cross-domain-proxy
      bun
      cangMuslPackage
      ;
  };
  image = pkgs.dockerTools.buildLayeredImage {
    name = "localhost/cang";
    tag = "latest";
    maxLayers = layers.cangImageMaxLayers;
    contents = layers.imageContents;
    includeNixDB = true;
    layeringPipeline = layers.cangImageLayeringPipeline;
    fakeRootCommands = ''
      mkdir -p \
        ./etc \
        ./etc/containers \
        ./etc/nix \
        ./home/dev/.cache \
        ./home/dev/.codex \
        ./home/dev/.terminfo/r \
        ./home/dev/.terminfo/x \
        ./root \
        ./tmp \
        ./var/empty \
        ./workspace
      chmod 1777 ./tmp
      if [ ! -e ./etc/passwd ]; then
        printf 'root:x:0:0:root:/root:/bin/sh\n' > ./etc/passwd
      else
        sed -i '/^dev:/d' ./etc/passwd
      fi
      if [ ! -e ./etc/group ]; then
        printf 'root:x:0:\n' > ./etc/group
      fi
      printf '%s\n' '${layers.mimallocLib}' > ./etc/ld-nix.so.preload
      chmod 0644 ./etc/ld-nix.so.preload
      cat > ./etc/nix-allocator-libs <<EOF_NIX_ALLOCATOR_LIBS
      mimalloc=${layers.mimallocLib}
      hardened=${layers.hardenedMallocLib}
      EOF_NIX_ALLOCATOR_LIBS
      chmod 0644 ./etc/nix-allocator-libs
      cat > ./etc/containers/containers.conf <<'EOF_CONTAINERS_CONF'
      [containers]
      seccomp_profile = "${containerLibPolicySeccompJson}/share/containers/seccomp.json"
      EOF_CONTAINERS_CONF
      chmod 0644 ./etc/containers/containers.conf
      cat > ./etc/nix/nix.conf <<'EOF_NIX_CONF'
      ${nixConfig}
      EOF_NIX_CONF
      chmod 0644 ./etc/nix/nix.conf
      cat > ./etc/rmux.conf <<'EOF_RMUX_CONF'
      set -g mouse off
      bind T if-shell -F '#{mouse}' 'set -g mouse off ; display-message "mouse OFF: native terminal selection enabled"' 'set -g mouse on ; display-message "mouse ON: pane mouse mode enabled"'

      # Quality-of-life.
      set -g history-limit 100000
      set -g renumber-windows on
      set -g base-index 1
      setw -g pane-base-index 1
      setw -g mode-keys vi
      set -g status-keys vi

      bind | split-window -h -c "#{pane_current_path}"
      bind - split-window -v -c "#{pane_current_path}"
      bind c new-window -c "#{pane_current_path}"
      bind h select-pane -L
      bind j select-pane -D
      bind k select-pane -U
      bind l select-pane -R
      EOF_RMUX_CONF
      chmod 0644 ./etc/rmux.conf
      cat > ./etc/tmux.conf <<'EOF_TMUX_CONF'
      set -g mouse on
      bind-key | split-window -h
      bind-key - split-window -v
      bind-key h select-pane -L
      bind-key l select-pane -R
      bind-key j select-pane -D
      bind-key k select-pane -U
      EOF_TMUX_CONF
      chmod 0644 ./etc/tmux.conf
      cp ${pkgs.ghostty.terminfo}/share/terminfo/x/xterm-ghostty ./home/dev/.terminfo/x/xterm-ghostty
      chmod 0644 ./home/dev/.terminfo/x/xterm-ghostty
      mkdir -p ./usr/lib
      ln -s ${pkgs.mesa} ./usr/lib/cang-mesa-runtime
      ln -s ${pkgs.mesa} ./usr/lib/cang-software-renderer
      ln -s ${pkgs.fontconfig.out} ./usr/lib/cang-fontconfig
      ${pkgs.lib.optionalString (rioBin != null) ''
        cp ${rioBin}/share/terminfo/r/rio ./home/dev/.terminfo/r/rio
        cp ${rioBin}/share/terminfo/x/xterm-rio ./home/dev/.terminfo/x/xterm-rio
        chmod 0644 ./home/dev/.terminfo/r/rio ./home/dev/.terminfo/x/xterm-rio
      ''}
      if ! grep -q '^nixbld:' ./etc/group; then
        printf 'nixbld:x:${toString layers.nixBuilderGroupId}:${layers.nixBuilderGroupMembers}\n' >> ./etc/group
      fi
      cat >> ./etc/passwd <<'EOF_PASSWD'
      ${layers.nixBuilderPasswdEntries}
      EOF_PASSWD
      chown 0:nixbld ./nix/store
      chmod 1775 ./nix/store
      chown -R 1000:1000 ./home/dev ./workspace
    '';

    config = builtins.fromJSON (builtins.unsafeDiscardStringContext (builtins.toJSON imageConfig));
  };
in
if imageChecks.missingImageConfigNixDbRefs != [ ] then
  builtins.throw imageChecks.missingRefsMessage
else
  image.overrideAttrs (old: {
    buildCommand = ''
      echo "checking cang image config Nix DB metadata coverage"
      test -e ${imageChecks.imageConfigNixDbRefs}/passed
      echo "checking cang image wrapper contracts"
      test -e ${imageChecks.wrapperContracts}/passed
    ''
    + (old.buildCommand or "");
  })
