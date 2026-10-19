#!/usr/bin/env bash
# Usage: bench/aot/ceiling.sh IETS OUTDIR
# The IR cache ceiling of docs/explanation/precompiled-ir.md: each workload
# with no IR cache, with a cache it fills (cold), and with a warm one.
# NIXPKGS defaults to the flake.lock pin; hyperfine must be on PATH.
set -eu
IETS=$(realpath "$1")
OUT=$2
mkdir -p "$OUT"
NP=${NIXPKGS:-/nix/store/r5l65vg4gi4dnx54zkwagfb8jmlqniy5-source}
W=$(realpath "$(dirname "$0")/../workloads")
C=$OUT/cache
echo '(import <nixpkgs> { system = "x86_64-linux"; }).hello.drvPath' > "$OUT/hello.nix"
echo "import $NP/ci/eval/outpaths.nix { path = $NP; systems = [ \"x86_64-linux\" ]; includeBroken = false; }" > "$OUT/outpaths.nix"
for w in 1 4; do
  for wl in "$OUT/hello.nix" "$W/nixos-server.nix"; do
    n=$(basename "$wl" .nix)
    base="$IETS eval --impure --strict --no-tracing-cache --workers $w -I nixpkgs=$NP $wl"
    hyperfine --shell=none --warmup 2 --runs 10 --export-json "$OUT/$n-w$w.json" \
      --prepare true --prepare "rm -rf $C" --prepare true \
      -n "no-ir-cache" "$base --no-ir-cache" \
      -n "cold (write)" "$base --cache-dir $C" \
      -n "warm" "$base --cache-dir $C"
  done
  base="$IETS eval-jobs --impure --no-instantiate --workers $w --chunks 8 --chunk 0 -I nixpkgs=$NP $OUT/outpaths.nix"
  $base --cache-dir "$C-op" > /dev/null
  hyperfine --shell=none --runs 3 --export-json "$OUT/outpaths-w$w.json" \
    -n "no-ir-cache" "$base --no-ir-cache" \
    -n "warm" "$base --cache-dir $C-op"
done
