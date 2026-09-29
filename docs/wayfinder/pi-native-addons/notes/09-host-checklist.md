# Ticket 09 - the host checklist, run step by step

2026-09-29, pi session, on the dev host.

## Step 0 - the fix reaches the remote (bob)

Push confirmed: `origin/main` = `60cb5a61b868dbb4c9c4f87ddd46e07db1456801`, and
`1d8272d` (the wrapper fix) is an ancestor of it. Before the push the local branch
was nine commits ahead with none published.

## Step 1 - bump the flake input

`/home/dev/nix/disp` is a NixOS flake (`nixosConfigurations.disp`), not a git
repository, so the lock was backed up before touching it:

```
cd /home/dev/nix/disp
cp -a flake.lock flake.lock.bak-2026-09-29
nix flake update cang
```

Result: `cang` `9793dc786246deb1e8fff37d0e364e7de4199f86` ->
`60cb5a61b868dbb4c9c4f87ddd46e07db1456801`. The same command also re-resolved
cang's transitive inputs (`cang/nixpkgs` `6d663c05` -> `cf5e7650`,
`cang/nixpkgs-unstable` `44a91898` -> `7a0f122f`), which is what
`nix flake update <input>` does; the backup restores the previous lock if a
tighter bump is wanted.

## Step 2 - the pi the flake now resolves to

Building that package from the pushed revision gives **the same store path** the
local tree produces:

```
nix build "github:zeroqn/cang#pi-coding-agent" --no-link --print-out-paths
/nix/store/nwvcnfx3bma7h97gzhh79msr3g4jswww-pi-coding-agent-0.87.1
```

and that output is the wrapper:

```
bin/pi is a symlink?           no
wrapper names the runtime dir: 2 reference(s)
inner bun binary present:      yes
pi --version through wrapper:  0.87.1
```

The wrapper's own exported value, taken by rewriting its generated exec line
(`sed 's|^exec .*|echo ...|'`, `cmp`-guarded so the rewrite really happened):

```
LD_LIBRARY_PATH=/nix/store/0hpv152zh95hv36a8ksi0iywicgaphpn-cang-native-addon-runtime/lib
```

## Step 3 - the honest proof (masked `/etc`, no malloc provider help)

The host masks this failure today because NixOS's preloaded malloc provider
carries `libstdc++` itself, so the check is run under the guest's loader
condition - `bwrap --dev-bind / / --bind <empty> /etc`, which removes
`/etc/ld-nix.so.preload` - with `bun` (never `node`, which links `libstdc++`
itself and always passes):

| condition | sharp | onnxruntime-node |
|---|---|---|
| plain, no `LD_LIBRARY_PATH` | `LOAD_FAIL: Could not load the "sharp" module using the linux-x64 runtime` | `LOAD_FAIL: libstdc++.so.6: cannot open shared object file: No such file or directory` |
| the wrapper's exported `LD_LIBRARY_PATH` | **LOAD_OK** | **LOAD_OK** |

Raw evidence beside this file in `09-raw/` (`host-checklist-probe.txt` plus the
four probe outputs).

## What is still outstanding

The host has not been switched yet: `command -v pi` is still
`/etc/profiles/per-user/dev/bin/pi` ->
`/nix/store/4a5lfaqi12h8kiyj309ydmg7a3sb714j-pi-coding-agent-0.85.1/lib/pi-coding-agent/pi`
(pi 0.85.1, no wrapper). Applying the configuration is a privileged system
switch and is bob's:

```
sudo nixos-rebuild switch --flake /home/dev/nix/disp#disp
```

After that, `command -v pi` should resolve to the wrapped store path above and
the step-3 probe should pass against the activated binary without any
`LD_LIBRARY_PATH` being set by hand.
