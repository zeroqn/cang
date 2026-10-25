#!/bin/sh
# Generate the rust-compile-bench workload: a deterministic, network-free crate
# graph that exercises rustc's file mix (thousands of small sources, one rlib +
# rmeta + dep-info + fingerprint dir per crate, a three-level dependency chain).
#
# Usage: gen-workload.sh <outdir>
#   LEAVES   number of leaf crates (default 1800: ~2.4k crates, ~19k files)
#   MODULES  modules per leaf crate (default 8)
#   LINES    assignments per module function (default 120)
#
# No external crates: `cargo build` never touches the network.
set -eu

out=${1:?usage: gen-workload.sh <outdir>}
leaves=${LEAVES:-1800}
modules=${MODULES:-8}
lines=${LINES:-120}

mid1n=$((leaves / 4))
mid2n=$((mid1n / 3))
if [ "$mid1n" -lt 4 ] || [ "$mid2n" -lt 1 ]; then
    echo "gen-workload: LEAVES too small" >&2
    exit 2
fi

rm -rf "$out"
mkdir -p "$out/crates" "$out/.cargo"

cat >"$out/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["crates/*"]

[profile.dev]
debug = 0
incremental = false
EOF

cat >"$out/.cargo/config.toml" <<'EOF'
[net]
offline = true
EOF

body=$(awk -v n="$lines" 'BEGIN { for (i = 1; i <= n; i++) printf "    acc = acc.wrapping_mul(31).wrapping_add(%d);\n", (i * 37) % 251 }')

i=1
while [ "$i" -le "$leaves" ]; do
    name=$(printf 'leaf_%04d' "$i")
    dir="$out/crates/$name"
    mkdir -p "$dir/src"
    cat >"$dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.1.0"
edition = "2021"
EOF
    : >"$dir/src/lib.rs"
    m=0
    while [ "$m" -lt "$modules" ]; do
        printf 'pub mod m%d;\n' "$m" >>"$dir/src/lib.rs"
        cat >"$dir/src/m$m.rs" <<EOF
pub struct S$m {
    pub a: u64,
    pub b: u64,
}

pub fn f(x: u64) -> u64 {
    let mut acc = x ^ $((i * 131 + m));
$body
    acc
}
EOF
        m=$((m + 1))
    done
    printf '\npub fn value() -> u64 {\n    let mut acc = 0u64;\n' >>"$dir/src/lib.rs"
    m=0
    while [ "$m" -lt "$modules" ]; do
        printf '    acc ^= m%d::f(%d);\n' "$m" "$m" >>"$dir/src/lib.rs"
        m=$((m + 1))
    done
    printf '    acc\n}\n' >>"$dir/src/lib.rs"
    i=$((i + 1))
done

j=1
while [ "$j" -le "$mid1n" ]; do
    name=$(printf 'mid1_%04d' "$j")
    dir="$out/crates/$name"
    mkdir -p "$dir/src"
    cat >"$dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.1.0"
edition = "2021"

[dependencies]
EOF
    : >"$dir/src/lib.rs"
    k=0
    while [ "$k" -lt 4 ]; do
        leaf=$(printf 'leaf_%04d' "$(( (j - 1) * 4 + k + 1 ))")
        printf '%s = { path = "../%s" }\n' "$leaf" "$leaf" >>"$dir/Cargo.toml"
        printf 'pub use %s as dep%d;\n' "$leaf" "$k" >>"$dir/src/lib.rs"
        k=$((k + 1))
    done
    printf '\npub fn value() -> u64 {\n    dep0::value() ^ dep1::value() ^ dep2::value() ^ dep3::value()\n}\n' >>"$dir/src/lib.rs"
    j=$((j + 1))
done

k=1
while [ "$k" -le "$mid2n" ]; do
    name=$(printf 'mid2_%04d' "$k")
    dir="$out/crates/$name"
    mkdir -p "$dir/src"
    cat >"$dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.1.0"
edition = "2021"

[dependencies]
EOF
    : >"$dir/src/lib.rs"
    d=0
    while [ "$d" -lt 3 ]; do
        mid=$(printf 'mid1_%04d' "$(( (k - 1) * 3 + d + 1 ))")
        printf '%s = { path = "../%s" }\n' "$mid" "$mid" >>"$dir/Cargo.toml"
        printf 'pub use %s as dep%d;\n' "$mid" "$d" >>"$dir/src/lib.rs"
        d=$((d + 1))
    done
    printf '\npub fn value() -> u64 {\n    dep0::value() ^ dep1::value() ^ dep2::value()\n}\n' >>"$dir/src/lib.rs"
    k=$((k + 1))
done

top="$out/crates/bench_top"
mkdir -p "$top/src"
{
    cat <<EOF
[package]
name = "bench_top"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "bench_top"
path = "src/main.rs"

[dependencies]
EOF
    k=1
    while [ "$k" -le "$mid2n" ]; do
        printf 'mid2_%04d = { path = "../mid2_%04d" }\n' "$k" "$k" >>"$top/Cargo.toml"
        k=$((k + 1))
    done
} >"$top/Cargo.toml"

{
    printf 'fn main() {\n    let mut acc = 0u64;\n'
    k=1
    while [ "$k" -le "$mid2n" ]; do
        printf '    acc ^= mid2_%04d::value();\n' "$k"
        k=$((k + 1))
    done
    printf '    println!("{acc}");\n}\n'
} >"$top/src/main.rs"

printf 'gen-workload: %s crates (%s leaves, %s mid1, %s mid2, 1 bin)\n' \
    "$((leaves + mid1n + mid2n + 1))" "$leaves" "$mid1n" "$mid2n"
