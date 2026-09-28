#!/usr/bin/env bash
# Install an Aseman release and the third-party runtime libraries it needs.
#
#   curl -fsSL https://github.com/cosmopole-org/Aseman/releases/download/vX.Y.Z/install.sh \
#     | bash -s -- --version vX.Y.Z
#
# Everything downloaded is verified before use: the release archive against the
# release's SHA256SUMS, and every third-party archive against the digest pinned below
# (the pins mirror contracts/release/runtime-dependencies.json and are checked against
# it in CI). Re-running is safe: a verified install is replaced atomically.
#
# Layout under --prefix (default /opt/aseman):
#   bin/          aseman-node asemanctl aseman-vmm aseman-meter aseman-keygen
#                 aseman-vmm-agent aseman-vmm-backend-nomad aseman-vmm-backend-native
#                 [firecracker]
#   lib/aseman/   libwasmedge.so.0 (found by the backend through its $ORIGIN RPATH)
#
# Options:
#   --version TAG        release tag to install (required unless --archive is given)
#   --prefix DIR         install root (default /opt/aseman)
#   --archive FILE       install from a downloaded aseman-dist-<arch>.tgz instead;
#                        its SHA256SUMS must sit beside it
#   --repo OWNER/NAME    GitHub repository (default cosmopole-org/Aseman)
#   --with-firecracker   also install Firecracker for KVM-capable worker hosts
#   --no-link            do not symlink the binaries into /usr/local/bin
set -euo pipefail

WASMEDGE_VERSION="0.17.1"
WASMEDGE_AMD64_URL="https://github.com/WasmEdge/WasmEdge/releases/download/0.17.1/WasmEdge-0.17.1-manylinux_2_28_x86_64.tar.gz"
WASMEDGE_AMD64_SHA256="27a1abec072ddf45b40e2e81e33c1e5fe9b241f31fd1bbf0182f05097489a07a"
WASMEDGE_ARM64_URL="https://github.com/WasmEdge/WasmEdge/releases/download/0.17.1/WasmEdge-0.17.1-manylinux_2_28_aarch64.tar.gz"
WASMEDGE_ARM64_SHA256="6d7762429083e787ccbddf629868bb59de4325ccdcc31d9f7bd240adcdd9fe9d"
FIRECRACKER_VERSION="1.17.0"
FIRECRACKER_AMD64_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz"
FIRECRACKER_AMD64_SHA256="06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558"
FIRECRACKER_ARM64_URL="https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-aarch64.tgz"
FIRECRACKER_ARM64_SHA256="e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256"

BINARIES=(aseman-node asemanctl aseman-vmm aseman-meter aseman-keygen aseman-vmm-agent
          aseman-vmm-backend-nomad aseman-vmm-backend-native)

version=""
prefix="/opt/aseman"
archive=""
repo="cosmopole-org/Aseman"
with_firecracker=false
link=true

usage() {
  cat <<'USAGE'
install.sh (--version TAG | --archive FILE) [--prefix DIR] [--repo OWNER/NAME]
           [--with-firecracker] [--no-link]
Installs the Aseman binaries plus a checksum-verified WasmEdge (and optionally
Firecracker) into --prefix (default /opt/aseman).
USAGE
}
die() { echo "install: $*" >&2; exit 1; }
say() { echo "install: $*"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version="${2:?}"; shift 2 ;;
    --prefix) prefix="${2:?}"; shift 2 ;;
    --archive) archive="${2:?}"; shift 2 ;;
    --repo) repo="${2:?}"; shift 2 ;;
    --with-firecracker) with_firecracker=true; shift ;;
    --no-link) link=false; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done

[[ "$(uname -s)" == "Linux" ]] || die "Aseman runs on Linux"
case "$(uname -m)" in
  x86_64|amd64) arch=amd64; uname_arch=x86_64 ;;
  aarch64|arm64) arch=arm64; uname_arch=aarch64 ;;
  *) die "unsupported architecture $(uname -m)" ;;
esac
for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
if [[ -z "$archive" && -z "$version" ]]; then
  die "--version TAG or --archive FILE is required"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fetch() { # url destination
  curl --fail --location --silent --show-error --retry 3 --proto '=https' --tlsv1.2 \
    -o "$2" "$1" || die "download failed: $1"
}

verify() { # file expected-sha256
  local actual
  actual="$(sha256sum "$1" | cut -d' ' -f1)"
  [[ "$actual" == "$2" ]] || die "checksum mismatch for $(basename "$1"): expected $2, got $actual"
}

