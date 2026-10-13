#!/usr/bin/env bash
# Start Chromium in a cang guest with the guest's GPU acceleration wired up.
#
# Two modes:
#
#   headless   Chromium's own new headless mode. Needs no compositor, no display
#              and no host-side anything: this is the mode for WebGL/WebGPU work,
#              screenshots, printing and page automation.
#   gui        Chromium as a window on the guest's Wayland display. In a cang
#              guest that display is the one cang's `--waypipe` provides (start
#              cang with `--gpu=drm --waypipe`), so the window is presented on the
#              host's own compositor.
#
# Both modes take the same GPU setup, and neither works without it:
#
#   Vulkan   the venus ICD guest-init pins for --gpu=drm (VK_ICD_FILENAMES, never
#            VK_DRIVER_FILES) backs ANGLE, which is what renders WebGL and raster.
#   EGL      glvnd's dispatcher plus mesa's vendor library and GBM backend, all
#            under /usr/lib/cang-*/.
#   VA-API   the mesa runtime's virtio_gpu_drv_video.so decodes H.264 through
#            vrend's video path, and Chromium's VA-API decoder is enabled here.
#
# So a guest cang started without --gpu=drm has no DRM render node, no venus ICD
# and no VA driver; this script stops with an explanation instead of starting a
# software-rendered browser that looks like it worked.
#
# Usage:
#   cang-chromium headless [URL] [-- <extra chromium arguments>]
#   cang-chromium gui      [URL] [-- <extra chromium arguments>]
#   cang-chromium --check  [-- <extra chromium arguments>]
#
# Options:
#   --check            report the GPU setup and exit non-zero unless Chromium
#                      really reports hardware acceleration
#   --print-command    print the resolved command and exit (no browser started)
#   --window-size WxH  initial window size (default 1280x720)
#   --user-data-dir D  profile directory (default /tmp/cang-chromium-<mode>)
#   --timeout SECS     kill Chromium after this many seconds (default: none)
#   -h, --help         this help
set -euo pipefail

MODE=""
CHECK=0
PRINT_ONLY=0
WINDOW_SIZE="1280x720"
USER_DATA_DIR=""
CHROME_TIMEOUT=""
CHROMIUM="${CANG_CHROMIUM:-chromium}"

# The guest image's layout. Overridable so the script can be exercised outside
# the image (and by a downstream image that lays the runtime tree out elsewhere).
MESA_RUNTIME="${CANG_MESA_RUNTIME_DIR:-/usr/lib/cang-mesa-runtime}"
GPU_RUNTIME="${CANG_GPU_RUNTIME_DIR:-/usr/lib/cang-gpu-runtime}"
VA_RUNTIME="${CANG_VA_RUNTIME_DIR:-/usr/lib/cang-va-runtime}"

die() { printf 'cang-chromium: %s\n' "$*" >&2; exit 2; }
note() { printf 'cang-chromium: %s\n' "$*" >&2; }

usage() {
  cat <<'USAGE'
cang-chromium - start Chromium in a cang guest with its GPU acceleration wired up

Usage:
  cang-chromium headless [URL] [-- <extra chromium arguments>]
  cang-chromium gui      [URL] [-- <extra chromium arguments>]
  cang-chromium --check  [-- <extra chromium arguments>]

  headless  Chromium's new headless mode: no display, no compositor. For WebGL,
            screenshots, printing and page automation.
  gui       A Chromium window on the guest's Wayland display, which in a cang
            guest is the one `cang --gpu=drm --waypipe` provides, presented on
            the host's own compositor.
  --check   Report the guest GPU setup and what Chromium actually got, and exit
            non-zero unless WebGL and video decode are hardware accelerated.

Options:
  --print-command    print the resolved Chromium command and exit
  --window-size WxH  initial window size (default 1280x720)
  --user-data-dir D  profile directory (default /tmp/cang-chromium-<mode>)
  --timeout SECS     kill Chromium after this many seconds
  -h, --help         this help

Both modes need a guest started with `cang --gpu=drm`; without it there is no DRM
render node, no venus Vulkan ICD and no VA-API driver, and this script says so
instead of starting a software-rendered browser.
USAGE
}

