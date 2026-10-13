#!/bin/sh
# Runs inside the cang microVM under --gpu=drm, from the host runner
# (tools/virglrenderer-cang-smoke/virglrenderer-smoke.sh).
#
# The VA-API clients are the image's own (`pkgs.ffmpeg-headless` and
# `pkgs.libva-utils` in the agent layer). They cannot come from the host: the
# guest's /nix/store is the image's closure (a few hundred paths), so a host
# store path does not exist in the guest and does not resolve there. Every step
# is bounded by `timeout`: a vrend video stall is a known failure mode, and it
# must be attributed to one step instead of hanging until the host-side VM
# timeout kills everything.
#
# The guest always exits 0. Each step's exit status goes to
# evidence/vaapi-rc.txt and the sizes to evidence/vaapi-sizes.txt; the host
# runner is what decides pass/fail, so a guest-side stall still leaves the
# evidence a human can read.
#
# Two probe hazards this script deliberately avoids:
#  * every ffmpeg gets -nostdin: under the managed PTY a guest ffmpeg stops on
#    SIGTTOU, which looks exactly like a hung hardware decode;
#  * no arm passes -bf 0: on a virtio-gpu host, -bf 0 with a *decoded* input
#    hangs the host driver (see notes/encode-measurement-hazards.md), and it is
#    not what this smoke is about.
set -u

E=/workspace/evidence
mkdir -p "$E"

# Resolved from PATH (the image's agent layer), not staged by the host runner.
FFMPEG="$(command -v ffmpeg || echo ffmpeg)"
VAINFO="$(command -v vainfo || echo vainfo)"
# "W H FRAMES": environment variables do not survive into the guest (cang passes
# PATH plus its own allowlist), so a staged file is the only channel.
PARAMS="$(cat /workspace/smoke/vaapi-params 2>/dev/null || echo '640 360 60')"
# shellcheck disable=SC2086
set -- $PARAMS
W="${1:-640}"
H="${2:-360}"
FRAMES="${3:-60}"
DEV=/dev/dri/renderD128
FRAME_BYTES=$((W * H * 3 / 2))
STEP_TIMEOUT=180
PSNR_TIMEOUT=120

: > "$E/vaapi-rc.txt"
: > "$E/vaapi-sizes.txt"

# rcs and sizes are recorded as each step finishes, so a hang mid-run still
# leaves the steps that completed.
step() { # step <name> <log> <command...>
  name="$1"
  log="$2"
  shift 2
  timeout "$STEP_TIMEOUT" "$@" > "$log" 2>&1
  rc=$?
  printf '%s=%s\n' "$name" "$rc" >> "$E/vaapi-rc.txt"
  return 0
}

size() { # size <name> <file>
  if [ -f "$2" ]; then
    printf '%s=%s\n' "$1" "$(wc -c < "$2" | tr -d ' ')" >> "$E/vaapi-sizes.txt"
  else
    printf '%s=missing\n' "$1" >> "$E/vaapi-sizes.txt"
  fi
}

# Unscored diagnostics: the first things to read when a VA-API check fails.
{
  echo "uname: $(uname -a)"
  echo "--- /dev/dri ---"
  ls -l /dev/dri 2>&1
  echo "--- va/gl/vk env ---"
  env | grep -iE 'LIBVA|LIBGL|MESA|VK_|GALLIUM|CANG_GPU' 2>&1
  echo "--- /usr/lib/cang-va-runtime (the guest half of cang's encode fix) ---"
  ls -l /usr/lib/cang-va-runtime 2>&1
  ls -l /usr/lib/cang-va-runtime/dri 2>&1
  echo "--- /usr/lib/cang-mesa-runtime ---"
  ls -l /usr/lib/cang-mesa-runtime/lib/dri 2>&1
  echo "--- clients (from the image) ---"
  echo "ffmpeg: $(command -v ffmpeg || echo missing) $("$FFMPEG" -hide_banner -version 2>&1 | head -1)"
  echo "vainfo: $(command -v vainfo || echo missing)"
  echo "mpv:    $(command -v mpv || echo missing)"
  "$FFMPEG" -hide_banner -hwaccels 2>&1 | head -10
} > "$E/vaapi-diag.txt" 2>&1

