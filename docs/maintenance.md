# Maintenance helpers

Pinned-asset refresh scripts for `nix/pins.nix`, run from the dev shell
(`nix develop`).

Refresh pinned cang prebuilt release metadata in `nix/pins.nix` from a neutral
raw-ELF release. The updater rejects wrapper-script assets, legacy
flake-locked names, and payloads containing concrete
`/nix/store/<hash>-...` references.

`sha-*` releases are rolling dev artifacts: the release workflow keeps the 20
newest and never deletes versioned releases, so re-run this updater before a
pinned `sha-*` release ages out. A permanent `v<version>` release is never
pruned and names its asset after the tag, so pin a released artifact with
`--tag`; the updater derives the `cang-<tag>-<arch>-unknown-linux-gnu` name
from the tag:

```bash
nix develop --command ./scripts/update-cang-prebuilt.sh
nix develop --command ./scripts/update-cang-prebuilt.sh --tag v0.9.0
```

This updater only re-pins an artifact that is *already published* (it downloads
from GitHub Releases). Cutting a new cang release records the pin before the asset
exists, so follow the [cang release scheme](#cang-release-scheme) below instead.

Refresh pinned RTK prebuilt release metadata in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-rtk-prebuilt.sh
```

Refresh pinned Helvesec/rmux prebuilt release metadata in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-rmux-prebuilt.sh
```

Refresh pinned `dolthub/dolt` prebuilt release metadata (tag and per-system
asset hashes) in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-dolt-prebuilt.sh
```

Refresh pinned `gastownhall/beads` prebuilt release metadata (tag and
per-system asset hashes; release asset names embed the tag without its leading
`v`) in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-beads-prebuilt.sh
```

Refresh pinned `@pydantic/monty-linux-x64-gnu` worker metadata (version, tarball
asset name, and SRI hash) in `nix/pins.nix` from the npm registry:

```bash
nix develop --command ./scripts/update-monty-prebuilt.sh
```

libkrun itself is no longer pinned as an asset: cang links libkrun's Rust API
compiled from source (see the next section), so `nix/pkgs/libkrun.nix`,
`scripts/update-libkrun.sh`, the `libkrunRelease` pin and the
`libkrun-loadable` check were removed. The fork's C-ABI release assets keep
being published for third parties; cang does not consume them.

### Updating the libkrun fork

Two references name a libkrun revision, and they have to move together:

- the `deps/libkrun` **submodule pointer**, which is what in-tree `cargo` builds
  (the devshell and CI's `cargo` steps) compile, and
- the `libkrun-src` **flake input**, which is what every Nix build compiles:
  `nix/pkgs/workspace-src.nix` grafts it into `deps/libkrun` of the workspace
  source, because a flake's own source cannot carry submodule contents.

A libkrun update is therefore:

1. Move the pointer (`git -C deps/libkrun fetch` + `checkout`, or a fork branch
   update), then `git add deps/libkrun` here - flake builds only see tracked
   files.
2. Point the input at the same revision: `nix flake update libkrun-src`, then
   compare `flake.lock`'s `libkrun-src` rev with
   `git submodule status deps/libkrun`. A mismatch means Nix builds and in-tree
   `cargo` builds compile different libkrun revisions.
3. Refresh `Cargo.lock` for the new graph (`nix develop --command cargo build`)
   and commit it.
4. Refresh both vendor hashes, taking the value from the build's hash-mismatch
   error: `cargoDeps` in `nix/pkgs/cang-rust.nix` (cang's whole graph) and
   `krunInitCargoDeps` in `nix/pkgs/libkrun-source.nix` (the musl guest-init
   blob, which is not a workspace member and vendors from its own
   `init/init-binary/Cargo.lock`). `nix build .#cang` reports whichever
   mismatches first.
5. Re-verify with a live boot and the Chromium GPU smoke, not just a green build.

### Updating the rutabaga_gfx fork

libkrun's GPU device is what asks rutabaga for the virglrenderer flags, so cang's
`VIRGL_RENDERER_USE_VIDEO` request (`--gpu=drm`'s VA-API video) only reaches
virglrenderer if rutabaga exposes the bit. Upstream `magma-gpu/rutabaga_gfx`
stops at `VIRGLRENDERER_DRM`, so the dependency is the `zeroqn/rutabaga_gfx` fork
branch `cang`: upstream `main` plus `VIRGL_RENDERER_USE_VIDEO`,
`VirglRendererFlags::use_video` and `RutabagaBuilder::set_use_video`.

cang compiles it the way it compiles libkrun - from a checkout in `deps/`, not
from the revision libkrun's own manifest names:

- `deps/rutabaga_gfx` is a **submodule** of the fork (branch `cang`). It is what
  an in-tree `cargo` build compiles, through the
  `[patch."https://github.com/zeroqn/rutabaga_gfx"]` block in the workspace
  manifest, and what cang's `Cargo.lock` records (the patched packages carry no
  source).
- `rutabaga-gfx-src` is the **flake input** a Nix build compiles: the revision
  arrives in `flake.lock`, and `nix/pkgs/workspace-src.nix` grafts it into
  `deps/rutabaga_gfx` for the same reason libkrun is an input (a flake's own
  source cannot carry submodule contents).

Three references name a rutabaga revision and move together: the
`deps/rutabaga_gfx` **submodule pointer**, the `rutabaga-gfx-src` **input**, and
the rev in `deps/libkrun/src/devices/Cargo.toml` (twice: `[dependencies]` and
`[target.'cfg(target_os = "linux")'.dependencies]`). That last one is libkrun's
own default, used by the fork's own builds and rolling releases, so it must stay
at a commit that has the flag even though cang's build patches over it.

A rutabaga update is therefore: rebase and push the fork's `cang` branch, fetch
the submodule and check out the same commit (`git -C deps/rutabaga_gfx fetch &&
git -C deps/rutabaga_gfx checkout <sha>`), `git add deps/rutabaga_gfx`, move
libkrun's rev to that commit as a libkrun fork commit and follow "Updating the
libkrun fork" above, then `nix flake update rutabaga-gfx-src`, refresh
`Cargo.lock` and the `cargoDeps` vendor hash. `krunInitCargoDeps` does not move:
the musl init blob's lock has no rutabaga. A *local* rutabaga patch needs no fork
commit and no hash refresh - the in-tree `cargo` build already reads the
submodule, and `--override-input rutabaga-gfx-src
"git+file://$PWD/deps/rutabaga_gfx"` covers a Nix build.

### Re-basing the libkrunfw guest kernel

`deps/libkrunfw` bundles a kernel whose base is two variables in the `Makefile`
(`KERNEL_VERSION` + `KERNEL_REMOTE`, and `KERNEL_HARDENED_VERSION`), mirrored by
`kernelVersion`/`kernelHardenedVersion` and two hashes in
`nix/pkgs/libkrunfw.nix`. Moving the base is a patch-series re-base, not a
version bump:

1. Apply the current series to a pristine checkout of the **old** kernel as a
   commit series (`git am patches/0*.patch`), import the **new** kernel's
   pristine tree as an unrelated root commit, and replay with
   `git rebase --onto <new> <old> series`. Git then resolves the mechanical drift
   itself and stops only on real conflicts; commits whose content is already
   upstream come out empty - in the 6.12.109 → 7.2.7 re-base that removed nine
   patches wholesale (vsock dgram, virtio-CAN, virtio_rtc, virtgpu partial map,
   scanout import).
2. Port what conflicts, then regenerate the patch files with
   `git format-patch <new-base>..series` and renumber them - the `Makefile`
   applies `patches/0*.patch` in sorted order and then the hardened patch, and
   each of them has to apply on a pristine tree.
3. Refresh the x86_64 configs from the `.config` `olddefconfig` produces during
   a real build, and check the options cang needs survived (`CONFIG_UDMABUF`,
   `CONFIG_SECURITY_LANDLOCK`, `CONFIG_DRM_VIRTIO_GPU`, `CONFIG_FUSE_DAX`,
   `CONFIG_NFT_TPROXY`, `CONFIG_ZRAM`, `CONFIG_KVM`, `CONFIG_NR_CPUS`). There are
   four of them and they differ only in a handful of lines: `-kvm` is what cang's
   own `libkrunfw` build uses (lz4 kernel + deferred struct-page init), the
   release's plain `x86_64` swaps those two back to the upstream defaults (gzip,
   no deferred init), and `-lto`/`-kvm-lto` add `CONFIG_LTO_CLANG_THIN` for
   `MakefileLto`, which carries its own copy of `KERNEL_VERSION`, `FULL_VERSION`
   and `TIMESTAMP` - move those with the base, or the LTO assets keep being built
   from the old kernel under the new release tag.
   A local LTO build needs an *unwrapped* clang: nix's wrapped one feeds its own
   `--target`/`-nostdlibinc` into the kernel's flags and turns them into errors,
   and the final `-shared` link cannot find `crtn.o`. Build it as
   `make -f MakefileLto package HOSTCC=gcc HOSTCXX=g++ 'CLANG=clang -nostartfiles'`
   with `nixpkgs#llvmPackages.clang-unwrapped` and
   `nixpkgs#llvmPackages.bintools-unwrapped` on `PATH`; CI's stock clang needs
   none of that.
4. Build the kernel (`make` in the fork checkout, or
   `nix build .#cang-dev --override-input libkrunfw-src path:$PWD/deps/libkrunfw`)
   and **boot a real guest** with the built firmware
   (`CANG_LIBKRUNFW_LIBRARY=<checkout>/libkrunfw.so.5` plus the harness in
   `docs/wayfinder/libkrunfw-kernel-rebase/notes/`). Re-base on that line's own
   branch, add an architecture to `release-arches` only once its config is
   refreshed and its build is verified, and cut and pin the fork release as in
   the scheme above.

Two traps worth knowing, both found the hard way in the 7.2.7 re-base:

- **Virtualization feature bits collide with upstream.** The fork's
  `VIRTIO_GPU_F_FENCE_PASSING` and upstream's `VIRTIO_GPU_F_BLOB_ALIGNMENT` are
  both bit 5. Because the libkrun host offers bit 5 as `RESOURCE_SYNC` and has no
  blob-alignment config field, reading the bit as `BLOB_ALIGNMENT` left
  `vgdev->blob_alignment` at zero and the create-blob ioctl's
  `IS_ALIGNED(size, 0)` check rejected every blob - a dead GPU. Re-check every
  fork bit against the new kernel's `include/uapi/linux/virtio_*.h`.
- **Kernel-internal APIs drift under the patches.** In 7.x, `proto_ops.bind`/
  `.connect` take `struct sockaddr_unsized *` (`.getname` still takes
  `struct sockaddr *`), `__udp4/6_lib_lookup()` dropped the `udp_table`
  argument, and `v4l2_fh_add()`/`v4l2_fh_del()` take the `struct file *`. A
  three-way merge can also drop a closing brace or a `break` at a hunk boundary
  without complaining, so compile every touched file - the kernel build is the
  only check that matters.

`deps/libkrunfw` works the same way: its **`libkrunfw-src` input** (the kernel
source `.#cang-dev` builds) moves with the **submodule pointer**, while the
*released* kernel bundle cang opens at run time stays the separate
`libkrunfwRelease` pin refreshed below. Neither fork checkout needs a commit to
be built against:
`nix build .#cang --override-input libkrun-src "git+file://$PWD/deps/libkrun"`
(or `libkrunfw-src`) compiles the worktree.

Refresh pinned `zeroqn/libkrunfw` release metadata in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-libkrunfw.sh
```

### libkrun/libkrunfw fork release schemes

Both forks publish two kinds of prerelease on `zeroqn/libkrun` and
`zeroqn/libkrunfw`, and both attest their assets with
`actions/attest-build-provenance`, so
`gh attestation verify <asset> --repo zeroqn/libkrun` (or `libkrunfw`) works for
any of them:

- **Rolling `<line>-<sha>`**: built and published on every push to the fork's
  release branch - `cang` on `zeroqn/libkrun`, and `cang` or `cang-lts` on
  `zeroqn/libkrunfw`, the latter publishing `cang-lts-<sha>`. The publish
  workflow keeps only the newest ten per line, so these are disposable dev
  artifacts.
- **Permanent `v<libkrun|libkrunfw version>-cang.<n>`** (for example
  `v2.0.0-cang.3`), and `v<version>-cang-lts.<n>` for the libkrunfw LTS line:
  published by a manual `workflow_dispatch` run of the same workflow, or by
  pushing the tag directly (the workflow also triggers on tags). The prune job
  never touches these, so a pin into one never ages out.

A versioned release is created with:

```bash
gh workflow run publish-cang-release.yml --repo zeroqn/libkrun -f version=2.0.0-cang.3
gh workflow run publish-cang-release.yml --repo zeroqn/libkrunfw -f version=5.6.2-cang.1
```

The workflow rejects a version whose base does not match the branch's
`FULL_VERSION`, and refuses to republish an existing version at a different
commit (bump the number instead).

The libkrunfw workflow publishes the kernel bundle cang opens at run time
(`krunfw`). cang does not pin the libkrun workflow's C-ABI assets any more - see
`./deps/libkrun` under "Updating the libkrun fork" - but the fork
keeps publishing them for third-party C-ABI consumers, and the workflow still
builds with `FFI=1` for them: ABI 2 gates the whole C surface behind that cargo
feature, so without it the asset builds fine and ships a `libkrun.so` that
exports **nothing**.

Any libkrun change cang picks up is re-verified through the GPU smoke, not just a
green build: `.2` exported the ABI but regressed venus to the 2D fallback, which
only the Chromium smoke catches (`tools/chromium-cang-smoke`, GPU and
`--waypipe` modes). `.3` carries that fix.

#### libkrunfw's two kernel lines

`zeroqn/libkrunfw` keeps one kernel line per branch, so a kernel re-base never
has to drag a second line along:

- **`cang`** is the newest kernel line (currently linux-7.2.7 + linux-hardened
  v7.2.7-hardened1). cang's `libkrunfw-src` input and the primary
  `libkrunfwRelease` pin follow it.
- **`cang-lts`** is the LTS line (linux-6.12.109 + v6.12.109-hardened1). It also
  carries the fork's arm64 patches; a re-base of the newest line drops them.

`release-arches` in the fork tree names the architectures a line's release
carries, and the publish workflow builds and asserts exactly those: a line whose
kernel config has not been refreshed for its `KERNEL_VERSION` publishes no asset
for that architecture rather than a stale one. Today that is x86_64 for `cang`
(whose aarch64/riscv64 configs are still 6.12-era) and x86_64, aarch64, riscv64
for `cang-lts`.

Pin a permanent libkrunfw release, and use it for any tagged cang release:

```bash
nix develop --command ./scripts/update-libkrunfw.sh --system x86_64-linux --tag v5.6.2-cang.3
nix develop --command ./scripts/update-libkrunfw.sh --system aarch64-linux --tag v5.6.2-cang-lts.1
nix develop --command ./scripts/update-libkrunfw.sh --system riscv64-linux --tag v5.6.2-cang-lts.1
```

Each `update-libkrunfw.sh` run rewrites one system: the x86_64 run moves the
primary `tag` (the release `publish_release.yml`, `nix/pkgs/libkrunfw.nix` and the
primary pin all key off), while another system records a `tag` of its own only
when its asset is published on the other line - a system without one inherits the
primary. The `.github/workflows/publish_release.yml` job for a cang version tag
refuses to run while `libkrunfwRelease.tag` is still a rolling tag.

### cang release scheme

`zeroqn/cang` publishes itself from git tags: `publish_release.yml` runs on
`tags: "*"` (and on every `main` push, for the rolling alpha), so releasing needs
no `gh` authentication and no `workflow_dispatch`. A **tag push** publishes

- the permanent `v<version>` release, carrying the neutral asset
  `cang-v<version>-<arch>-unknown-linux-gnu` and its `.sha256`;
- the same asset on the immutable `sha-<short-sha>` prerelease of that revision;
- a build-provenance attestation
  (`gh attestation verify <asset> --repo zeroqn/cang`);
- and, from `publish_image.yml`, the image `ghcr.io/zeroqn/cang:<tag>`.

A **branch push** names the asset `cang-<arch>-unknown-linux-gnu` instead and only
touches the rolling releases, which keep the 20 newest `sha-*` prereleases; pin a
`v<version>` release.

`publish_release.yml` serializes both ref types in one
`rolling-alpha-release` concurrency group with `cancel-in-progress: true`, so the
*later* run cancels the earlier one - not necessarily the tag run. Pushing `main`
and the tag in one command is therefore a race, and v0.9.1 lost it: the branch
run was created second, the tag run was cancelled, and no `v0.9.1` release was
published at all. The tag run is the one that has to survive - it publishes the
versioned release *and* updates the `sha-<short-sha>` prerelease for its
revision - so push the branch first, wait for its run to finish, and only then
push the tag. If the tag run does get cancelled, re-create the tag at the same
commit (`git push --delete origin v<version>` and then `git push origin
v<version>`) to trigger it again.

Release a version in this order:

1. Bump `workspace.package.version` in `Cargo.toml` and the workspace-member
   entries in `Cargo.lock` (this is what `pins.cangVersion` reads) and commit it.
   The vendored lock carries those member versions, so `cargoDeps.hash` in
   `nix/pkgs/cang-rust.nix` has to move in the same commit: set it to
   `pkgs.lib.fakeHash`, run `nix build .#cang`, and copy the `got:` hash out of
   the mismatch. Nothing else about the graph changes, so that is the only hash
   a version bump touches.
2. Compute the asset SRI from a local build normalized exactly like the release
   workflow. The asset is patched to a neutral interpreter and embeds no
   `/nix/store` paths, so the result is byte-reproducible and is the hash CI
   publishes:

   ```bash
   nix build .#cang-ci-sccache -o result-cang-ci
   install -m 0755 result-cang-ci/bin/cang "/tmp/cang-v<version>-x86_64-unknown-linux-gnu"
   nix shell nixpkgs#patchelf -c patchelf \
     --set-interpreter /lib64/ld-linux-x86-64.so.2 --set-rpath "" \
     "/tmp/cang-v<version>-x86_64-unknown-linux-gnu"
   sha256sum "/tmp/cang-v<version>-x86_64-unknown-linux-gnu"
   ```

   Run this from `nix develop`, which provides `readelf` and `sha256sum`.

   The `.#cang-ci-sccache` attribute hard-codes `SCCACHE_DIR=/nix/var/cache/sccache`
   (the CI runner creates it), so a local build dies in that directory with
   `sccache: failed to create directory /nix/var/cache/sccache/preprocessor:
   Permission denied`. The workflow's own workaround,
   `--option extra-sandbox-paths "/nix/var/cache/sccache=<host dir>"`, is a
   restricted nix setting: an untrusted user only gets
   `ignoring the client-specified setting 'sandbox-paths'`, the sandbox keeps
   its read-only `/nix`, and the build fails the same way. Point `SCCACHE_DIR`
   at a sandbox-writable path for the local hash run instead - `overrideAttrs`
   needs no source edit:

   ```bash
   nix build --impure --expr 'let
     f = builtins.getFlake "git+file://'"$PWD"'";
     p = f.packages.x86_64-linux.cang-ci-sccache;
   in p.overrideAttrs (oa: { SCCACHE_DIR = "/build/cang-sccache"; })' -o result-cang-ci
   ```

   The hash does not depend on which cache directory is used: verified 2026-09
   by building the attribute twice with two different `SCCACHE_DIR` values and
   hashing both normalized assets, and again when cutting v0.9.1 by rebuilding
   v0.9.0's `038ef6f` this way and getting the published
   `19d4d67f...` back. It *does* depend on building this attribute: with
   `RUSTC_WRAPPER` set the binary comes out un-LTO'd (about 10 MB), while a plain
   `nix build .#cang` links with `lto = "thin"` (about 4.6 MB).
3. Commit the pin in `nix/pins.nix`: `cangPrebuiltRelease.tag = "v<version>"`, the
   versioned `asset` name from step 2, and that SRI.
4. Tag **the pin commit** — not the version bump — and push branch and tag, with
   the branch push's run finished before the tag goes out (v0.7.2 and v0.8.0 are
   both lightweight tags):

   ```bash
   git tag v<version>
   git push origin main
   # wait for the "Publish release binaries" run for main, then:
   git push origin "v<version>"
   ```

   One `git push origin main "v<version>"` is what v0.9.1 did, and it lost the
   race described above: the tag run was cancelled, so the tag had to be
   re-created before the versioned release appeared.

The tag has to point at the pin commit because `nix build .#cang-prebuilt` reads
the pin out of the tagged tree: tagging the bump commit leaves the pinned hash
behind the tag, so a `v<version>` checkout would still pin the previous release.
Do not tag first and pin afterwards, and do not move a tag once it is pushed.

The libkrunfw pin must already be a permanent `v<version>-cang.<n>` release. The
tag-triggered job refuses to publish while `libkrunfwRelease.tag` is a rolling
`cang-<sha>` tag, because those are pruned.

Verify the published release against the pin:

```bash
nix build .#cang-prebuilt
./result-cang-prebuilt/bin/cang --version   # prints `cang <version>`
```

#### Fork releases

`zeroqn/libkrunfw` and `zeroqn/libkrun` publish their own artifacts from pushed
permanent tags: push the fork's `cang` branch, then a `v<BASE>-cang.<n>` tag
(`<BASE>` is the fork `Makefile`'s `FULL_VERSION`, e.g. `5.6.2` / `2.0.0`). The
fork's `publish-cang-release.yml` validates the tag against that Makefile and
publishes the release plus build-provenance attestations, so no `gh` auth is
needed - the tag push is the whole trigger. Rolling `cang-<sha>` tags are
disposable and pruned; only `v*` tags are pinnable.

cang pins `libkrunfwRelease` only (it links libkrun's Rust API compiled from
source, so the `libkrun` and `libkrunfw` revisions live in the submodule
pointers plus the `libkrun-src`/`libkrunfw-src` inputs, not in a release pin).
After the fork release is published:

```bash
for system in x86_64-linux aarch64-linux riscv64-linux; do
  nix develop --command ./scripts/update-libkrunfw.sh --tag v<version>-cang.<n> --system "$system"
done
nix build .#libkrunfw
```

#### Released so far

- `v0.12.0` (`942c4bc`) - the `--gpu=drm` VA-API video path is complete on both
  halves. The host's `virglrenderer` now carries the client's packed parameter
  sets, the picture-order DPB references and reference lists a B-frame needs and
  the client's rate-control mode, and allocates the video surface linearly, so a
  guest's `h264_vaapi`/`hevc_vaapi` encode matches a host control's PSNR. Guest
  presentation is GPU-side: `GBM_BACKENDS_PATH` makes Waypipe carry dma-bufs,
  guest-init builds the NixOS-shaped `/run/opengl-driver` farm and exposes the
  glvnd EGL dispatcher so Chromium's GPU process boots on native EGL, and
  `mpv --hwdec=vaapi --vo=gpu --gpu-api=opengl` presents `vaapi` surfaces. A
  host whose own mesa VA driver needs the RBSP bound applies
  `cang.overlays.default` (`packages.mesa-rbsp-bounds`) or points
  `CANG_VA_DRIVER_PATH` at its own build, which the VM worker's `dlopen`
  interposer reads because secure-execution mode makes libva ignore
  `LIBVA_DRIVERS_PATH`. The render server's seccomp policy now allows
  `rename()`, which had been SIGSYS-killing it mid-present; x86_64 asset sha256
  `e5e0c90f5bd732ea4b6b78fa01c165940b7c6517cef15f8f6ef4d872ae5676f4`.
- `v0.11.2` (`b3007a1`) - the guest attach session no longer drops a child's
  final PTY output (it is forwarded before the `Exit` frame) and the
  guest-init suite is deterministic under load, so the nix builds whose
  checkPhase runs it stop failing at random; x86_64 asset sha256
  `290f103ce0999f0a0abf384a8dd0eb88a98f1ed10acffcdfbeba864c5174089d`.
- `v0.11.1` (`6cd1bd8`) - the default host sandbox boots again: Landlock
  relax mode accepts the managed guest kernel console log and the console
  device builders' duplicates of the worker's own stdio descriptors, and the
  packaged seccomp policy allows `sigaltstack`, which libkrun calls while it
  builds the VMM; x86_64 asset sha256
  `23b0953e9cbd197a4b872b8c9a955fa15f44c90e1de0de9a5d28fb3100dc9066`.
- `v0.11.0` (`4a6ad8b`) - the libkrun fork is re-based onto upstream main
  (`v2.0.0-cang.5`) and reads virtio-blk in parallel, libkrunfw moves to the
  7.2.7 kernel line (`v5.6.2-cang.3`, with aarch64/riscv64 on
  `v5.6.2-cang-lts.1`), and the agent layer ships the pinned `fresh` editor;
  x86_64 asset sha256
  `9c76d8f3e5d9a43db4fed4f21cb7144cf23407622793ba80cf365e98ff2632c6`.
- `v0.10.1` (`f0d5ecb`) - the libkrun and libkrunfw forks arrive as flake inputs
  (a Nix build no longer needs submodule contents and `nix/dev` is gone) and
  `ffier` leaves cang's lock; x86_64 asset sha256
  `c1384613589aec772efa9eee84fde4ca6e4f3769a442324c5c0af1ea6228ba32`.
- `v0.10.0` (`a9e9e66`) - the `CREATE_GUEST_HANDLE` zero-copy `wl_shm` fast path
  (`--zero-copy-shm`), on the libkrunfw `v5.6.2-cang.2` kernel (patches
  0037-0039 plus `CONFIG_UDMABUF=y`); x86_64 asset sha256
  `8ddc3b889224a90a63f799c2c1f7b007952861f03156aba4089c5c590f8a3c8e`.
- `v0.9.1` (`dd9d248`) - the virtio-balloon attach; x86_64 asset sha256
  `8488e43c320d76b48526d7e022774a065e8dc307001c5c0259b642ec50472edc`.
- `v0.9.0` (`4cd3dce`) - the Rust-API link that closed the prebuilt-libkrun map;
  x86_64 asset sha256
  `19d4d67f04d7c9f74f6b739f3e309eaf51011f4a5d5bdec766c410027e683ecd`.

Each of these is the hash its release publishes, and each was reproduced locally
from the tagged tree with the `overrideAttrs` recipe above (v0.9.0 while
validating that recipe; v0.9.1, v0.10.1, v0.11.0, v0.11.1, v0.11.2 and v0.12.0
before their pins went in - for v0.10.1, v0.11.0, v0.11.2 and v0.12.0 the
main-branch run of the same commit published the same bytes as the rolling
`sha-<shortsha>` artifact first, while for v0.11.1 that run's build step failed
and the tag run published the bytes the local recipe had already produced),
so a freshly published `.sha256` that disagrees with this list means the build
inputs moved: check `cargoDeps.hash` and the `cang-ci-sccache` attribute first.

Refresh pinned Pi coding agent source/npm metadata in `nix/pins.nix` from `earendil-works/pi`:

```bash
nix develop --command ./scripts/update-pi-coding-agent.sh
```

Refresh pinned `herdrdev/herdr` prebuilt release metadata (tag and per-system
asset hashes) in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-herdr.sh
```

Refresh pinned `sinelaw/fresh` prebuilt release metadata (tag and per-system
static-musl asset hashes) in `nix/pins.nix`. The updater refuses to pin a release
younger than two days - without `--tag` it selects the newest release that has
already aged past that, and with `--tag` it verifies the named release is old
enough too:

```bash
nix develop --command ./scripts/update-fresh-prebuilt.sh
```

Refresh pinned `zvec-ai/zvec-grep` source and npm dependency metadata in
`nix/pins.nix` (the updater also rejects a release whose `bin.zg` no longer
points at `dist/cli/index.js`, which `nix/pkgs/zvec-grep.nix` installs):

```bash
nix develop --command ./scripts/update-zvec-grep.sh
```

### wl-cross-domain-proxy subtree

`deps/wl-cross-domain-proxy` is a git subtree of
<https://codeberg.org/drakulix/wl-cross-domain-proxy>, not a submodule: the
sources are committed directly, so every flake source carries them. (The two
real submodules do not rely on that: their checkouts arrive as the
`libkrun-src` and `libkrunfw-src` inputs.) `nix/wl-cross-domain-proxy.nix`
builds that directory and the cang image installs the resulting guest proxy.

Pull upstream commits with:

```bash
git subtree pull --prefix=deps/wl-cross-domain-proxy \
  https://codeberg.org/drakulix/wl-cross-domain-proxy.git main -m "..."
```

`Cargo.lock` is committed inside the subtree, so a pull that changes
dependencies invalidates the `cargoHash` in `nix/wl-cross-domain-proxy.nix`.
Set it to `lib.fakeHash`, run `nix build .#wl-cross-domain-proxy`, and copy the
hash printed in the mismatch error.
