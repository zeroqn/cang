# Networking

## Network modes

The default network mode is libkrun virtio-net/passt: cang starts a `passt`
unix-socket backend inside the same namespace, sets guest env `CANG_USE_PASST=1`,
and adds the libkrun net device before the VMM is built. Passing `--tsi`
opts into libkrun's virtio-vsock/TSI proxy mode. In that mode cang does not add a
libkrun network device, but the libkrun VMM still starts from the pasta-backed
namespace so the guest's Podman-like host aliases can reach the host at
`169.254.1.2`. Both modes always materialize `/etc/hosts` with:

```text
169.254.1.2    host.containers.internal host.docker.internal
```

Use repeatable `-p, --publish SPEC` to expose guest services on host ports.
In the default passt mode, unprefixed publish specs default to TCP; `tcp:` and
`udp:` select passt `-t` and `-u` forwarding respectively, and passt owns deeper
grammar validation for ranges, bind-address suffixes, interfaces, and exclusions:

```bash
./result/bin/cang -p tcp:8080:80 -p udp:5353:5353 -- bash -lc 'echo ok'
```

Passing `--tsi` switches to TSI mode, where cang supports only simple TCP
`HOST_PORT:GUEST_PORT` mappings through a two-hop path: `pasta` listens in the
host-facing helper namespace and forwards `HOST_PORT` into the VM worker's
private network namespace, while libkrun `krun_set_port_map()` maps guest
listens on `GUEST_PORT` to that same target-namespace `HOST_PORT`:

```bash
./result/bin/cang --tsi -p 8080:80 -- bash -lc 'python3 -m http.server 80'
```

TSI publish specs intentionally reject UDP, host bind addresses, port ranges,
random host ports, `all`/`none`, and protocol selectors.
Cang still does not create a shared/global rootless network namespace.

## Helper namespace and network setup

On a successful btrfs-snapshot run, cang then resolves the image's executable
`cang-guest-init`, writes a private hex-encoded `launch.conf` under the task
state directory, and supervises a keep-id helper namespace around
`<cang-exe> internal libkrun-network-enter <launch.conf>`. Buildah remains the
OCI image/rootfs materialization and cleanup tool, but it is no longer the
UID/GID namespace adapter for the libkrun helper. The helper wrapper requires
util-linux `unshare`, `newuidmap`, `newgidmap`, and usable `/etc/subuid` plus
`/etc/subgid` entries for the invoking user. It maps the invoking host UID and
GID to the same IDs inside the helper namespace, maps the lower and upper ID
ranges through subordinate IDs, then runs the helper as namespace root with
retained capabilities so prepared-root bind mounts can be grafted without
turning host-user-owned sources such as `/workspace` into `root:root` in the
guest view. During host-side network setup, cang temporarily uses the keep-id
filesystem UID/GID for helper state writes, then restores namespace-root
filesystem identity in the VM worker before prepared-root grafting. Missing
mapping support is a hard launch error instead of a silent fallback to
root-owned bind mounts. This path does not rely on Podman, idmapped
mounts, host `chown`, `:U` ownership mutation, or relaxed guest-init ownership
repair. The internal helper is also a network manager: it creates one private
network namespace holder for the cang session, starts `pasta` with Podman-like
`--map-guest-addr 169.254.1.2` and `--dns-forward 169.254.1.1`, then forks the
VM worker into that namespace. In the default passt mode, the helper creates an
`AF_UNIX` socketpair and starts `passt` with `--fd <child-fd>` before the VM
worker enters the private network namespace; the worker inherits the other fd
and passes it to libkrun with `krun_add_net_unixstream()`. This follows crun's
passt wiring, keeps published ports bound in the helper's host-facing network
namespace, and avoids creating passt control sockets on host `/tmp`. Missing `pasta`, unsupported
unprivileged namespace setup, or early proxy exit is a hard launch error
instead of a silent broken-host-alias fallback. The Nix `cang`,
`cang-prebuilt`, and development
shell paths include `pkgs.passt` so both `pasta` and `passt` are on `PATH`;
non-Nix invocations must provide those tools themselves.

## Host-alias smoke test

On a host with libkrun and unprivileged namespace support, smoke-test the alias
contract by starting a host listener and connecting from both modes:

```bash
# terminal 1
python3 -m http.server 18080 --bind 0.0.0.0

# terminal 2
./result/bin/cang -- bash -lc 'getent hosts host.containers.internal && curl -fsS http://host.containers.internal:18080/'
./result/bin/cang --tsi -- bash -lc 'getent hosts host.docker.internal && curl -fsS http://host.docker.internal:18080/'
```

## Nested virtualization

Cang direct-libkrun mode requests nested virtualization before guest entry with
libkrun's `krun_check_nested_virt`/`krun_set_nested_virt` APIs, matching the
crun `krun.nested_virt=1` flow used by the OCI/libkrun path. This exposes
VMX/SVM to the guest when the host or outer VM already supports nested KVM; it
does not bind-mount host `/dev/kvm` and does not create `/dev/kvm` manually. The
node should appear from the guest KVM driver and devtmpfs, after which
`cang-guest-init` makes it world-accessible for the default non-root task user.

If `/dev/kvm` is still absent inside the cang guest, confirm the host has
`/dev/kvm`, then check the relevant host nested parameter: Intel hosts should
report `Y` or `1` from `/sys/module/kvm_intel/parameters/nested`, and AMD hosts
should report `Y` or `1` from `/sys/module/kvm_amd/parameters/nested`. Also
confirm the active libkrun firmware/kernel is KVM-capable (`CONFIG_KVM=y` plus
the relevant `CONFIG_KVM_INTEL=y` and/or `CONFIG_KVM_AMD=y`) and that devtmpfs is
enabled. Guest-side diagnostics usually start with
`dmesg | grep -Ei 'kvm|vmx|svm'`.