# The driver libva will actually load. The image links
# /usr/lib/cang-va-runtime at the layer that carries cang's guest-side encode
# patch; if that layer's store path was collected, the symlink dangles, libva
# falls through to the unpatched mesa runtime, and every hardware encode stalls
# at frame=0 - which looks exactly like a cang regression. Resolving it here
# makes the dangling case a scored failure instead.
{
  printf 'image-path=%s\n' "$(ls /usr/lib/cang-va-runtime/dri 2>/dev/null | tr '\n' ' ')"
  for candidate in /usr/lib/cang-va-runtime/dri/*_drv_video.so; do
    [ -e "$candidate" ] || continue
    resolved="$(readlink -f "$candidate" 2>/dev/null || true)"
    if [ -n "$resolved" ] && [ -f "$resolved" ]; then
      printf 'driver=%s\n' "$resolved"
    else
      printf 'driver=<unresolved:%s>\n' "$candidate"
    fi
  done
} > "$E/vaapi-driver.txt" 2>&1

# The VA profile/entrypoint set the guest's driver advertises. Decode needs VLD,
# encode needs EncSlice; both come from the host through cang's vrend.
timeout 60 "$VAINFO" --display drm --device "$DEV" > "$E/vaapi-vainfo.txt" 2>&1
printf 'vainfo=%s\n' "$?" >> "$E/vaapi-rc.txt"

# Source frames: lavfi testsrc2 as raw nv12, the input every other step shares.
step source "$E/log-source.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -f lavfi -i "testsrc2=size=${W}x${H}:rate=30" -frames:v "$FRAMES" \
  -pix_fmt nv12 -f rawvideo "$E/source.nv12"
size source "$E/source.nv12"

# The decode arm's input is software-encoded, so the VA-API decode path is
# measured on its own (a VA-encoded input would confound the two halves).
step reference "$E/log-reference.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$E/source.nv12" \
  -c:v libx264 -preset veryfast -qp 26 -bf 1 -g 30 -f h264 "$E/reference.h264"
size reference "$E/reference.h264"

# Decode, hardware, into VAAPI surfaces and straight to /dev/null: this is what
# proves the decoder did the work, counted in frames.
step decode_vaapi "$E/log-decode-vaapi.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel info \
  -hwaccel vaapi -hwaccel_device "$DEV" -hwaccel_output_format vaapi \
  -i "$E/reference.h264" -f null -

# Decode, hardware, downloaded to nv12 by the decoder itself (no
# `-hwaccel_output_format vaapi`, so ffmpeg copies the surface out) so the host
# can compare the frames byte-for-byte against the source.
#
# The explicit `-vf hwdownload,format=nv12` form is NOT used: in a cang guest
# the driver cannot derive an image from a decoded surface, so that filter fails
# with EINVAL and then segfaults (rc 139). The unfiltered variant below is the
# one that reads the same decoder's frames back, and the unscored variant at the
# end records the crashing form so the evidence shows it.
step decode_vaapi_content "$E/log-decode-vaapi-content.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -hwaccel vaapi -hwaccel_device "$DEV" \
  -i "$E/reference.h264" -vf format=nv12 -pix_fmt nv12 \
  -f rawvideo "$E/decoded-vaapi.nv12"
size decoded_vaapi "$E/decoded-vaapi.nv12"

# Decode, software: the control for the arm above - the reference clip is
# decodable at all, and what the hardware decoded can be compared to.
step decode_software "$E/log-decode-software.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -i "$E/reference.h264" -vf format=nv12 -f rawvideo "$E/decoded-software.nv12"
size decoded_software "$E/decoded-software.nv12"

# Encode, hardware: cang's vrend encode path, from raw frames the guest uploads.
step encode_vaapi "$E/log-encode-vaapi.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -vaapi_device "$DEV" \
  -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$E/source.nv12" \
  -vf 'format=nv12,hwupload' -c:v h264_vaapi -qp 26 -bf 1 -g 30 \
  -f h264 "$E/encoded-vaapi.h264"
size encoded_vaapi "$E/encoded-vaapi.h264"

# The stream the hardware encoded has to be a bitstream somebody else accepts:
# decode it in software.
step encoded_decode_software "$E/log-encoded-decode-software.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -i "$E/encoded-vaapi.h264" -vf format=nv12 -f rawvideo "$E/encoded-vaapi-decoded.nv12"
size encoded_decode_software "$E/encoded-vaapi-decoded.nv12"

# Round trip: the stream hardware encoded goes straight back through hardware
# decode, both counted in frames and downloaded for the comparison.
step roundtrip_frames "$E/log-roundtrip-frames.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel info \
  -hwaccel vaapi -hwaccel_device "$DEV" -hwaccel_output_format vaapi \
  -i "$E/encoded-vaapi.h264" -f null -

step roundtrip_vaapi "$E/log-roundtrip-vaapi.txt" \
  "$FFMPEG" -nostdin -hide_banner -loglevel error \
  -hwaccel vaapi -hwaccel_device "$DEV" \
  -i "$E/encoded-vaapi.h264" -vf format=nv12 -pix_fmt nv12 \
  -f rawvideo "$E/roundtrip.nv12"
size roundtrip_vaapi "$E/roundtrip.nv12"

# PSNR against the source, per plane: luma alone would pass a stream whose chroma
# is wrong, which is exactly how cang's chroma bug presented.
psnr() { # psnr <name> <decoded-file>
  name="$1"
  # The per-frame log matters as much as the average: a defect can be exact on
  # the first frames and decay later (references built from damaged planes), and
  # an average hides that shape.
  timeout "$PSNR_TIMEOUT" "$FFMPEG" -nostdin -hide_banner -loglevel info \
    -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$2" \
    -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$E/source.nv12" \
    -lavfi "psnr=stats_file=$E/vaapi-psnr-$name-perframe.log" -f null - \
    > "$E/vaapi-psnr-$name.txt" 2>&1
  printf 'psnr_%s=%s\n' "$name" "$?" >> "$E/vaapi-rc.txt"
}

psnr decode "$E/decoded-vaapi.nv12"
psnr encode "$E/encoded-vaapi-decoded.nv12"
psnr roundtrip "$E/roundtrip.nv12"

# The software decode of the same clip as the reference, not the source: it
# isolates the decoder from the encoder, so "exact" means exact.
psnr_pair() { # psnr_pair <name> <file-a> <file-b>
  name="$1"
  timeout "$PSNR_TIMEOUT" "$FFMPEG" -nostdin -hide_banner -loglevel info \
    -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$2" \
    -f rawvideo -pix_fmt nv12 -s "${W}x${H}" -r 30 -i "$3" \
    -lavfi "psnr=stats_file=$E/vaapi-psnr-$name-perframe.log" -f null - \
    > "$E/vaapi-psnr-$name.txt" 2>&1
  printf 'psnr_%s=%s\n' "$name" "$?" >> "$E/vaapi-rc.txt"
}

psnr_pair decode_hw_sw "$E/decoded-vaapi.nv12" "$E/decoded-software.nv12"

# Frames the two hwframe decodes reported, so the host scores a count rather
# than inferring one from the downloaded bytes.
{
  printf 'decode_vaapi=%s\n' "$(grep -o 'frame=[[:space:]]*[0-9]*' "$E/log-decode-vaapi.txt" | tail -1 | grep -o '[0-9]*')"
  printf 'roundtrip_frames=%s\n' "$(grep -o 'frame=[[:space:]]*[0-9]*' "$E/log-roundtrip-frames.txt" | tail -1 | grep -o '[0-9]*')"
} > "$E/vaapi-frames.txt"

# Unscored: the explicit `hwdownload` form, recorded because it is a known
# failure in this guest (EINVAL, then a segfault when a format filter follows).
# A change here - either way - is worth seeing in the evidence.
timeout "$STEP_TIMEOUT" "$FFMPEG" -nostdin -hide_banner -loglevel warning \
  -hwaccel vaapi -hwaccel_device "$DEV" -hwaccel_output_format vaapi \
  -i "$E/reference.h264" -vf 'hwdownload,format=nv12' -f rawvideo "$E/hwdownload.nv12" \
  > "$E/log-hwdownload.txt" 2>&1
printf 'hwdownload=%s (known failing: EINVAL/EINVAL->SIGSEGV, see README)\n' "$?" > "$E/vaapi-hwdownload.txt"

# The render node the guest used, for the console log's sake.
ls -l "$DEV" > "$E/vaapi-render-node.txt" 2>&1

exit 0