# 1. The Aseman release archive, verified against the release's SHA256SUMS.
name="aseman-dist-${arch}.tgz"
if [[ -n "$archive" ]]; then
  [[ -f "$archive" ]] || die "no archive at $archive"
  cp "$archive" "$work/$name"
  sums="$(dirname "$archive")/aseman-dist-${arch}.SHA256SUMS"
  [[ -f "$sums" ]] || die "no aseman-dist-${arch}.SHA256SUMS beside $archive"
  cp "$sums" "$work/SHA256SUMS"
else
  base="https://github.com/${repo}/releases/download/${version}"
  say "downloading Aseman ${version} (${arch})"
  fetch "$base/$name" "$work/$name"
  fetch "$base/aseman-dist-${arch}.SHA256SUMS" "$work/SHA256SUMS"
fi
expected="$(awk -v n="$name" '$2 == n || $2 == "*"n {print $1}' "$work/SHA256SUMS")"
[[ -n "$expected" ]] || die "SHA256SUMS does not list $name"
verify "$work/$name" "$expected"
mkdir -p "$work/release"
tar -xzf "$work/$name" -C "$work/release"
for binary in "${BINARIES[@]}"; do
  [[ -x "$work/release/bin/$binary" ]] || die "release archive lacks bin/$binary"
done

# 2. WasmEdge, needed by the native VMM backend.
if [[ "$arch" == amd64 ]]; then url="$WASMEDGE_AMD64_URL"; sum="$WASMEDGE_AMD64_SHA256";
else url="$WASMEDGE_ARM64_URL"; sum="$WASMEDGE_ARM64_SHA256"; fi
say "downloading WasmEdge ${WASMEDGE_VERSION}"
fetch "$url" "$work/wasmedge.tgz"
verify "$work/wasmedge.tgz" "$sum"
mkdir -p "$work/wasmedge" "$work/release/lib/aseman"
tar -xzf "$work/wasmedge.tgz" -C "$work/wasmedge" lib64/libwasmedge.so.0.1.1
install -m 0644 "$work/wasmedge/lib64/libwasmedge.so.0.1.1" "$work/release/lib/aseman/"
ln -sf libwasmedge.so.0.1.1 "$work/release/lib/aseman/libwasmedge.so.0"

# 3. Firecracker, only for KVM worker hosts.
if $with_firecracker; then
  if [[ "$arch" == amd64 ]]; then url="$FIRECRACKER_AMD64_URL"; sum="$FIRECRACKER_AMD64_SHA256";
  else url="$FIRECRACKER_ARM64_URL"; sum="$FIRECRACKER_ARM64_SHA256"; fi
  say "downloading Firecracker ${FIRECRACKER_VERSION}"
  fetch "$url" "$work/firecracker.tgz"
  verify "$work/firecracker.tgz" "$sum"
  member="release-v${FIRECRACKER_VERSION}-${uname_arch}/firecracker-v${FIRECRACKER_VERSION}-${uname_arch}"
  tar -xzf "$work/firecracker.tgz" -C "$work" "$member"
  install -m 0755 "$work/$member" "$work/release/bin/firecracker"
fi

# 4. The binaries must resolve every shared library from this layout.
if command -v ldd >/dev/null; then
  missing="$(ldd "$work/release/bin/aseman-vmm-backend-native" | grep 'not found' || true)"
  [[ -z "$missing" ]] || die "the native backend cannot resolve: $missing"
fi

# 5. Swap the new tree in atomically; the previous one is kept for rollback.
mkdir -p "$(dirname "$prefix")"
staged="${prefix}.new.$$"
rm -rf "$staged"
cp -a "$work/release" "$staged"
if [[ -e "$prefix" ]]; then
  rm -rf "${prefix}.previous"
  mv "$prefix" "${prefix}.previous"
fi
mv "$staged" "$prefix"

if $link; then
  for binary in "${BINARIES[@]}" $($with_firecracker && echo firecracker); do
    ln -sf "$prefix/bin/$binary" "/usr/local/bin/$binary" 2>/dev/null \
      || say "could not link /usr/local/bin/$binary (add $prefix/bin to PATH)"
  done
fi
say "installed into $prefix$([[ -e "${prefix}.previous" ]] && echo " (previous kept at ${prefix}.previous)")"
say "next: asemanctl bootstrap --profile compact"
