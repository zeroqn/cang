# Ticket 02 - loader-visible mechanisms, without an inherited environment variable

Resolved 2026-09-29 by the charting session. The research child that claimed
this ticket was killed mid-run by the host disk filling; the findings below are
direct measurements.

## What the loader in this environment actually reads

`strings` is not installed on this host, so the loader was scanned with
`grep -a`:

```
$ ld=/nix/store/znb3q6g1ik34454j3vcjx824h1871asg-glibc-2.42-84/lib/ld-linux-x86-64.so.2
$ grep -a -o -E '/etc/[a-z0-9._-]*(preload|cache)[a-z0-9._-]*' $ld | sort -u
/etc/ld-nix.so.preload
/etc/ld.so.cache
```

So nixpkgs' glibc knows **both** hooks:

- `/etc/ld-nix.so.preload` - the nix-specific preload list (this is the file
  `nix/image/container.nix:99` writes and guest-init rewrites per `--alloc`).
- `/etc/ld.so.cache` - the ordinary glibc cache path is compiled in, even though
  nothing in this environment ever generates one.

Search order observed on the host (`LD_DEBUG=libs`): the requesting object's
RUNPATH, `LD_LIBRARY_PATH`, and then the loader's **own store lib dir** as the
"system search path". There is no `/lib`, no `/usr/lib`, no `/etc/ld.so.cache`
and no `ldconfig`-managed tree anywhere in the host or the image.
`/lib64/ld-linux-x86-64.so.2 -> nix-ld` exists on the NixOS host only; it is not
part of the cang image, and it only helps foreign *executables*, not `dlopen`.

## Mechanisms, with a verdict each

| mechanism | reaches a dlopen'd addon? | in the guest? | verdict |
|---|---|---|---|
| `LD_LIBRARY_PATH` pointing at `gcc-*-lib/lib` | yes - measured LOAD_OK | image Env or a wrapper | works; env inherited by everything (see ticket 03) |
| `LD_PRELOAD` of a library that NEEDs libstdc++ | yes - measured LOAD_OK with cang's `hardened_malloc-14` | `/etc/ld-nix.so.preload` already exists but is the allocator's file | works, but couples the C++ runtime to the allocator: `--alloc=glibc` deletes the file and would break the addons again |
| `/etc/ld.so.preload` listing `libstdc++.so.6` | not measured; the hook is a real glibc feature and the loader path is compiled in | image `/etc` can carry it | plausible and env-free, untested here |
| `/etc/ld.so.cache` built over a shipped compat lib dir | not measured; the path is compiled into this loader | image `/etc` can carry it, and it would be built from store paths the image already contains | plausible and env-free, but nothing in the image generates one (`ldconfig` would have to run at image build) |
| RUNPATH rewriting of the addons | yes, but the addon tree is installed **at runtime** under `~/.pi` (`~/.pi` is grafted host -> guest) | a start-time step in a wrapper would have to mutate the user's tree | works, but mutates user state on every pi start |
| an FHS `/lib` or default-path compat dir | no - the only default path is the loader's own store `lib`, which is read-only store content | - | ruled out |

## What the image already contains

`gcc` is in the image (`nix/image/layers.nix` `cToolchainImagePackages`), so
`libstdc++.so.6` and `libgcc_s.so.1` are already present in the guest's store as
part of the flake's gcc - a compat directory or an `LD_LIBRARY_PATH` can point at
an existing store path; no new library content is needed.

## Shortlist for the mechanism ticket

1. `LD_LIBRARY_PATH` to `${pkgs.stdenv.cc.cc.lib}/lib` (or the image's gcc lib
   dir) via a `pi` wrapper in `nix/pkgs/pi-coding-agent.nix` - reaches host and
   guest, proven by measurement, narrowest thing that works.
2. The same path added to the image Env (`nix/image/config.nix`) - reaches every
   dynamic process in the guest, not the host.
3. An env-free `/etc` hook (`/etc/ld.so.preload`, or `/etc/ld.so.cache` over a
   compat dir) written at image build - no inherited variable, but guest-only and
   still unprobed.
4. Extending `/etc/ld-nix.so.preload`'s line set past the allocator - the
   allocator owns that file (`--alloc=glibc` removes it), so this is a
   correctness trap rather than a mechanism.
