# Live guest evidence: the fixed image starts `zg` under both halves of the fix

Session: pi, 2026-10-16. Guest: cang VM booted from the image built from the
uncommitted fix (`/nix/store/xsl286zj0kjqnpivx9prcjwi3lzlxwx7-cang.tar.gz`,
loaded `localhost/cang:latest` at 13:20:21Z, kernel `7.2.10-hardened1`), driver
`cang` `/nix/store/1gbnhx10nw81fy3n3x65rljgz10ln9lb-cang-0.13.1/bin/cang`, guest
init `...-cang-static-x86_64-unknown-linux-musl-0.13.1/bin/cang-guest-init`,
`--alloc mimalloc`. One bounded VM per arm (`timeout -k 5 900`, host EXIT=0), no
strays after each. Full per-arm raw logs and the runner scripts:
`/home/dev/cang/disk/cxxfix-evidence/{REPORT.md,logs/,arm-scripts/}` - the lines
below were re-grepped from `logs/arm{A,B,C,D}.log` (console mtimes 21:22:34,
21:23:27, 21:24:38, 21:25:53 +0800, all after the image load).

## Image pre-check (offline, `podman image mount`)

```
/etc/ld-nix.so.preload      /nix/store/45n260gs000cmi7fff67gy428yxi26dw-mimalloc-3.5.3/lib/libmimalloc.so
pi wrapper LD_LIBRARY_PATH  /nix/store/xwv4q37ssyhkp1h573bw353pm91axprr-cang-native-addon-runtime/lib
  libstdc++.so.6 ->         /nix/store/j7qx4s4mr17j1wqgvqdzj33lmrnzb387-gcc-16.2.0-lib/lib/libstdc++.so.6.0.36
```

| Arm | What it exercises | Result |
| --- | --- | --- |
| A | guest default env: preload path + `zg --version`/`--help` | PASS `zg_version_rc=0`, `0.3.0` |
| B | the wrapper's injected dir + `zg` as a child of the wrapper | PASS `0.3.0` both ways |
| C | ADR-0009 sanity: `pi --version`, addons with/without the dir | PASS `pi_version_rc=0`; `ort_injected_rc=0`, `sharp` likewise; both fail without it |
| D | causality: pre-fix allocator / pre-fix libstdc++ / fixed combo | PASS D1 `139`, D2 `1`, D3 `0`, D4/D5 `0` |

## Arm A - preload resolves to mimalloc 3.5.3 and `zg` starts

```
+ cat /etc/ld-nix.so.preload
/nix/store/45n260gs000cmi7fff67gy428yxi26dw-mimalloc-3.5.3/lib/libmimalloc.so
+ readlink -f "$P"
/nix/store/45n260gs000cmi7fff67gy428yxi26dw-mimalloc-3.5.3/lib/libmimalloc.so.3.5
+ zg --version
0.3.0
zg_version_rc=0
```

## Arm B - the wrapper's one libstdc++ is now newer than every consumer

```
INJECTED_DIR=/nix/store/xwv4q37ssyhkp1h573bw353pm91axprr-cang-native-addon-runtime/lib
+ readlink -f "$DIR/libstdc++.so.6"
/nix/store/j7qx4s4mr17j1wqgvqdzj33lmrnzb387-gcc-16.2.0-lib/lib/libstdc++.so.6.0.36
+ LD_LIBRARY_PATH="$DIR" zg --version
0.3.0
zg_under_injected_rc=0
+ /workspace/pi-probe                  # the wrapper with its exec line replaced by a probe
IN_WRAPPER LD_LIBRARY_PATH=[/nix/store/xwv4q37ssyhkp1h573bw353pm91axprr-cang-native-addon-runtime/lib]
0.3.0
```

## Arm C - ADR 0009 still holds under the newer runtime

```
+ pi --version
1.2.0
pi_version_rc=0
# without the injected dir (control): ERR_DLOPEN_FAILED libstdc++.so.6: cannot open shared object file
+ LD_LIBRARY_PATH="$DIR" bun -e "require('/workspace/addons/node_modules/onnxruntime-node')"
ort_injected_ok function
ort_injected_rc=0
# sharp: same shape (control fails, injected arm loads)
```

## Arm D - both pre-fix halves reproduce in the same guest

```
+ LD_PRELOAD="$M332" zg --version
/workspace/arm.sh: line 27:   827 Segmentation fault         LD_PRELOAD="$M332" zg --version
D1_rc=139
+ LD_LIBRARY_PATH="$G15" zg --version
zg: .../gcc-15.3.0-lib/lib/libstdc++.so.6: version `GLIBCXX_3.4.35' not found (required by .../libonnxruntime.so.1)
zg: .../gcc-15.3.0-lib/lib/libstdc++.so.6: version `GLIBCXX_3.4.36' not found (required by .../libonnxruntime.so.1)
D2_rc=1
+ LD_PRELOAD="$M353" LD_LIBRARY_PATH="$G16" zg --version
0.3.0
D3_rc=0
+ LD_PRELOAD="$M353" zg --version   # D4 control
0.3.0
D4_rc=0
+ LD_LIBRARY_PATH="$G16" zg --version   # D5 control
0.3.0
D5_rc=0
```

`readelf -V` in the same guest: `...-gcc-15.3.0-lib/libstdc++.so.6` (6.0.34) ends at
`GLIBCXX_3.4.34`; `...-gcc-16.2.0-lib/libstdc++.so.6` (6.0.36) defines
`GLIBCXX_3.4.35` and `3.4.36`.