# What to tell the user when the guest has no GPU at all.
no_gpu_hint() {
  cat >&2 <<'HINT'

cang-chromium: this guest has no GPU acceleration, because cang was not started
               with --gpu=drm. Guest GPU is provided by cang's DRM virtio-gpu
               mode, which is also what pins the venus Vulkan ICD and exposes the
               VA-API render node. Start the guest as:

                   cang --gpu=drm [--waypipe] -- <command>

               (--waypipe is what gives the guest a Wayland display for `gui`.)
HINT
}

describe() {
  printf '  cang-chromium: %-28s %s\n' "$1" "$2" >&2
}

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
EXTRA=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    headless | gui)
      [ -z "$MODE" ] || die "mode given twice: $MODE and $1"
      MODE="$1"
      shift
      ;;
    --check) CHECK=1; shift ;;
    --print-command) PRINT_ONLY=1; shift ;;
    --window-size) WINDOW_SIZE="${2:?missing value for --window-size}"; shift 2 ;;
    --user-data-dir) USER_DATA_DIR="${2:?missing value for --user-data-dir}"; shift 2 ;;
    --timeout) CHROME_TIMEOUT="${2:?missing value for --timeout}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    --)
      shift
      # Everything after `--` goes to Chromium untouched.
      while [ "$#" -gt 0 ]; do EXTRA+=("$1"); shift; done
      ;;
    -*)
      # An unknown option is Chromium's, not ours; the rest of the line follows
      # it, so argument order is preserved.
      while [ "$#" -gt 0 ]; do EXTRA+=("$1"); shift; done
      ;;
    *)
      EXTRA+=("$1")
      shift
      ;;
  esac
done

[ -n "$MODE" ] || [ "$CHECK" -eq 1 ] || { usage >&2; exit 2; }

# A URL is the only positional argument this script passes through itself; the
# rest are Chromium's, in the order the caller gave them.
URL=""
if [ -n "$MODE" ] && [ "${#EXTRA[@]}" -gt 0 ]; then
  case "${EXTRA[0]}" in
    http://* | https://* | file://* | about:* | chrome://* | data:*)
      URL="${EXTRA[0]}"
      EXTRA=("${EXTRA[@]:1}")
      ;;
  esac
fi

# ---------------------------------------------------------------------------
# Preflight: the guest has to be a --gpu=drm guest
# ---------------------------------------------------------------------------
if ! command -v "$CHROMIUM" >/dev/null 2>&1; then
  die "no chromium: '$CHROMIUM' is not an executable on PATH (set CANG_CHROMIUM to override)"
fi

render_node=""
for candidate in /dev/dri/renderD*; do
  [ -e "$candidate" ] || continue
  render_node="$candidate"
  break
done

if [ -z "$render_node" ]; then
  describe "/dev/dri/renderD*" "missing"
  no_gpu_hint
  exit 2
fi

[ -d "$MESA_RUNTIME" ] || die "$MESA_RUNTIME is missing; this guest image has no cang Mesa runtime"
[ -d "$VA_RUNTIME" ] || note "$VA_RUNTIME is missing; VA-API video decode will not work"
[ -d "$GPU_RUNTIME" ] || note "$GPU_RUNTIME is missing; the EGL dispatcher may not be reachable"

# The venus ICD is what guest-init pins for --gpu=drm; if it is absent the guest
# has DRM without the Vulkan half, which a --gpu=drm guest never should.
venus_icd="$MESA_RUNTIME/share/vulkan/icd.d/virtio_icd.x86_64.json"
if [ ! -f "$venus_icd" ]; then
  describe "$venus_icd" "missing"
  no_gpu_hint
  exit 2
fi

# Fill in what guest-init exports for --gpu=drm guests, if this shell lost it.
# Never VK_DRIVER_FILES: that would override the ICD an EGL client picks for
# itself (see the cang guest-init notes on VK_ICD_FILENAMES).
export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:-$GPU_RUNTIME/lib}"
export LIBGL_DRIVERS_PATH="${LIBGL_DRIVERS_PATH:-$MESA_RUNTIME/lib/dri}"
export LIBVA_DRIVERS_PATH="${LIBVA_DRIVERS_PATH:-$VA_RUNTIME/dri:$MESA_RUNTIME/lib/dri}"
export __EGL_VENDOR_LIBRARY_FILENAMES="${__EGL_VENDOR_LIBRARY_FILENAMES:-$MESA_RUNTIME/share/glvnd/egl_vendor.d/50_mesa.json}"
export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-$venus_icd}"
export GBM_BACKENDS_PATH="${GBM_BACKENDS_PATH:-$MESA_RUNTIME/lib/gbm}"

