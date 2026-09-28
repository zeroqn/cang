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

libkrun itself is no longer pinned: cang links libkrun's Rust API out of the
`deps/libkrun` submodule (see the next section), so `nix/pkgs/libkrun.nix`,
`scripts/update-libkrun.sh`, the `libkrunRelease` pin and the
`libkrun-loadable` check were removed. The fork's C-ABI release assets keep
being published for third parties; cang does not consume them.

### Updating the `deps/libkrun` submodule

A libkrun update is a submodule pointer bump, and things have to move together:

1. Move the pointer (`git -C deps/libkrun fetch` + `checkout`, or a fork branch
   update), then `git add deps/libkrun` here - flake builds only see tracked
   files.
2. Refresh `Cargo.lock` for the new graph (`nix develop --command cargo build`)
   and commit it.
3. Refresh both vendor hashes, taking the value from the build's hash-mismatch
   error: `cargoDeps` in `nix/pkgs/cang-rust.nix` (cang's whole graph) and
   `libkrunCargoDeps` in `nix/pkgs/libkrun-source.nix` (the fork's own lock, used
   by the musl guest-init blob). `nix build .#cang` reports whichever mismatches
   first.
4. Re-verify with a live boot and the Chromium GPU smoke, not just a green build.

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

- **Rolling `cang-<sha>`**: built and published on every push to the fork's
  `cang` branch. The publish workflow keeps only the newest ten, so these are
  disposable dev artifacts.
- **Permanent `v<libkrun|libkrunfw version>-cang.<n>`** (for example
  `v2.0.0-cang.3`): published by a manual `workflow_dispatch` run of the same
  workflow, or by pushing the tag directly (the workflow also triggers on tags). The prune job never touches these, so a pin into one never ages out.

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
`./deps/libkrun` under "Updating the `deps/libkrun` submodule" - but the fork
keeps publishing them for third-party C-ABI consumers, and the workflow still
builds with `FFI=1` for them: ABI 2 gates the whole C surface behind that cargo
feature, so without it the asset builds fine and ships a `libkrun.so` that
exports **nothing**.

Any libkrun change cang picks up is re-verified through the GPU smoke, not just a
green build: `.2` exported the ABI but regressed venus to the 2D fallback, which
only the Chromium smoke catches (`tools/chromium-cang-smoke`, GPU and
`--waypipe` modes). `.3` carries that fix.

Pin a permanent libkrunfw release, and use it for any tagged cang release:

```bash
nix develop --command ./scripts/update-libkrunfw.sh --system x86_64-linux --tag v5.6.2-cang.1
nix develop --command ./scripts/update-libkrunfw.sh --system aarch64-linux --tag v5.6.2-cang.1
nix develop --command ./scripts/update-libkrunfw.sh --system riscv64-linux --tag v5.6.2-cang.1
```

Each `update-libkrunfw.sh` run rewrites one system, so pass the same `--tag` for
all three. The `.github/workflows/publish_release.yml` job for a cang version tag
refuses to run while `libkrunfwRelease.tag` is still a rolling `cang-<sha>` tag.

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
`v<version>` release. A tag push also cancels the branch-push run in the shared
`rolling-alpha-release` concurrency group, which is harmless: the tag run
publishes the versioned release *and* the `sha-<short-sha>` prerelease for its
revision.

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
   (the CI runner creates it), so a local build fails with
   `failed to create directory /nix/var/cache/sccache` unless that directory
   exists and is writable, or the value is overridden. The hash does not depend
   on which cache directory is used - verified 2026-09 by building the attribute
   twice with two different `SCCACHE_DIR` values and hashing both normalized
   assets - but it *does* depend on building this attribute: with `RUSTC_WRAPPER`
   set the binary comes out un-LTO'd (about 10 MB), while a plain
   `nix build .#cang` links with `lto = "thin"` (about 4.6 MB).
3. Commit the pin in `nix/pins.nix`: `cangPrebuiltRelease.tag = "v<version>"`, the
   versioned `asset` name from step 2, and that SRI.
4. Tag **the pin commit** — not the version bump — and push branch and tag (v0.7.2
   and v0.8.0 are both lightweight tags):

   ```bash
   git tag v<version>
   git push origin main "v<version>"
   ```

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

Refresh pinned Pi coding agent source/npm metadata in `nix/pins.nix` from `earendil-works/pi`:

```bash
nix develop --command ./scripts/update-pi-coding-agent.sh
```

Refresh pinned `herdrdev/herdr` prebuilt release metadata (tag and per-system
asset hashes) in `nix/pins.nix`:

```bash
nix develop --command ./scripts/update-herdr.sh
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
sources are committed directly so a flake build sees them without
`?submodules=1`. `nix/wl-cross-domain-proxy.nix` builds that directory and the
cang image installs the resulting guest proxy.

Pull upstream commits with:

```bash
git subtree pull --prefix=deps/wl-cross-domain-proxy \
  https://codeberg.org/drakulix/wl-cross-domain-proxy.git main -m "..."
```

`Cargo.lock` is committed inside the subtree, so a pull that changes
dependencies invalidates the `cargoHash` in `nix/wl-cross-domain-proxy.nix`.
Set it to `lib.fakeHash`, run `nix build .#wl-cross-domain-proxy`, and copy the
hash printed in the mismatch error.
