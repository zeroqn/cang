# Build outputs and Nix DB diagnostics

## Build outputs

Source builds of the host package compile libkrun's Rust API from the fork
checkouts the flake pins as its `libkrun-src`, `libkrunfw-src` and
`rutabaga-gfx-src` inputs, kept in `flake.lock`; `nix/pkgs/workspace-src.nix`
grafts them into `deps/` of the workspace source, so a Nix build needs no
submodules - not from a local checkout, not in CI, and not for a `github:`
consumer. The `deps/libkrun`, `deps/libkrunfw` and `deps/rutabaga_gfx` submodules
stay for in-tree `cargo` builds (the workspace `[patch]` points libkrun's
`rutabaga_gfx` dependency at the third one), so a fresh checkout still wants
`git submodule update --init --recursive` before `cargo build`. A local fork edit
goes to the input instead of the checkout:
`nix build .#cang --override-input libkrun-src "git+file://$PWD/deps/libkrun"`
(or `rutabaga-gfx-src`, `libkrunfw-src`).

- `.#cang`: compile the workspace Rust host package with `$out/bin/cang` as a
  raw dynamic ELF. libkrun is *compiled in* from the `libkrun-src` input grafted
  into `deps/libkrun` (the `cang-libkrun` crate binds libkrun's Rust API), so
  this output builds libkrun too - it needs the bindgen hook, `pkg-config`,
  `virglrenderer`, `gbm` and the musl guest init blob
  (`nix/pkgs/libkrun-source.nix`). Runtime helpers
  are installed under `$out/libexec/cang-helpers`, the firmware
  (`libkrunfw.so*`) under `$out/lib/cang` with an `$ORIGIN/../lib/cang` rpath on
  the binary, and cang needs no wrapper script or duplicate payload.
- `.#cang-dev`: the same host package built against libkrunfw compiled from the
  `libkrunfw-src` input instead of the pinned release asset
  (`useLocalSource = true` in `nix/pkgs/libkrunfw.nix`), for local kernel
  configuration experiments; the kernel build is slow, so this target is only
  worth it for that. Point it at the checkout with
  `--override-input libkrunfw-src "git+file://$PWD/deps/libkrunfw"`.
- `.#cang-prebuilt`: install a pinned published neutral dynamic Linux `cang`
  asset as raw `$out/bin/cang`, patch ordinary ELF runtime dependencies with
  Nix, and provide the same package-relative helper and `$out/lib/cang`
  library layout as source-built `.#cang`. Assets published before cang linked
  libkrun still need `libkrun*.so` from the pinned `libkrun` package, which is
  why this packager keeps that wiring.
- `.#cang-render-server-env`: the host render-server environment
  (`CANG_MESA_LIBDIR`, `CANG_MESA_ICD`, `CANG_VULKAN_LOADER_LIBDIR`) a
  tree-built raw-ELF `.#cang` needs to find mesa and the Vulkan loader for
  `virgl_render_server`, as a sourceable `$out/render-server-env.sh`. The values
  are defined once in `nix/lib/render-server-env.nix`, which `.#cang-prebuilt`'s
  wrapper also bakes in; the file's assignments only fill unset variables, so a
  value the caller exported wins. `tools/chromium-cang-smoke` sources it, which
  is what lets its no-argument invocation run the default `.#cang`.
- `.#cang-musl`: static/musl `cang-guest-init` (and `cang-granted`) binaries
  for image/guest use. It intentionally does not build or expose `bin/cang`;
  the host `cang` binary is a dynamically linked ELF (libkrun itself is linked
  in statically, but libc and the GPU stack are not), and it opens
  `libkrunfw.so` from the package or dev shell runtime library path.
- `.#rmux-prebuilt`: install the pinned published Helvesec/rmux Linux release
  tarball for the current system. The cang image includes this package as
  `rmux` alongside Nixpkgs `tmux`.
- `.#rio-bin` (`x86_64-linux`): install the pinned `zeroqn/headless` Rio package.
  The x86_64 cang image includes `rio` and installs its `rio` and `xterm-rio`
  terminfo entries in `/home/dev/.terminfo`, so managed guest shells can use
  either Rio terminal identity without additional guest setup. Upstream does
  not currently publish this package for `aarch64-linux`.
- `.#rtk-prebuilt`: install the pinned published RTK release asset (currently
  pinned for `x86_64-linux`).
- `.#herdr-prebuilt`: install the pinned published `herdrdev/herdr` Linux
  release binary (static-PIE) for the current system. The cang image includes
  this package as `herdr` in the agent layer.
- `.#fresh-prebuilt`: install the pinned `sinelaw/fresh` static-musl Linux
  release tarball for the current system. The cang image includes this package
  as `fresh` in the agent layer.
- `.#zvec-grep` (`x86_64-linux`): install the pinned `zvec-ai/zvec-grep` (`zg`)
  hybrid workspace search CLI from the GitHub source archive, wrapped around
  Nixpkgs Node.js. The agent layer keeps the glibc x86_64 native payloads and
  prunes the musl, CUDA, and cross-arch copies the image cannot load.
- `.#dolt-prebuilt`: install the pinned `dolthub/dolt` Linux release tarball
  binary for the current system. The cang image includes this package as
  `dolt` in the agent layer.
- `.#beads-prebuilt`: install the pinned `gastownhall/beads` Linux release
  tarball binary for the current system, patched with Nix to use the image's
  glibc and libstdc++. The cang image includes this package as `bd` in the
  agent layer.
- `.#monty-prebuilt` (`x86_64-linux`): install the pinned published
  `@pydantic/monty-linux-x64-gnu` npm tarball's `monty` worker (the sandboxed
  Python interpreter the RLM extension spawns), patched with Nix to use the
  image's glibc and libstdc++. The image includes this package as `monty` in
  the agent layer and exports `MONTY_BIN` pointing at it, so the extension uses
  the store worker instead of the platform package it may find in
  `node_modules`. Pin the version in lockstep with the `@pydantic/monty` JS
  client: client and worker reject each other over a protocol-version mismatch.
