---
label: wayfinder:research
title: Why do PR 889's rxq drain and its used-queue signal blank the guest's VA-API decode?
status: open
blocked_by: []
claimed_by: pi session (2026-10-14)
---

# 16 - why does PR 889 blank the guest's VA-API decode?

## Question

Upstream PR 889 returns with upstream main, so the fork cannot drop it for ever.
[Ticket 15](15-pr-889-breaks-vaapi-decode.md) established that carrying it blanks
the guest's hardware-decoded frames (luma 0, chroma 128 - the guest reads its own
cleared VA surfaces) and that the re-base itself does not. What is the mechanism,
and what has to change on cang's side so the PR can be carried with the VA-API
decode green?

## What is established

- **Both halves of the PR do it on their own**, on top of the branch tip:
  `drain_rxq()` with the signal removed, and `signal_used_queue()` each iteration
  with no drain. It is not one specific interaction between the two.
- **The result is binary and repeatable per build** (six FAIL runs, four PASS runs,
  identical md5s): `f22b572f…` fails, `3006d5a7…` passes for the same clip.
- **CPU load is not the lever**: three guest CPU hogs leave PR 889 failing, 28 host
  CPU hogs leave the pre-rebase tip passing.
- **The guest does the same work in both states**: identical software decode, 60
  decoded frames, no ffmpeg/libva error, no guest kernel message, identical
  negotiated virtio-gpu features, and debug logs of the same size whose only
  difference is where the `Map surface` lines sit relative to the slice decodes.
- **Timesync is not the link**: the guest's clock tracks the host in both states.

## Leading hypothesis

The decoded picture is read back before the host has put it there: the guest's
read-back (`Map surface`) is ordered differently against the decode, and PR 889's
extra vsock work moves the guest's view of the decode's completion deterministically.
On the host side the picture is written by the freshly landed
`virglrenderer-decode-surface-readback.patch` /
`virglrenderer-decode-surface-sync.patch` pair (guest-vaapi-video tickets 09/10), so
a decode the guest considers complete before that read-back has run returns the
guest's own cleared surface - exactly the failing picture.

For: the interleaving is the only guest-side difference, and the blank frame's exact
contents (luma cleared to 0, chroma to the neutral 128, the first surface untouched)
are the driver's cleared surfaces rather than a corrupt decode.
Against: a 28x host slowdown does not break the passing arm, so a plain CPU race is
not sufficient on its own.

## Next step

Instrument the host the way tickets 09/10 did: markers in the vrend read-back
(`virgl_video.c` / `vrend_video.c`), and if the ordering is not visible there,
interpose libva on the host to log every `vaSyncSurface` / `vaDeriveImage` /
`vaMapBuffer` against the decode's completion, then diff a PASS and a FAIL arm. The
fork's `virtio-gpu` fence path is the second place to look: the guest's fence must
not be retired before the read-back has written the guest's planes.

## Deliverable

A fix - in the host read-back, the fence path, or the guest driver - that keeps the
VA-API decode green with PR 889 carried, so the next re-base onto upstream main does
not have to drop it; or a demonstration that the PR must be paired with a specific
upstream change before cang can carry it.

## Second measurement, 2026-10-14 (after v0.13.0)

Both arms were rebuilt from the pinned fork (`421247f6` = v2.0.0-cang.7, PASS and
`f8c18a1c` = that tree plus PR 889, FAIL) and the guest was left to decode the same
staged H.264 clip with the same guest-init, image, kernel, renderer and store. The
failure reproduces byte-for-byte (`y:5.372522 u:9.994820 v:9.662058` against
`y:46.633758 u:44.227307 v:44.408519`), and the guest's own view localises it:

