#!/bin/sh
# Ticket 06: in-guest proof that the ticket-05 pi wrapper carries libstdc++.so.6
# to a pi extension's glibc addons. Runs INSIDE the cang microVM (default
# allocator, no --alloc flag, no hand-set LD_LIBRARY_PATH).
set -u
E=/workspace/evidence
P=/home/dev/.pi/agent/git/github.com/zeroqn/pi
mkdir -p "$E"
LOG="$E/probe.txt"
: > "$LOG"
say() { echo "$@" | tee -a "$LOG"; }

say "### ticket 06 in-guest addon probe (POST-FIX image)"
say "# date-utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
say "# epoch: $(date +%s)"
say "# id: $(id 2>&1)"
say "# uname: $(uname -a 2>&1)"
say "# HOME=$HOME"
say "# cwd=$(pwd)"
say "# ambient LD_LIBRARY_PATH=[${LD_LIBRARY_PATH-<unset>}]"

say ""
say "### runners"
for t in bun node pi bash sh; do say "# which $t = $(command -v $t 2>&1)"; done
say "# bun --version  = $(bun --version 2>&1)"
say "# node --version = $(node --version 2>&1)"
say "# pi --version   = $(pi --version 2>&1 | head -1)"

say ""
say "### REQUIREMENT 1: which pi store path this image contains"
PI=$(command -v pi)
say "command -v pi        = $PI"
say "readlink -f pi       = $(readlink -f "$PI" 2>&1)"
say "stat -c %F %N        = $(stat -c '%F %N' "$PI" 2>&1)"
say "raw binary test -x   = $(test -x "$(readlink -f "$PI")" && echo yes || echo no)"
say "wrapper refs runtime = $(grep -c 'cang-native-addon-runtime' "$PI" 2>/dev/null)"
say "--- wrapper text (bin/pi) ---"
while IFS= read -r l; do say "  WRAP $l"; done < "$PI"

RUNTIME_DIR=$(grep -o '/nix/store/[a-z0-9]*-cang-native-addon-runtime' "$PI" 2>/dev/null | head -1)
say "runtime dir parsed from wrapper: ${RUNTIME_DIR:-<none>}"
if [ -n "$RUNTIME_DIR" ]; then
  say "runtime dir ls: $(ls -l "$RUNTIME_DIR/lib" 2>&1 | tr '\n' '|')"
  say "runtime dir libstdc target: $(readlink -f "$RUNTIME_DIR/lib/libstdc++.so.6" 2>&1)"
fi

say ""
say "### REQUIREMENT 3a: the LD_LIBRARY_PATH the wrapper itself computes"
# Mechanically execute the wrapper with its final exec replaced by a print, so
# the value is exactly the one the wrapper exports, not a hand-written guess.
WENV=/tmp/pi-wrapper-env.sh
sed -e 's|^exec .*|echo "WRAPPER_EXPORTED_LD_LIBRARY_PATH=$LD_LIBRARY_PATH"|' "$PI" > "$WENV"
chmod +x "$WENV"
WRAPPED_LP=$("$WENV" 2>&1 | sed -n 's/^WRAPPER_EXPORTED_LD_LIBRARY_PATH=//p')
say "wrapper exported value: ${WRAPPED_LP:-<none>}"
say "sed-rewrote-exec check (must not equal the wrapper): $(cmp -s "$WENV" "$PI" && echo IDENTICAL-BAD || echo REWRITTEN-OK)"

say ""
say "### REQUIREMENT 3b: LD_LIBRARY_PATH of a live pi process (/proc/<pid>/environ)"
( script -q -e -c "timeout 6 pi" /dev/null >/tmp/pi-live.log 2>&1 ) &
sleep 3
LPIDS=$(pgrep -f 'lib/pi-coding-agent/pi' 2>/dev/null | tr '\n' ' ')
say "pgrep -f lib/pi-coding-agent/pi = [${LPIDS:-<none>}]"
LIVE_LP=""
for pid in $LPIDS; do
  if [ -r "/proc/$pid/environ" ]; then
    cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
    lp=$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep '^LD_LIBRARY_PATH=' | head -1)
    say "  live-pid=$pid cmdline=$cmd"
    say "  live-pid=$pid $lp"
    [ -n "$lp" ] && LIVE_LP="${lp#LD_LIBRARY_PATH=}"
  fi
done
say "live pi LD_LIBRARY_PATH = [${LIVE_LP:-<none captured>}]"
# cleanup any surviving pi
pkill -f 'lib/pi-coding-agent/pi' 2>/dev/null
sleep 1

if [ -z "${WRAPPED_LP:-}" ]; then
  say "FATAL: could not derive the wrapper's exported LD_LIBRARY_PATH"
fi

say ""
say "### /etc/ld-nix.so.preload + loader facts"
say "ls -l preload: $(ls -l /etc/ld-nix.so.preload 2>&1)"
if [ -r /etc/ld-nix.so.preload ]; then
  while IFS= read -r l; do say "preload: $l"; done < /etc/ld-nix.so.preload
else
  say "preload-file-unreadable"