- `.#libkrunfw`: install the pinned `zeroqn/libkrunfw` release asset for the
  current system.
- libkrun itself has no package output any more. `.#cang` compiles libkrun's
  Rust API from the `libkrun-src` input (see the source-build note above), so
  there is nothing to pin or install: that input's revision is the version, it
  lives in `flake.lock`, and it moves together with the `deps/libkrun` submodule
  pointer - see
  [the maintenance procedure](maintenance.md#updating-the-libkrun-fork).
- `.#virglrenderer`: the nixpkgs `virglrenderer` with this repo's host-side
  patches (`virglrenderer-enum-26.patch`,
  `virglrenderer-gbm-layout-linear-modifier.patch`,
  `virglrenderer-encode-raw-headers.patch` and
  `virglrenderer-encode-upload-fence.patch`, applied by the overlay in
  `nix/lib/systems.nix`). Host-side only: cang links `libvirglrenderer.so.1`
  and the `virgl_render_server` helper is symlinked from this package, so the
  cang packages already ship it; downstream flakes that build their own host
  vrend/libkrun stack should consume this output instead of nixpkgs'
  `virglrenderer`. The encode patch is the host half of the guest VA-API encode
  fix - it submits the guest client's packed parameter sets to the host's VA
  driver - and needs the image-local guest driver described below.
- The guest's VA-API driver is not a flake output: it is image-local. The
  image's mesa is a prebuilt binary drop that cannot be patched, so
  `nix/lib/systems.nix` builds `mesaVaApi` (nixpkgs' mesa with only the `virgl`
  gallium driver, plus `mesa-virgl-encode-raw-headers.patch`), `nix/image/layers.nix`
  exposes it as the `cang-va-runtime` driver directory and the image links that at
  `/usr/lib/cang-va-runtime`, which guest-init puts first in
  `LIBVA_DRIVERS_PATH`. It is the guest half of the host `virglrenderer`
  encode patch above, and needs that patch on the host side; the image's GL and
  Vulkan keep using the pinned prebuilt mesa.
- `.#podman`: the nixpkgs Podman package, re-exported for downstream flakes and
  for the image, which ships it with the nixpkgs `crun` runtime.
- `.#container-lib-policy-seccomp-json`: install the pinned
  `containers/container-libs` `common/pkg/seccomp/seccomp.json` policy at
  `share/containers/seccomp.json` for downstream flakes or image reuse.
- `.#container`: cang Podman image archive named `localhost/cang:latest`;
  includes rootless Podman tooling such as Podman, Buildah, crun, netavark,
  aardvark-dns, passt, and docker-compose, and Nix formatting tooling such as
  `nixfmt`.

## Nix store / DB diagnostics

`nix build .#container` depends on a static image metadata linter before running
the layered image build command. To run only that linter:

```bash
nix build .#checks.$(nix eval --raw --impure --expr builtins.currentSystem).container-nix-db-metadata
```

The check compares store paths referenced by the image Docker config/env against
the `pkgs.closureInfo { rootPaths = layers.imageContents; }` store-path list.
That is the same closure Docker Tools loads into the image Nix DB when
`includeNixDB = true`. It fails fast when image metadata can pull a store path
into `/nix/store` without that path being covered by generated image Nix DB
metadata. This check does not inspect or mutate the host Nix DB.

Inside a cang container, run the packaged live DB scanner manually:

```bash
cang-nix-store-db-check
```

The runtime checker compares present `/nix/store/<hash>-name` entries with
`nix path-info --all`, ignores the internal `/nix/store/.links` link farm and
transient `*.lock` files, and prints `nix-store --verify-path` evidence for
present-but-invalid paths. When the libkrun Nix disk upperdir is visible at
`/run/cang/nix-disk/upper`, failures also compare each invalid store object
with `/run/cang/nix-disk/upper/store/<name>` and report whether that store-layer
object is present in the upperdir or not found there. This is store-layer
evidence only, not root-cause proof: absence from the upperdir is not proof that
lower image metadata is correct or that the lower image is at fault. It is
diagnostic only and never repairs or mutates the Nix DB.