| arm | hardware decode | software decode, same VM |
| --- | --- | --- |
| `421247f6` | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` |
| `f8c18a1c` (+ PR 889) | md5 `f22b572fb3967b0e4377a04088a546c0`, first 8 Y bytes all zero | md5 `3006d5a7881626c4c1ca1ff2b7b4eb91` |

So the clip, the guest userland, the file I/O and the software decode are
identical in both arms, and only the hardware path returns a cleared picture -
with the useful side effect that a working arm's `hw` decode is bit-identical to
its `sw` decode, which is worth keeping as the guard.

Tracing, so far:

- the pinned prebuilt `libvirglrenderer` *does* carry `VIRGL_LOG_LEVEL`,
  `VIRGL_LOG_FILE` and `VREND_DEBUG` (an earlier "no knobs" reading was a missing
  `strings` binary), and the render server's `/proc/<pid>/environ` shows those
  variables arriving - but no log file appears even with `--seccomp=off
  --landlock=off`, so that build's file logger needs another trigger;
- the guest side's `VIRGL_DEBUG`/`MESA_DEBUG` (both present in the image's
  `libgallium`) produce logs of identical shape in both arms (705 lines, the same
  counts), so the guest driver's trace does not separate them.

The DRM-ioctl route is blocked from an unprivileged shell here. Tracing the whole
run (`strace -f ... -- cang ...`) breaks the rootless setup: `newuidmap` and
`newgidmap` refuse to run under ptrace (`Operation not permitted`), cang's rootfs
ownership fix then fails with `os error 22` and the VM never starts. Attaching to
the render helper *after* cang has set up needs `ptrace_scope` 0 (it is 1 here,
"only a parent may trace"), so `strace -p <helper pid>` is refused - silently,
if its stderr is redirected, which produced an empty log before the cause was
found.

Next: instrument a **local build** of `virglrenderer` (or of cang's video
read-back patch) with a log line on the decoded-surface read-back path and run
both arms against it, which needs neither ptrace nor the library's file logger.

## Testing a candidate fix outside the fork (2026-10-14, late)

The candidate fix is a host-side fence: the picture's inputs (the coded bitstream
and the parameter buffers) are written into shared memory by GL commands and the
VA submission is what reads them, exactly the gap
`virglrenderer-encode-upload-fence.patch` closes on the encode side and
`virgl_video_end_frame()` does not close on the decode side. PR 889's extra
used-queue signals are what let the guest get far enough ahead to lose that race
every frame.

To test it without touching the fork, a patched `virglrenderer` was built locally
(`nix build --impure` over `.#virglrenderer-release-build` with the one-line patch
appended to `patches`; lib sha256 `f22a25d7c6e285ee…` against the pin's
`3afe8165ff9ac76e…`) and the arm run against it. Three attempts did not test it,
each for an environment reason, and each was caught by checking *which* library
was actually mapped rather than by trusting the exit status:

- copying the cang prefix and repointing `libexec/cang-helpers/virgl_render_server`
  left the link resolving to the pin;
- exporting `CANG_VIRGL_RENDER_SERVER` does not help: the smoke resolves the
  helper from its `--cang` root (`helper="$cang_root/libexec/cang-helpers/…"`) and
  asserts it, so the harness, not the environment, chooses the renderer;
- the last attempt could not even rewrite the link (`chmod` returned EROFS on the
  copied tree).

