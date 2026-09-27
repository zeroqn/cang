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
nix develop --command ./scripts/update-cang-prebuilt.sh --tag v0.7.1
```

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

Refresh pinned `zeroqn/libkrun` prebuilt release metadata in `nix/pins.nix`
from the newest matching rolling `cang-<sha>` tag that contains both required
Linux assets. Root `.#libkrun` and every shared consumer (`.#cang`, images, and
`.#cang-prebuilt`) use the same pinned prebuilt libkrun package. Local source
builds stay in the submodule-aware dev flake and use the checked-out
`deps/libkrun` submodule:

```bash
nix develop --command ./scripts/update-libkrun.sh
```

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
  `v2.0.0-cang.1`): published by a manual `workflow_dispatch` run of the same
  workflow, or by pushing the tag directly (the workflow also triggers on tags). The prune job never touches these, so a pin into one never ages out.

A versioned release is created with:

```bash
gh workflow run publish-cang-release.yml --repo zeroqn/libkrun -f version=2.0.0-cang.1
gh workflow run publish-cang-release.yml --repo zeroqn/libkrunfw -f version=5.6.2-cang.1
```

The workflow rejects a version whose base does not match the branch's
`FULL_VERSION`, and refuses to republish an existing version at a different
commit (bump the number instead).

Pin a permanent release, and use it for any tagged cang release:

```bash
nix develop --command ./scripts/update-libkrun.sh --tag v2.0.0-cang.1
nix develop --command ./scripts/update-libkrunfw.sh --system x86_64-linux --tag v5.6.2-cang.1
nix develop --command ./scripts/update-libkrunfw.sh --system aarch64-linux --tag v5.6.2-cang.1
nix develop --command ./scripts/update-libkrunfw.sh --system riscv64-linux --tag v5.6.2-cang.1
```

Each `update-libkrunfw.sh` run rewrites one system, so pass the same `--tag` for
all three. The `.github/workflows/publish_release.yml` job for a cang version tag
refuses to run while `libkrunRelease.tag` or `libkrunfwRelease.tag` is still a
rolling `cang-<sha>` tag.

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
