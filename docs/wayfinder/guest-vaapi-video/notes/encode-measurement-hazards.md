# Encode measurement hazards (cang dev host, virtio-gpu)

These bit twice on 2026-10-03/04 and each time produced a wrong conclusion, so they are recorded
before anything else in this map is measured again.

## 1. `-bf 0` plus a decoded (mp4) input hangs every ffmpeg, host and guest

`/dev/dri/renderD128` on this machine is virtio-gpu (`vendor 0x1af4 device 0x1050`) - the "host" is
itself a VM whose VA encode runs through mesa's virgl VA driver. On that stack:

| arm (640x360 h264 mp4, `hwupload`, `h264_vaapi`) | result |
| --- | --- |
| no `-bf` | rc=0, 13 430 B |
| `-qp 26` | rc=0, 7 542 B |
| `-bf 1` | rc=0, 13 430 B |
| **`-bf 0`** | **hangs**: 0 bytes, `frame= 0` forever, SIGTERM ignored, only SIGKILL ends it (blocked in the driver; one measurement showed a busy CPU spin, a later one 0.001 s user time - do not rely on the spin as a symptom) |
| `-bf 0` + lavfi `testsrc2` input | rc=0 |
| `-bf 0` + raw nv12 input | rc=0 |

The hang needs `-bf 0` *and* a software-decoded source, and it reproduces with **plain host ffmpeg, no
cang involved**. Any probe that passes `-bf 0` and a decoded clip therefore measures this host bug,
not its subject. Use `-bf 1` (or omit `-bf`).

## 2. A hung arm leaves a spinning VM behind, and SIGTERM does not stop it

`timeout N` sends SIGTERM only. A stalled guest ffmpeg ignores it, so `timeout 420 script -q -e -c
"cang ..."` can sit for half an hour with a VM worker at 100% CPU and no `vm-exit=` line - and the
sub-agent running it blocks silently. Always `timeout -k 10 N`, and after every arm kill the leftover
set before the next one:

```sh
pkill -9 -x ffmpeg; pkill -9 -x cang; pkill -9 -x pasta; pkill -9 -x passt
```

(in a script file, not inline: `pkill -f 'cang internal libkrun'` run from a shell whose own command
line contains that literal kills the shell itself.)

## 3. Consequence for the earlier rounds

Every guest arm in ticket 04's "second measurement round" and ticket 05's real-content probes ran with
`-bf 0`. Any arm that reported *completing* did so while the host still tolerated `-bf 0`; any arm that
reported *hanging at `frame= 0`* on a decoded clip is confounded with hazard 1. The testsrc arms are
not affected, because `-bf 0` completes on testsrc.

## 4. The image can carry no patched VA driver at all (2026-10-04)

The loaded `localhost/cang:latest` had **no `/usr/lib/cang-va-runtime` entry in any of its nine
layers** (child-4 extracted all of them from the image tar to check; the last layer's `/usr/lib` held
only `cang-fontconfig`, `cang-mesa-runtime -> ...-mesa-26.1.8`, `cang-software-renderer -> ...-mesa-26.1.8`).
Guest-init's `MESA_ENV` puts `/usr/lib/cang-va-runtime/dri` first in `LIBVA_DRIVERS_PATH`, so that entry
was simply dead and libva fell through to `/usr/lib/cang-mesa-runtime/lib/dri` - the pinned prebuilt
mesa, which carries **none** of cang's guest-side encode patch. The visible symptom was an
`h264_vaapi` encode that stalls at `frame= 0`, writes 0 bytes, and never even prints the
`supported references:` line - identical for every cang build (with and without overlay patches),
which is what made it look like a cang regression. libva says nothing; `patched` and `prebuilt` arms
of a driver A/B are then the *same* driver and the comparison is void.

Diagnose from one guest arm (no VM needed if you have the tar):

```sh
ls -d /usr/lib/cang-va-runtime/dri        # in the guest: empty means the layer is missing
tar -tzf <image.tar.gz> | grep cang-va-runtime   # or check the archive's layers
```

The current tree links it unconditionally (`nix/image/container.nix` `ln -s ${layers.vaApiRuntime}
./usr/lib/cang-va-runtime`, built from `pkgs.mesaVaApi` in `nix/image/layers.nix`), so an image that
lacks it was built from a tree that did not - check the tree, not just the patches list.

Independent hazard on top of that: an image built without a GC root (`nix build --no-link`, or a
build whose `-o` symlink was later removed) is not protected together with its layer store paths.
`podman load` keeps serving the loaded image from the podman graph while `nix-collect-garbage` is
free to delete the layers its symlinks point into, which produces the same silent downgrade. Root
every image you build and keep the symlink (`nix build .#container -o <path outside the store>`).

## 5. Loading the image: space arithmetic that actually works (2026-10-04)

`podman load` needs room in two places at once - the blob temp (`$TMPDIR`) and the unpacked layers
(the `graphroot`) - and on this host both live on the same btrfs loop file, which itself draws its
blocks from `/`. Two attempts died on ENOSPC (`no space left on device` while applying a layer, then
while storing a blob in `$TMPDIR`) before this combination worked:

- free the loop first: delete finished VM task dirs. They are owned by the VM's mapped uid, so a
  plain `rm -rf` fails with `Permission denied`; run it inside the user namespace:
  `CONTAINERS_STORAGE_CONF=<conf> podman unshare rm -rf <graphroot>/../state/cang/workspace/tasks/workspace-*`
  (that freed ~10 GB and is what unblocked the load);
- leave `TMPDIR` **inside the loop** (`$D/tmp`) with the loop at ~26 GB free, and `/` at ~18 GB:
  ~9 GB of blob temp plus ~20 GB of unpacked layers then fit;
- the failed attempts roll back cleanly, but `/` sits at 0 bytes while they run - keep an eye on it,
  and do not start a VM in the same minutes.

After the successful load the run to compare against is: `Loaded image: localhost/cang:latest`
(id `60c470a1a2e3`, 8.88 GB), guest `ls -l /usr/lib/cang-va-runtime/dri/virtio_gpu_drv_video.so`
resolving to `../lib/libgallium.so`.