So the reliable route is one of: run the guest probe directly with
`CANG_VIRGL_RENDER_SERVER` pointing at the patched helper (the probe generates its
own clip with the guest's ffmpeg), or copy the cang prefix into a scratch tree
with `--no-preserve=mode` and repoint the link there. Until one of those proves
the fence, the interim policy stands: the fork does not carry PR 889, and when
upstream ships it the fork takes a `Revert "vsock: drain rxq in muxer thread"`
commit rather than letting the regression in.

## Mechanism found and fixed (2026-10-14, late)

**The decode's inputs are GL-written and VA-read with nothing ordering the two.**
The picture's coded bitstream and parameter buffers are written into this process'
shared memory by GL commands, and `virgl_video_end_frame()` -> `vaEndPicture()` is
what reads them. The encode direction already closes exactly this gap
(`virglrenderer-encode-upload-fence.patch`); the decode side never got the same
fence, so a decoder that runs before those writes execute reads an input buffer
that is not there yet and the picture comes out neutral. PR 889 signals the used
queue on every muxer iteration, which lets the guest get far enough ahead to lose
that race on every frame - and either half of the PR alone does the same, which is
why the earlier splitting measurement could not separate them.

**Fix:** `nix/pkgs/patches/virglrenderer-decode-upload-fence.patch` - a
`glFinish()` immediately before the decode submission.

**Verification** (`tools/virglrenderer-cang-smoke` arm 2; same guest image,
guest-init, kernel, renderer revision and hermetic store in every arm; the render
server taken from a locally built patched virglrenderer through a cang built with a
`postFixup` that repoints the packaged helper, so the mapped library was the
patched one):

  f8c18a1c (upstream main + fork + PR 889), stock renderer       FAIL  y 5.372522  u 9.994820  v 9.662058
  f8c18a1c, renderer built with the fence                        PASS  y 46.633758 u 44.227307 v 44.408519
  421247f6 (v2.0.0-cang.7, no PR 889), renderer with the fence   PASS  the same pre-rebase numbers

All twelve arm-2 checks pass in the patched builds; the only FAIL was
`virglrenderer-helper`, the attribution check that *expects* `.#virglrenderer` and
found the local experiment build - an artefact of the wiring, not of the fix.

**Shipping, and the trap that had to be fixed first:** the fix reaches users
through the prebuilt pin, and that pin's asset name and version carried only the
nixpkgs version and the pinned upstream revision - not cang's patch set. A patch
change therefore looked like "nothing changed" to `build-virglrenderer.yml`: it
skipped the build and left the pin pointing at the earlier tarball, so the fence
would never have shipped. The pipeline now keys on a hash of the patch set too
(`a5be550`), which republished the asset and moved the pin by itself.

**Consequence for the fork:** PR 889 can be carried again - the re-base on it is
safe now - and cang is not exposed whenever upstream ships it.

**Landed and shipped.** `build-virglrenderer.yml` published
`virglrenderer-1.3.0-c8e3fc3b-p440d7bb5-x86_64-linux.tar.gz` and moved the pin in
its own commit (`9b343e6`). Running arm 2 against that *shipped* artefact - as
`.#virglrenderer`, with cang built from the tip so its packaged helper names it -
passes all thirteen checks, `virglrenderer-helper` included, with the pre-rebase
PSNR. The shipped `libvirglrenderer.so.1.11.0` hashes `82771340082907f3…` against
the previous pin's `3afe8165ff9ac76e…`, so the artefact that ships is the fenced
one.

## Correction (2026-10-14, late): the fence is not sufficient

Putting PR 889 back into the fork - the `cang` branch, `v2.0.0-cang.8` - and running
the arm again against the **shipped** fenced renderer says the fence is not the fix
it looked like. Same fresh container image, same 0.13.0 guest-init, same store:

  fork without PR 889 (421247f6) + shipped fenced renderer   PASS  y 46.633758
  fork with PR 889 (a01c0b79)   + shipped fenced renderer    FAIL  y 5.372522
  fork with PR 889, again (isolated run)                     FAIL  y 5.372522

The fence is provably in the renderer that ran: `virgl_video_end_frame` in the
shipped `libvirglrenderer.so.1.11.0` disassembles with an indirect GL call
immediately before `vaEndPicture`, the same fingerprint as the locally built
renderer that passed once - and the pre-fix pin's library has nothing there. The
control above also rules out the image, the guest-init and the cang build on their
own, since the same ones pass without the PR.

So the single earlier PASS (PR 889 + a locally built fenced renderer + the image
that happened to be in the store) was timing, not a fix: the fence narrows the
window without closing it. The decode is a race, and one run in either direction is
not evidence - which is exactly how that result was over-read.

**What stands:** the fence is defensible on its own terms (the encode direction has
had the same one since `virglrenderer-encode-upload-fence.patch`, and the decode
side genuinely had no ordering between its GL-written inputs and `vaEndPicture`),
and it ships in the renderer. **What does not stand:** any claim that it fixes PR
889. cang stays pinned to `v2.0.0-cang.7` (`421247f6`, no PR 889), which is the last
state the arm passes in, and `v2.0.0-cang.8` is left published as a superseded tag
like `v2.0.0-cang.6` before it.

**Next hypothesis.** The window that matters may not be the host's GL at all, but
the guest-write -> host-read direction across the virtio ring: the decoder's inputs
arrive as buffers the guest writes, and PR 889's extra used-queue signals change
*when the guest sees those writes retired*, not when the host's GL executes.
Instrumenting the guest's submission (a locally built `virglrenderer` with a log
line on the decode path, the route that produced the fence) is the way to see it;
more host-side fences are not.

## Two corrections and the next probe (2026-10-15)

**The guest-side confound is eliminated.** The two cang builds (with and without
PR 889) reference *different* `krun-init` derivations, which looked like a second
variable - but their binaries are byte-identical
(`8dcef2246ba66bc795cd0f27d3a4190e…`), the static build being reproducible. So the
two arms differ in exactly one place: the host-side muxer change.

**The earlier "split" measurement was mislabelled.** The variant recorded as
"signal only" still called `drain_rxq()`; only its `if drain_rxq()` conditional was
folded to an unconditional signal. What that pair actually shows is therefore
narrower than recorded: *drain without signal* fails, *drain + always signal*
fails. Nothing yet separates the drain from the signal - the useful split is a build
whose muxer no longer drains at all, with the original conditional signal intact.

**Next probe: measure the host, not the guest.** "The guest reads its own cleared
surfaces" was an inference from the frame contents, never a measurement. One log
line in the local `virglrenderer` build settles it: in `virgl_video_end_frame` /
`decode_completed`, print the surface id, the mapped plane pointer and the first
bytes of the mapped surface. If the host's surface is already zero when the copy
runs, the decode itself produced nothing in the host driver (a VA/driver problem on
this host), not a guest read-back problem.

## Third measurement (2026-10-15): the host's surface is empty, its input is not

