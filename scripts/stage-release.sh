#!/usr/bin/env bash
# Build every Aseman release binary and stage the release tree in OUT (never in the
# source tree). The release workflow and local image builds both use it:
#
#   scripts/stage-release.sh OUT
#
# The native backend links WasmEdge; set WASMEDGE_DIR to an unpacked WasmEdge SDK
# (the version pinned in contracts/release/runtime-dependencies.json) before building.
# WasmEdge itself is not staged: scripts/install.sh installs it beside the binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARIES=(aseman-node aseman-keygen asemanctl aseman-vmm aseman-meter aseman-vmm-agent
          aseman-vmm-backend-nomad aseman-vmm-backend-native)

out="${1:?usage: stage-release.sh OUT}"
case "$(realpath -m "$out")" in
  "$ROOT"|"$ROOT"/*) echo "stage-release: OUT must be outside the source tree" >&2; exit 1 ;;
esac

arguments=()
for binary in "${BINARIES[@]}"; do arguments+=(--bin "$binary"); done
(cd "$ROOT" && cargo build --release --locked "${arguments[@]}")

rm -rf "$out"
mkdir -p "$out/bin"
for binary in "${BINARIES[@]}"; do
  install -m 0755 "$ROOT/target/release/$binary" "$out/bin/$binary"
done
install -m 0644 "$ROOT/contracts/release/runtime-dependencies.json" "$out/runtime-dependencies.json"
# The deployment profiles asemanctl bootstrap drives, found beside the binaries.
mkdir -p "$out/share/aseman/compose"
install -m 0644 "$ROOT"/deploy/compose/*.yaml "$ROOT/deploy/compose/compact.env.example" \
  "$out/share/aseman/compose/"

# Every binary but the native backend is self-contained apart from the C library; the
# backend must find WasmEdge through its RPATH once installed.
for binary in "${BINARIES[@]}"; do
  if [[ "$binary" != aseman-vmm-backend-native ]] && ldd "$out/bin/$binary" | grep -q 'not found'; then
    ldd "$out/bin/$binary" >&2
    echo "stage-release: $binary has unresolved libraries" >&2
    exit 1
  fi
done
readelf -d "$out/bin/aseman-vmm-backend-native" | grep -q 'lib/aseman' \
  || { echo "stage-release: the native backend lacks its lib/aseman RPATH" >&2; exit 1; }
echo "stage-release: staged ${#BINARIES[@]} binaries in $out"