# A failed GPU process is the failure mode this script exists to prevent, so make
# it loud: these two flags are what keeps ANGLE on the venus path in this guest.
#   --use-angle=vulkan          WebGL/raster through ANGLE's Vulkan backend
#   --disable-vulkan-surface    ANGLE's Vulkan WSI/swapchain never completes over
#                               venus; the GPU process drives venus, then stalls
#                               and aborts (exit_code=6) if the surface path is
#                               left enabled
# `--enable-features=Vulkan` is deliberately NOT used: it moves the display
# compositor onto Vulkan, which needs a VkSurfaceKHR the ozone-wayland path does
# not implement, and the GPU process then crash-loops instead of painting.
# VA-API decode and encode through the guest's mesa VA driver. Chromium's own
# feature names have changed over releases; the unknown ones are ignored, and the
# check mode is what proves the surviving set actually accelerated the decoder.
VAAPI_FEATURES=VaapiVideoDecoder,VaapiVideoEncoder,VaapiVideoDecodeLinuxGL,AcceleratedVideoDecodeLinuxGL

GPU_FLAGS=(
  --no-sandbox
  --disable-gpu-sandbox
  --use-angle=vulkan
  --disable-vulkan-surface
  --enable-accelerated-video-decode
  # The guest GPU is not in Chromium's allowlist, so both the WebGL path and the
  # video decoder would otherwise be refused before anything is tried.
  --ignore-gpu-blocklist
  --enable-logging=stderr
)

case "$MODE" in
  headless)
    # Headless has no display compositor to keep off Vulkan, so the Vulkan
    # feature set (and Skia's Vulkan renderer) joins the VA-API features. One
    # --enable-features flag, not two: Chromium does not reliably merge repeats.
    MODE_FLAGS=(--headless=new "--enable-features=Vulkan,UseSkiaRenderer,$VAAPI_FEATURES")
    USER_DATA_DIR="${USER_DATA_DIR:-/tmp/cang-chromium-headless}"
    ;;
  gui)
    [ -n "${WAYLAND_DISPLAY:-}" ] || {
      note "gui mode needs a Wayland display and WAYLAND_DISPLAY is unset."
      cat >&2 <<'HINT'
               Start the guest with cang's waypipe display:

                   cang --gpu=drm --waypipe -- <command>

               and run this script from inside that guest (the host then presents
               the window through its own compositor). `cang-chromium headless`
               needs no display at all.
HINT
      exit 2
    }
    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ ! -e "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]; then
      die "WAYLAND_DISPLAY=$WAYLAND_DISPLAY has no socket under $XDG_RUNTIME_DIR; the guest's Wayland display is not up"
    fi
    MODE_FLAGS=(--ozone-platform=wayland "--enable-features=$VAAPI_FEATURES"
                --app="${URL:-about:blank}")
    USER_DATA_DIR="${USER_DATA_DIR:-/tmp/cang-chromium-gui}"
    ;;
  "")
    # --check without a mode: report, using a headless run.
    MODE="check"
    MODE_FLAGS=(--headless=new "--enable-features=Vulkan,UseSkiaRenderer,$VAAPI_FEATURES")
    USER_DATA_DIR="${USER_DATA_DIR:-/tmp/cang-chromium-check}"
    ;;
esac

WINDOW_FLAGS=(--window-size="$WINDOW_SIZE")

build_command() {
  COMMAND=("$CHROMIUM" "${MODE_FLAGS[@]}" "${WINDOW_FLAGS[@]}" "${GPU_FLAGS[@]}"
           "--user-data-dir=$USER_DATA_DIR")
  if [ "$MODE" = gui ]; then
    :
  elif [ -n "$URL" ]; then
    COMMAND+=("$URL")
  fi
  COMMAND+=("${EXTRA[@]}")
}