The read-back path itself is now instrumented, inside the render server cang
really runs (the local source build of `virglrenderer`, reached by making
`nix/lib/virglrenderer-cang.nix` pick the source tree, with the probe patch added
to its patch list and a `postFixup`-free build - the shipped helper is a thin
loader, so repointing its symlink alone left the *prebuilt* library mapped).
Failing arm (fork with PR 889, decode `y:5.372522`), logged in
`sync_dmabuf_to_video_buffer` and in `h264_decode_bitstream`:

    PROBE input buffers=1 empty=0 [0] size=9871 first=00 00 01 65 88 84 03 ff
    PROBE map fourcc=0x3231564e ('NV12') planes=2 layout=2 sixteen=0
              w0=640 h0=368 w1=320 h1=184 pitch0=768 pitch1=768 guest_planes=3
    PROBE plane0 row0=00 00 00 00 00 00 00 00   row1=00...  q=00...
    PROBE plane1 row0=80 80 80 80 80 80 80 80   row1=80...  q=80...
    PROBE packed plane=0 row0=00...   plane=1/2 row0=80...

Two things fall out, and they move the mechanism off the input side:

1. **The input is intact.** The coded bitstream the host's decoder is handed is
   9871 bytes starting `00 00 01 65` - an H.264 IDR NAL start code plus NAL type
   5 - so the guest's write reached the host complete. `empty=0`.
2. **The host's decoded surface is the cleared picture.** The mapped surface is
   all-zero luma with `0x80` chroma, and the packing path copies exactly that, so
   the guest receives a faithful copy of a surface that already held nothing.
   vrend's decode produced no picture into the surface *with valid input*.

That rules out the guest-write -> host-read ordering hypothesis for the *input*
(the fence I added covered the GL path and changed nothing, consistent with this
reading) and leaves the *output* side: with PR 889 in play the host's decode ends
up with an empty surface even though it was given a correct bitstream - i.e. the
host's own decode-vs-surface-reuse/in-flight ordering, not the guest's writes.

Next probe: log `decode_completed()` and the VA statuses of `vaEndPicture()` and
`vaSyncSurface()` per frame, with the surface's first bytes *at that moment* - does
the host believe the decode finished, and is the surface empty even then (a
reuse/clear race), or does the status show a decode that never produced a picture
(a driver-side drop)?

### The probe perturbs the race it measures (important)

Running the *same instrumented renderer* in the **control** arm - the fork
*without* PR 889, which passes with the shipped prebuilt renderer - also fails,
with the same empty surface. The difference between the two renderers is the
probe: it does an `fopen`/`fprintf`/`fclose` on the read-back path for every
frame. That path is exactly where the race is, so a probe that adds work there
can lose the race in *both* arms, and cannot be used to compare them.

Consequences:

- The *observation* stands (in the failing arm the host's mapped surface is empty
  and the input bitstream is intact) - it was measured in a run that failed in
  the arm's own terms as well.
- The *comparison* between arms does not stand until the probe is weightless:
  record into memory and dump once at process exit (`atexit`), log only the first
  few frames, or gate on a flag set by the environment. Only then is a two-arm
  A/B meaningful.

## The local source build of virglrenderer is broken; the shipped prebuilt is not (2026-10-15)

The isolation run settles the instrument question, and the answer is neither the
probe nor PR 889:

    arm: fork WITHOUT PR 889 (421247f6) - the configuration that PASSES with the
         shipped renderer - with a locally built *source* virglrenderer and no
         probe patch at all (wired: probe=0):
    PASS  vaapi-steps
    FAIL  vaapi-psnr-decode

So a locally built `.#virglrenderer` (the flake's source path: `pkgs.virglrenderer`
plus cang's patch list) blanks the decode **on its own**, with the same patches as
the pin, while `.#virglrenderer` resolved through the pin (the CI-built prebuilt
tarball) passes in exactly the same arm. That invalidates every instrumented run in
this ticket as evidence about PR 889: those runs used the source path, so they were
measuring this build difference. The "mapped surface is empty / input is intact"
lines describe a run whose renderer was already broken - the input observation is
probably still true of a healthy run, but neither is usable as evidence until a
passing baseline exists.

What still stands is the anchor A/B, which used the shipped prebuilt in both arms:
fork tip PASS, +PR 889 FAIL, re-based tip FAIL, re-based minus PR 889 PASS.

Open question, and the next step: why do the source build and the prebuilt differ
at all when they carry the same patches - build environment (the prebuilt is built
by `build-virglrenderer.yml` on a CI runner), the toolchain, or something the
workflow sets. The controlled pair is one no-PR-889 arm with the pinned prebuilt
(expected PASS) against the same arm built from the local source (measured FAIL).