fi
say "ld.so.cache: $(ls -l /etc/ld.so.cache 2>&1)"
say "libstdc via ldconfig dirs: /lib/libstdc++.so.6=$(ls -l /lib/libstdc++.so.6 2>&1) /usr/lib/libstdc++.so.6=$(ls -l /usr/lib/libstdc++.so.6 2>&1)"

say ""
say "### addon files"
say "sharp node: $(ls -l "$P/node_modules/@img/sharp-linux-x64/lib/" 2>&1 | tr '\n' '|')"
say "onnx node:  $(ls -l "$P/node_modules/onnxruntime-node/bin/napi-v6/linux/x64/" 2>&1 | tr '\n' '|')"

# ---------------------------------------------------------------- probe cases
probe_case() { # <label> <mode:plain|wrapper> <runner> <module>
  label="$1"; mode="$2"; runner="$3"; mod="$4"
  tag="$(printf '%s' "$label-$runner-$mod" | tr -c 'a-zA-Z0-9' '_')"
  cout="$E/case-$tag.out"; cerr="$E/case-$tag.err"; dbg="$E/lddebug-$tag.txt"
  if [ "$mode" = wrapper ]; then
    ( cd "$P" && LD_LIBRARY_PATH="$WRAPPED_LP" "$runner" -e "try{require('$mod');console.log('LOAD_OK')}catch(e){console.log('LOAD_FAIL: '+e.message)}" ) >"$cout" 2>"$cerr"
    rc=$?
    ( cd "$P" && LD_LIBRARY_PATH="$WRAPPED_LP" LD_DEBUG=libs "$runner" -e "try{require('$mod')}catch(e){}" ) >/dev/null 2>"$dbg"
  else
    ( cd "$P" && "$runner" -e "try{require('$mod');console.log('LOAD_OK')}catch(e){console.log('LOAD_FAIL: '+e.message)}" ) >"$cout" 2>"$cerr"
    rc=$?
    ( cd "$P" && LD_DEBUG=libs "$runner" -e "try{require('$mod')}catch(e){}" ) >/dev/null 2>"$dbg"
  fi
  verdict=$(grep -q '^LOAD_OK$' "$cout" && echo LOAD_OK || echo LOAD_FAIL)
  say ""
  say "### CASE [$label] LDPATH=${mode} $runner require('$mod')"
  say "exit=$rc verdict=$verdict"
  say "stdout: $(cat "$cout" 2>&1 | tr '\n' '|')"
  say "stderr: $(head -3 "$cerr" 2>&1 | tr '\n' '|')"
  say "$verdict" > "$E/verdict-$tag.txt"
}

# control: same bun, variable UNSET -> must FAIL again
probe_case plain-plain   plain   bun  sharp
probe_case plain-plain   plain   bun  onnxruntime-node
# wrapper-supplied LD_LIBRARY_PATH -> must LOAD_OK
probe_case wrapper-env   wrapper bun  sharp
probe_case wrapper-env   wrapper bun  onnxruntime-node
# node is NOT evidence of the fix (links libstdc++ itself)
probe_case plain-node    plain   node sharp
probe_case plain-node    plain   node onnxruntime-node

say ""
say "### LD_DEBUG: how libstdc++.so.6 now resolves (wrapper env, bun sharp)"
grep -i 'libstdc' "$E/lddebug-wrapper_env_bun_sharp.txt" 2>/dev/null | head -30 | while IFS= read -r l; do say "  DBG $l"; done
say ""
say "### LD_DEBUG: resolved path contains the runtime dir?"
grep -i 'cang-native-addon-runtime' "$E/lddebug-wrapper_env_bun_sharp.txt" 2>/dev/null | head -10 | while IFS= read -r l; do say "  DBG $l"; done
say ""
say "### LD_DEBUG (control): libstdc++ lookup under plain bun sharp"
grep -i 'libstdc' "$E/lddebug-plain_plain_bun_sharp.txt" 2>/dev/null | head -12 | while IFS= read -r l; do say "  DBG $l"; done

say ""
say "### SUMMARY"
say "pi_store_path=$PI"
say "pi_raw_binary=$(readlink -f "$PI" 2>&1)"
say "runtime_dir=${RUNTIME_DIR:-<none>}"
say "wrapper_exported_LD_LIBRARY_PATH=${WRAPPED_LP:-<none>}"
say "live_pi_LD_LIBRARY_PATH=${LIVE_LP:-<none captured>}"
say "bun_sharp_plain=$(cat "$E/verdict-plain_plain_bun_sharp.txt" 2>/dev/null)"
say "bun_onnx_plain=$(cat "$E/verdict-plain_plain_bun_onnxruntime_node.txt" 2>/dev/null)"
say "bun_sharp_wrapper=$(cat "$E/verdict-wrapper_env_bun_sharp.txt" 2>/dev/null)"
say "bun_onnx_wrapper=$(cat "$E/verdict-wrapper_env_bun_onnxruntime_node.txt" 2>/dev/null)"
say "node_sharp_plain=$(cat "$E/verdict-plain_node_node_sharp.txt" 2>/dev/null)"
say "node_onnx_plain=$(cat "$E/verdict-plain_node_node_onnxruntime_node.txt" 2>/dev/null)"
say "### DONE"