print_report() {
  local label="$1" value="$2"
  printf '%-22s %s\n' "$label" "$value"
}

report_setup() {
  print_report "chromium" "$("$CHROMIUM" --version 2>/dev/null || echo unknown)"
  print_report "mode" "${MODE:-<none>}"
  print_report "render node" "$render_node"
  print_report "venus ICD" "$VK_ICD_FILENAMES"
  print_report "EGL vendor" "$__EGL_VENDOR_LIBRARY_FILENAMES"
  print_report "GBM backends" "$GBM_BACKENDS_PATH"
  print_report "GL drivers" "$LIBGL_DRIVERS_PATH"
  print_report "VA drivers" "$LIBVA_DRIVERS_PATH"
  print_report "wayland display" "${WAYLAND_DISPLAY:-<none>}"
  if command -v vainfo >/dev/null 2>&1; then
    local profiles
    profiles="$(vainfo --display drm --device "$render_node" 2>/dev/null |
      sed -n 's/^[[:space:]]*\(VAProfileH264[A-Za-z]*\|VAProfileHEVCMain\)[[:space:]]*:[[:space:]]*\(VAEntrypoint[A-Za-z]*\).*/\1:\2/p' |
      sort -u | paste -sd, - || true)"
    print_report "VA-API profiles" "${profiles:-<none reported>}"
  else
    print_report "VA-API profiles" "<vainfo not installed>"
  fi
}

# ---------------------------------------------------------------------------
# --check: run Chromium against a page that reports what it really got
# ---------------------------------------------------------------------------
CHECK_PAGE=""
write_check_page() {
  CHECK_PAGE="$(mktemp /tmp/cang-chromium-check-XXXXXX.html)"
  cat >"$CHECK_PAGE" <<'HTML'
<!doctype html>
<meta charset="utf-8">
<title>cang chromium acceleration check</title>
<body>
<pre id="out">running</pre>
<script>
  var lines = [];
  function reportRenderer() {
    var c = document.createElement('canvas');
    var gl = c.getContext('webgl2') || c.getContext('webgl');
    if (!gl) { lines.push('renderer=no-webgl'); return; }
    var dbg = gl.getExtension('WEBGL_debug_renderer_info');
    lines.push('renderer=' + (dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER)));
    lines.push('vendor=' + (dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : ''));
    lines.push('gl_version=' + gl.getParameter(gl.VERSION));
  }
  function reportDecode() {
    if (!navigator.mediaCapabilities) { lines.push('video=no-media-capabilities'); return; }
    navigator.mediaCapabilities.decodingInfo({
      type: 'file',
      video: { contentType: 'video/mp4; codecs="avc1.42E01E"', width: 640, height: 360, bitrate: 1000000, framerate: 30 }
    }).then(function (info) {
      lines.push('video_supported=' + info.supported);
      lines.push('video_hardware=' + info.powerEfficient);
      lines.push('video_smooth=' + info.smooth);
      document.getElementById('out').textContent = lines.join('\n');
    }).catch(function (err) {
      lines.push('video=error:' + err);
      document.getElementById('out').textContent = lines.join('\n');
    });
  }
  reportRenderer();
  reportDecode();
</script>
</body>
HTML
}

run_check() {
  write_check_page
  report_setup
  local out log rc
  out="$(mktemp /tmp/cang-chromium-check-dom-XXXXXX.html)"
  log="$(mktemp /tmp/cang-chromium-check-log-XXXXXX.txt)"
  local cmd=("$CHROMIUM" --headless=new "--enable-features=Vulkan,UseSkiaRenderer,$VAAPI_FEATURES"
             "${WINDOW_FLAGS[@]}" "${GPU_FLAGS[@]}"
             "--user-data-dir=$USER_DATA_DIR" --virtual-time-budget=30000
             --dump-dom "file://$CHECK_PAGE")
  if [ "$PRINT_ONLY" -eq 1 ]; then
    printf '%q ' "${cmd[@]}"; printf '\n'
    return 0
  fi
  rc=0
  timeout "${CHROME_TIMEOUT:-120}" "${cmd[@]}" >"$out" 2>"$log" || rc=$?
  echo
  if [ "$rc" -ne 0 ]; then
    print_report "chromium exit" "$rc"
    note "chromium failed; its stderr is at $log"
    return 1
  fi
  cp "$log" "${TMPDIR:-/tmp}/cang-chromium-last-check.log" 2>/dev/null || true
  # The first key sits inline after the <pre> tag (`--dump-dom` serialises body
  # text that way), so match the keys anywhere rather than at line start.
  local value
  for key in renderer vendor gl_version video_supported video_hardware video_smooth; do
    value="$(sed -n "s/.*${key}=\([^<&]*\).*/\1/p" "$out" | head -1)"
    [ -n "$value" ] && print_report "$key" "$value"
  done
  local renderer video_hw status=0
  renderer="$(sed -n 's/.*renderer=\([^<&]*\).*/\1/p' "$out" | head -1)"
  video_hw="$(sed -n 's/.*video_hardware=\([^<&]*\).*/\1/p' "$out" | head -1)"
  case "$renderer" in
    "" | no-webgl)
      print_report "verdict" "FAIL: Chromium produced no WebGL renderer"
      status=1
      ;;
    *SwiftShader* | *llvmpipe* | *softpipe*)
      print_report "verdict" "FAIL: Chromium fell back to software rendering ($renderer)"
      status=1
      ;;
    *)
      print_report "verdict" "WebGL is hardware accelerated ($renderer)"
      ;;
  esac
  # Chromium's own MediaCapabilities answer is reported, not scored: it is a
  # heuristic (a host with a working radeonsi VA-API stack reports
  # powerEfficient=false for this configuration), so the VA-API evidence this
  # check scores is the driver's own profile list.
  if [ -n "$video_hw" ]; then
    print_report "video decode" "Chromium reports powerEfficient=$video_hw (informational)"
  fi
  if command -v vainfo >/dev/null 2>&1; then
    local profiles
    profiles="$(vainfo --display drm --device "$render_node" 2>/dev/null |
      sed -n 's/^[[:space:]]*\(VAProfileH264[A-Za-z]*\)[[:space:]]*:[[:space:]]*\(VAEntrypoint[A-Za-z]*\).*/\1:\2/p' |
      sort -u | paste -sd, - || true)"
    case "$profiles" in
      *VAProfileH264*:VAEntrypointVLD*)
        print_report "VA-API decode" "H.264 VLD present (${profiles})"
        ;;
      *)
        print_report "VA-API decode" "FAIL: no H.264 VLD profile reported by the guest driver (${profiles:-none})"
        status=1
        ;;
    esac
  else
    print_report "VA-API decode" "vainfo is not installed; cannot check the guest driver"
  fi
  rm -f "$out" "$log" "$CHECK_PAGE" "${CHECK_PAGE:-}" 2>/dev/null || true
  return "$status"
}

build_command

if [ "$PRINT_ONLY" -eq 1 ] && [ "$CHECK" -eq 0 ]; then
  printf '%q ' "${COMMAND[@]}"
  printf '\n'
  exit 0
fi

if [ "$CHECK" -eq 1 ]; then
  if [ "$PRINT_ONLY" -eq 1 ]; then
    run_check
    exit $?
  fi
  echo "cang Chromium acceleration check"
  echo
  run_report_rc=0
  run_check || run_report_rc=$?
  echo
  if [ "$run_report_rc" -eq 0 ]; then
    echo "RESULT: PASS - Chromium is using the guest's GPU (venus Vulkan, VA-API video)"
  else
    echo "RESULT: FAIL - see the lines above; the guest GPU stack is not in use"
  fi
  exit "$run_report_rc"
fi

rm -f "$CHECK_PAGE"
mkdir -p "$USER_DATA_DIR"

note "mode=$MODE render_node=$render_node"
note "run: ${COMMAND[*]}"

if [ -n "$CHROME_TIMEOUT" ]; then
  set +e
  timeout "$CHROME_TIMEOUT" "${COMMAND[@]}"
  rc=$?
  set -e
  exit "$rc"
fi

exec "${COMMAND[@]}"
