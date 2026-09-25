#!/usr/bin/env bash
# run-nodes.sh — unified Caspar node runner
#
# Usage:
#   ./run-nodes.sh [single|triple] [OPTIONS]
#
# Modes:
#   single  — run only node1
#   triple  — run node1 + node2 + node3 (default)
#
# Options:
#   --no-docker      Run nodes as local binaries instead of docker containers.
#                    (Default: docker. Local mode supports triple-node without
#                    port or data-dir conflicts — each node has its own ports.)
#   --no-questdb     Skip QuestDB startup (manage it separately)
#   --fresh          Wipe /tmp/caspar/* before starting (clean-slate run)
#   --no-gvisor      Skip gVisor (runsc) install / configuration. By default
#                    gVisor is installed and registered with Docker so all
#                    caspar VMs run sandboxed.
#   --no-firecracker Skip Firecracker install and network setup. By default
#                    Firecracker is installed and its host bridge is configured
#                    so microVM-backed workloads can run immediately.
#   --no-rebuild     Skip all build steps and use the existing dist/ binaries/libs
#                    (local mode) or existing Docker image as-is.  By default the
#                    build is always run so the cluster reflects the latest source.
#                    Errors out if the required artifact is missing.
#   --foreground     (docker mode) keep tailing container logs until Ctrl-C
#                    instead of returning immediately
#   --skip-deploy    Skip WASM creature build & deployment. By default
#                    run-nodes.sh clones decillionai-server (if absent), builds
#                    all WASM creatures with TinyGo, and deploys them so the
#                    cluster is fully ready before the script exits.
#   --help           show this help
#
# Companion script:
#   ./stop-nodes.sh  — gracefully shuts down everything started by this script

set -euo pipefail

# ─── PATH: native Linux docker locations first ───────────────────────────────
# Put native Linux paths BEFORE the Windows-mounted paths that WSL appends.
# This prevents Windows Docker Desktop's docker.exe (reachable via /mnt/c/...)
# from being used instead of a native Docker CE install.
export PATH="/snap/bin:/usr/local/bin:/usr/bin:/usr/sbin:${PATH:-}"

# _native_docker: returns true only if a native Linux docker binary exists.
# Rejects .exe wrappers and Windows-mount paths (/mnt/...).
_native_docker_exists() {
  local p
  p=$(command -v docker 2>/dev/null) || return 1
  # Reject Windows-mount paths and .exe wrappers
  [[ "$p" == /mnt/* ]] && return 1
  [[ "$p" == *.exe  ]] && return 1
  return 0
}

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Prefer the pre-built dist binary; fall back to the cargo build output.
BINARY="$REPO_DIR/target/release/caspar-node"
[[ -x "$REPO_DIR/dist/bin/caspar-node" ]] && BINARY="$REPO_DIR/dist/bin/caspar-node"
DATA_ROOT="/tmp/caspar"
# Use the pre-built jar from dist/ if /opt/questdb/questdb.jar is absent
QUESTDB_JAR="${QUESTDB_JAR:-}"
[[ -z "$QUESTDB_JAR" ]] && [[ -f "/opt/questdb/questdb.jar" ]] && QUESTDB_JAR="/opt/questdb/questdb.jar"
[[ -z "$QUESTDB_JAR" ]] && [[ -f "$REPO_DIR/dist/questdb/questdb.jar" ]] && QUESTDB_JAR="$REPO_DIR/dist/questdb/questdb.jar"
[[ -z "$QUESTDB_JAR" ]] && QUESTDB_JAR="/opt/questdb/questdb.jar"  # keep original as default for download logic
QUESTDB_DATA="$DATA_ROOT/questdb"
QUESTDB_PORT=8812
DOCKER_IMAGE="caspar-node:latest"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info()  { echo -e "${CYAN}[caspar]${NC} $*"; }
ok()    { echo -e "${GREEN}[caspar]${NC} $*"; }
warn()  { echo -e "${YELLOW}[caspar]${NC} $*"; }
die()   { echo -e "${RED}[caspar] FATAL:${NC} $*" >&2; exit 1; }

# ─── Environment detection ────────────────────────────────────────────────────
# Detect WSL: /proc/version contains "Microsoft" or "WSL"
_is_wsl() { grep -qi 'microsoft\|wsl' /proc/version 2>/dev/null; }
# Detect systemd as PID 1 (works bare-metal AND WSL2 with systemd enabled)
_has_systemd() { [[ "$(ps -p 1 -o comm= 2>/dev/null)" == "systemd" ]]; }
# Return the active package manager token: apt | dnf | yum | pacman | unknown
_pkg_mgr() {
  command -v apt-get &>/dev/null && { echo apt;    return; }
  command -v dnf     &>/dev/null && { echo dnf;    return; }
  command -v yum     &>/dev/null && { echo yum;    return; }
  command -v pacman  &>/dev/null && { echo pacman; return; }
  echo unknown
}
# Normalise uname -m → Docker/apt arch token (amd64 / arm64 / armhf)
_arch() {
  case "$(uname -m)" in
    x86_64)        echo amd64 ;;
    aarch64|arm64) echo arm64 ;;
    armv7l)        echo armhf ;;
    *)             uname -m   ;;
  esac
}
# Install packages via the appropriate manager (Debian/Ubuntu, RHEL/Fedora, Arch)
_pkg_install() {
  case "$(_pkg_mgr)" in
    apt)    apt-get install -y -qq "$@" ;;
    dnf)    dnf     install -y -q  "$@" ;;
    yum)    yum     install -y -q  "$@" ;;
    pacman) pacman  -S --noconfirm "$@" ;;
    *)      warn "Unknown package manager — install manually: $*"; return 1 ;;
  esac
}
_pkg_update() {
  case "$(_pkg_mgr)" in
    apt)    apt-get update -qq ;;
    dnf|yum) : ;;  # dnf/yum update on demand; skip explicit refresh
    pacman) pacman -Sy --noconfirm ;;
    *) : ;;
  esac
}

# ─── Arg parsing ─────────────────────────────────────────────────────────────
MODE="triple"
USE_DOCKER=true
FRESH=false
SETUP_GVISOR=true
SETUP_FIRECRACKER=true
NO_REBUILD=false
FOREGROUND=false
DEPLOY_CREATURES=true

for arg in "$@"; do
  case "$arg" in
    single)            MODE="single" ;;
    triple)            MODE="triple" ;;
    --no-docker)       USE_DOCKER=false ;;
    --no-questdb)      warn "--no-questdb is ignored: each node requires its own QuestDB instance to start" ;;
    --fresh)           FRESH=true ;;
    --no-gvisor)       SETUP_GVISOR=false ;;
    --no-firecracker)  SETUP_FIRECRACKER=false ;;
    --no-rebuild)      NO_REBUILD=true ;;
    --foreground)      FOREGROUND=true ;;
    --skip-deploy)     DEPLOY_CREATURES=false ;;
    --help|-h)
      sed -n '2,34p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "Unknown argument: $arg" ;;
  esac
done

NODES=(1)
[[ "$MODE" == "triple" ]] && NODES=(1 2 3)

declare -A NODE_TCP=([1]=8074 [2]=8174 [3]=8274)

# ─── Sudo pre-check ──────────────────────────────────────────────────────────
# If Docker is not yet installed we will need root. Validate sudo credentials
# NOW (interactively) so subsequent _as_root calls never hang waiting for a
# password on a background/piped invocation.
if $USE_DOCKER && ! _native_docker_exists; then
  if [[ "$EUID" -ne 0 ]] && command -v sudo &>/dev/null; then
    echo -e "${CYAN}[caspar]${NC} Docker CE not installed — root access is needed."
    echo -e "${CYAN}[caspar]${NC} Please enter your sudo password when prompted:"
    sudo -v || die "sudo authentication failed. Run 'sudo -v' first, then re-run this script."
    # Keep sudo alive in the background for the duration of the script
    ( while true; do sudo -n true; sleep 50; done ) &
    _SUDO_KEEPALIVE_PID=$!
    trap 'kill $_SUDO_KEEPALIVE_PID 2>/dev/null' EXIT
    ok "sudo credentials cached"
  fi
fi

# ─── Helper: run a function body as root ─────────────────────────────────────
# Usage: _as_root <function_name>
# Calls the named function directly if already root, otherwise exports its
# definition and re-invokes it through sudo bash.  Colour helper functions
# (info/ok/warn) are exported alongside so output remains consistent.
_as_root() {
  local fn="$1"
  if [[ "$EUID" -eq 0 ]]; then
    "$fn"
    return
  fi
  command -v sudo &>/dev/null || { warn "Cannot run $fn: need root and sudo is not available."; return 1; }

  # Write all function definitions to a temp file so heredocs (e.g. <<'PYEOF'
  # in _setup_gvisor) are preserved correctly — `bash -c "..."` misparses
  # heredocs embedded inside a string and silently drops later definitions.
  local tmpscript
  tmpscript=$(mktemp /tmp/caspar_root_XXXXXX.sh)
  # shellcheck disable=SC2064
  trap "rm -f '$tmpscript'" RETURN

  # Dump every currently-defined function, then call the requested one.
  declare -f              >> "$tmpscript"
  echo "${fn}"            >> "$tmpscript"
  chmod 700 "$tmpscript"

  sudo bash "$tmpscript"
}

# ─── gVisor setup ─────────────────────────────────────────────────────────────
#
# Snap-docker compatibility notes:
#   • Snap docker reads its daemon.json from /var/snap/docker/<rev>/config/daemon.json
#     NOT from /etc/docker/daemon.json — we detect this and write to the right path.
#   • Snap docker uses strict confinement: external runtime binaries (runsc) may be
#     blocked unless the snap has the right interfaces. We register runsc and warn if
#     confinement might prevent execution. For full gVisor support, Docker CE (apt)
#     is recommended over snap docker.
#   • Restart is done via `snap restart docker` instead of `systemctl restart docker`.
_setup_gvisor() {
  set -e
  info "Installing gVisor (runsc)…"

  if [[ "$(_pkg_mgr)" == apt ]]; then
    # ── Debian / Ubuntu path (official apt repo) ──────────────────────────────
    _pkg_update
    _pkg_install apt-transport-https ca-certificates curl gnupg

    local keyring="/usr/share/keyrings/gvisor-archive-keyring.gpg"
    [[ -f "$keyring" ]] \
      || curl -fsSL https://gvisor.dev/archive.key | gpg --dearmor --yes -o "$keyring"

    local arch; arch=$(_arch)
    echo "deb [arch=${arch} signed-by=${keyring}] https://storage.googleapis.com/gvisor/releases release main" \
      > /etc/apt/sources.list.d/gvisor.list

    _pkg_update
    _pkg_install runsc
  else
    # ── Non-Debian: direct binary download from gVisor release CDN ───────────
    local hw_arch; hw_arch=$(uname -m)   # x86_64 or aarch64
    local runsc_url="https://storage.googleapis.com/gvisor/releases/release/latest/${hw_arch}/runsc"
    info "Non-apt system — downloading runsc binary for ${hw_arch}…"
    curl -fsSL "$runsc_url" -o /usr/local/bin/runsc
    chmod +x /usr/local/bin/runsc
    ok "runsc installed: $(runsc --version 2>&1 | head -1)"
  fi

  # ── Detect whether Docker is snap-based or CE (apt) ─────────────────────────
  # Guard the probe: on hosts without snapd the bare `snap` invocation exits
  # 127, which `set -euo pipefail` turns into a hard abort of the whole run.
  local daemon_json snap_rev snap_config_dir
  snap_rev=""
  if command -v snap &>/dev/null; then
    snap_rev=$(snap list docker 2>/dev/null | awk 'NR>1{print $3}' | head -1 || true)
  fi
  if [[ -n "$snap_rev" ]]; then
    # Snap docker: config lives inside the snap revision directory
    snap_config_dir="/var/snap/docker/${snap_rev}/config"
    daemon_json="${snap_config_dir}/daemon.json"
    mkdir -p "$snap_config_dir"
    warn "Snap docker detected (rev ${snap_rev}) — writing runtime to ${daemon_json}"
    warn "Note: snap strict confinement may prevent runsc execution."
    warn "For full gVisor support, install Docker CE via apt instead of snap."
  else
    # Docker CE (apt) — standard path
    daemon_json="/etc/docker/daemon.json"
    mkdir -p /etc/docker
  fi

  # ── Merge runsc runtime into the correct daemon.json ────────────────────────
  # --network=host:    sandbox uses the container netns' real stack rather than
  #                    gVisor's internal netstack. This is required so creatures
  #                    can (a) resolve DNS (gVisor's netstack cannot reach
  #                    Docker's embedded resolver at 127.0.0.11 on user-defined
  #                    networks) and (b) reach both the node and the public
  #                    internet (e.g. the LLM backbone) through the host.
  # --platform=ptrace: works without /dev/kvm.
  # --ignore-cgroups:  required in nested / cgroup-less environments (containers,
  #                    CI, the web runner) where /sys/fs/cgroup/<controller> is
  #                    absent — otherwise runsc fails with "cannot set up cgroup".
  python3 - "$daemon_json" <<'PYEOF'
import json, os, sys, tempfile
path = sys.argv[1]
try:
    cfg = json.loads(open(path).read().strip() or '{}')
except FileNotFoundError:
    cfg = {}
cfg.setdefault('runtimes', {})['runsc'] = {
    'path': 'runsc',
    'runtimeArgs': ['--network=host', '--platform=ptrace', '--ignore-cgroups'],
}
cfg_dir = os.path.dirname(path)
fd, tmp = tempfile.mkstemp(dir=cfg_dir, prefix='.daemon.')
with os.fdopen(fd, 'w') as f:
    json.dump(cfg, f, indent=2); f.write('\n')
os.replace(tmp, path)
print(f'  wrote {path}')
PYEOF

  # ── Restart Docker and wait for daemon to come back ─────────────────────────
  if [[ -n "${snap_rev:-}" ]]; then
    # Snap docker restart
    snap restart docker 2>/dev/null \
      && { local i; for i in $(seq 1 20); do docker info >/dev/null 2>&1 && break; sleep 0.5; done; } \
      || warn "snap restart docker failed — gVisor runtime registered but Docker not reloaded"
  elif systemctl is-active --quiet docker 2>/dev/null; then
    systemctl restart docker
    local i
    for i in $(seq 1 20); do docker info >/dev/null 2>&1 && break; sleep 0.5; done
  fi

  ok "gVisor (runsc) installed and registered with Docker (${daemon_json})"
}

# ─── WSL2 DNS fix (run as root) ──────────────────────────────────────────────
# WSL2's auto-generated /etc/resolv.conf uses 10.255.255.254 as a virtual DNS
# relay. On some corporate / VPN networks this relay fails to resolve external
# hostnames. This function permanently switches to 8.8.8.8 / 1.1.1.1 by
# disabling WSL's auto-generation and writing a static resolv.conf.
_fix_wsl_dns() {
  set -e
  # Only run inside WSL
  grep -qi 'microsoft\|wsl' /proc/version 2>/dev/null || return 0

  local current_ns
  current_ns=$(awk '/^nameserver/{print $2; exit}' /etc/resolv.conf 2>/dev/null)
  # If already using a non-relay DNS, skip
  [[ "$current_ns" != "10.255.255.254" ]] && [[ "$current_ns" != "172.16."* ]] \
    && [[ -n "${current_ns:-}" ]] && { ok "WSL2 DNS already set to $current_ns — skipping fix"; return 0; }

  # Disable WSL auto-generation of resolv.conf
  local wsl_conf="/etc/wsl.conf"
  if ! grep -q 'generateResolvConf' "$wsl_conf" 2>/dev/null; then
    {
      grep -v 'generateResolvConf' "$wsl_conf" 2>/dev/null || true
      printf '\n[network]\ngenerateResolvConf = false\n'
    } > /tmp/_wsl_conf_new
    mv /tmp/_wsl_conf_new "$wsl_conf"
  else
    sed -i 's/generateResolvConf *= *true/generateResolvConf = false/' "$wsl_conf"
  fi

  # Unlink if it's a symlink (WSL manages it as one)
  [[ -L /etc/resolv.conf ]] && rm /etc/resolv.conf

  # Write static resolv.conf
  printf '# Static DNS set by run-nodes.sh (overrides broken WSL relay)\nnameserver 8.8.8.8\nnameserver 1.1.1.1\n' \
    > /etc/resolv.conf

  ok "WSL2 DNS fixed: /etc/resolv.conf → 8.8.8.8 / 1.1.1.1"
}

# ─── Docker CE auto-installation ─────────────────────────────────────────────
# Called (as root) when USE_DOCKER=true but docker is not in PATH.
# Supports Debian/Ubuntu (apt) and RHEL/Fedora/Amazon (dnf/yum).
# Also handles WSL2 environments where systemd may not be running.
_install_docker() {
  set -e
  info "Docker not found — installing Docker CE…"

  # ── Fix WSL2 DNS first so apt-get can reach download.docker.com ─────────────
  _fix_wsl_dns

  # ── Detect package manager ──────────────────────────────────────────────────
  if command -v apt-get &>/dev/null; then
    # Debian / Ubuntu / Raspbian
    apt-get update -qq
    apt-get install -y -qq \
      apt-transport-https ca-certificates curl gnupg lsb-release

    # Load /etc/os-release for $ID
    . /etc/os-release
    local keyring="/usr/share/keyrings/docker-archive-keyring.gpg"
    [[ -f "$keyring" ]] \
      || curl -fsSL "https://download.docker.com/linux/${ID}/gpg" \
           | gpg --dearmor --yes -o "$keyring"

    local arch; arch=$(dpkg --print-architecture)
    local codename; codename=$(lsb_release -cs)
    echo "deb [arch=${arch} signed-by=${keyring}] \
https://download.docker.com/linux/${ID} ${codename} stable" \
      > /etc/apt/sources.list.d/docker.list

    apt-get update -qq
    apt-get install -y -qq \
      docker-ce docker-ce-cli containerd.io \
      docker-buildx-plugin docker-compose-plugin

  elif command -v dnf &>/dev/null || command -v yum &>/dev/null; then
    # RHEL / Fedora / Amazon Linux
    local pkg_mgr; command -v dnf &>/dev/null && pkg_mgr=dnf || pkg_mgr=yum
    $pkg_mgr install -y -q yum-utils
    yum-config-manager --add-repo \
      https://download.docker.com/linux/centos/docker-ce.repo
    $pkg_mgr install -y -q \
      docker-ce docker-ce-cli containerd.io \
      docker-buildx-plugin docker-compose-plugin
  else
    die "Unsupported package manager — install Docker manually: https://docs.docker.com/engine/install/"
  fi

  # ── WSL2: switch to iptables-legacy so Docker CE daemon can start ───────────
  # Ubuntu 22.04/24.04 defaults to nftables; dockerd requires iptables-legacy
  # to configure bridge NAT rules inside WSL2 where nftables isn't available.
  if _is_wsl && command -v update-alternatives &>/dev/null; then
    update-alternatives --set iptables  /usr/sbin/iptables-legacy  >/dev/null 2>&1 || true
    update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy >/dev/null 2>&1 || true
    ok "Configured iptables-legacy for Docker CE on WSL2"
  fi

  # ── Start the daemon ────────────────────────────────────────────────────────
  # systemd path (bare-metal, LXC, WSL2 with systemd enabled)
  if systemctl is-system-running 2>/dev/null | grep -qE 'running|degraded|starting'; then
    systemctl enable docker --quiet 2>/dev/null || true
    systemctl start  docker         2>/dev/null || true
    local i; for i in $(seq 1 30); do docker info >/dev/null 2>&1 && break; sleep 1; done
  else
    # No systemd (plain WSL2, containers, CI) — launch dockerd directly
    if ! docker info >/dev/null 2>&1; then
      nohup dockerd --host=unix:///var/run/docker.sock \
            > /tmp/dockerd.log 2>&1 &
      local i; for i in $(seq 1 40); do docker info >/dev/null 2>&1 && break; sleep 1; done
    fi
  fi

  # ── Allow the invoking (non-root) user to run docker ────────────────────────
  if [[ -n "${SUDO_USER:-}" ]]; then
    usermod -aG docker "$SUDO_USER" 2>/dev/null || true
  fi

  docker info >/dev/null 2>&1 || die "Docker daemon still not reachable after installation"
  ok "Docker CE installed and running: $(docker --version)"
}

# ─── Snap → Docker CE migration ──────────────────────────────────────────────
# Removes snap docker and installs Docker CE (apt) in its place.
# Required for full gVisor (runsc) support — snap's strict confinement blocks
# external runtime binaries and uses a non-standard daemon.json location.
_migrate_snap_to_docker_ce() {
  set -e
  info "Removing snap docker and installing Docker CE (apt)…"

  # Stop and remove snap docker
  snap stop docker 2>/dev/null || true
  snap remove docker 2>/dev/null || true
  # Remove leftover snap socket/pid files
  rm -f /run/snap.docker/docker.sock /run/snap.docker/docker.pid 2>/dev/null || true

  # Install Docker CE via the official apt repo (reuses _install_docker logic)
  _install_docker

  ok "Migrated from snap docker to Docker CE"
}

# ─── Firecracker setup ────────────────────────────────────────────────────────
FC_VERSION="v1.10.1"
FC_ARCH=$(uname -m)   # x86_64 or aarch64

_install_firecracker() {
  set -e
  local fc_version="${FC_VERSION:-v1.10.1}"
  local fc_arch="${FC_ARCH:-$(uname -m)}"

  info "Installing Firecracker ${fc_version} (${fc_arch})…"
  _pkg_update
  case "$(_pkg_mgr)" in
    apt)    _pkg_install curl libelf-dev e2fsprogs ;;
    dnf)    _pkg_install curl elfutils-libelf-devel e2fsprogs ;;
    yum)    _pkg_install curl elfutils-libelf-devel e2fsprogs ;;
    pacman) _pkg_install curl libelf e2fsprogs ;;
    *)      warn "Cannot auto-install Firecracker deps — ensure curl + e2fsprogs are present" ;;
  esac

  mkdir -p /opt/firecracker/{vms,kernel,rootfs,snapshots}

  # Binary
  if ! command -v firecracker &>/dev/null; then
    local tgz="firecracker-${fc_version}-${fc_arch}.tgz"
    curl -fsSL \
      "https://github.com/firecracker-microvm/firecracker/releases/download/${fc_version}/${tgz}" \
      -o "/tmp/${tgz}"
    tar -xzf "/tmp/${tgz}" -C /tmp
    mv "/tmp/release-${fc_version}-${fc_arch}/firecracker-${fc_version}-${fc_arch}" \
       /usr/local/bin/firecracker
    chmod +x /usr/local/bin/firecracker
    rm -rf "/tmp/${tgz}" "/tmp/release-${fc_version}-${fc_arch}"
    ok "Installed: $(firecracker --version 2>&1 | head -1)"
  else
    ok "Firecracker binary already present: $(firecracker --version 2>&1 | head -1)"
  fi

  # Guest kernel
  if [[ ! -f /opt/firecracker/kernel/vmlinux ]]; then
    info "Downloading guest kernel for ${fc_arch}…"
    curl -fsSL \
      "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/${fc_arch}/kernels/vmlinux.bin" \
      -o /opt/firecracker/kernel/vmlinux
    chmod +x /opt/firecracker/kernel/vmlinux
    ok "Guest kernel ready ($(ls -lh /opt/firecracker/kernel/vmlinux | awk '{print $5}'))"
  else
    ok "Guest kernel already present"
  fi

  # Guest rootfs (Alpine-based ext4 image)
  # Requires loop device support — may not be available in WSL2 without a custom kernel.
  if [[ ! -f /opt/firecracker/rootfs/rootfs.ext4 ]]; then
    # WSL2 guard: check for usable loop devices before attempting mount
    if _is_wsl && ! ls /dev/loop* &>/dev/null 2>&1; then
      warn "WSL detected: no /dev/loop* devices found — skipping rootfs build."
      warn "To enable loop devices in WSL2, add to /etc/wsl.conf:"
      warn "  [boot]"
      warn "  command = modprobe loop"
      warn "Firecracker binary + kernel are installed; rootfs must be provided manually."
    else
      info "Building guest rootfs (Alpine 3.20 / ${fc_arch})…"
      _pkg_install e2fsprogs 2>/dev/null || true
      local alpine_url="https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/${fc_arch}/alpine-minirootfs-3.20.0-${fc_arch}.tar.gz"
      curl -fsSL "$alpine_url" -o /tmp/alpine-minirootfs.tar.gz
      dd if=/dev/zero of=/opt/firecracker/rootfs/rootfs.ext4 bs=1M count=128 status=none
      mkfs.ext4 -q /opt/firecracker/rootfs/rootfs.ext4
      local mnt; mnt=$(mktemp -d)
      mount -o loop /opt/firecracker/rootfs/rootfs.ext4 "$mnt"
      tar -xzf /tmp/alpine-minirootfs.tar.gz -C "$mnt"
      printf '#!/bin/sh\nmount -t proc proc /proc\nmount -t sysfs sysfs /sys\nmount -t devtmpfs devtmpfs /dev 2>/dev/null||true\nexec /bin/sh\n' \
        > "$mnt/sbin/init"
      chmod +x "$mnt/sbin/init"
      umount "$mnt"; rmdir "$mnt"
      rm -f /tmp/alpine-minirootfs.tar.gz
      ok "Guest rootfs ready ($(ls -lh /opt/firecracker/rootfs/rootfs.ext4 | awk '{print $5}'))"
    fi
  else
    ok "Guest rootfs already present"
  fi

  [[ -e /dev/kvm ]] \
    && ok "/dev/kvm available — hardware-accelerated microVMs enabled" \
    || warn "/dev/kvm not available — Firecracker will use ptrace platform (slower cold-start)"
}

_setup_firecracker_network() {
  set -e
  local bridge="br0"
  local bridge_cidr="172.16.0.1/24"
  local host_iface
  host_iface=$(ip route show default 2>/dev/null | awk '/default/{print $5; exit}')

  # WSL2: restricted networking — bridge and iptables may not fully work.
  # We attempt best-effort but never hard-fail.
  if _is_wsl; then
    warn "WSL detected: bridge networking / iptables rules may be restricted."
    warn "Firecracker microVM networking may not work inside WSL2 without a custom kernel."
  fi

  for tool in ip iptables sysctl; do
    command -v "$tool" &>/dev/null || { warn "$tool not found; skipping Firecracker network setup"; return 1; }
  done

  if ! ip link show "$bridge" &>/dev/null; then
    ip link add name "$bridge" type bridge 2>/dev/null \
      || { warn "Could not create bridge $bridge (WSL restriction?) — skipping"; return 0; }
    ip addr add "$bridge_cidr" dev "$bridge" 2>/dev/null || true
    ip link set "$bridge" up 2>/dev/null || true
  fi

  if [[ -n "$host_iface" ]]; then
    iptables -t nat -C POSTROUTING -o "$host_iface" -j MASQUERADE 2>/dev/null \
      || iptables -t nat -A POSTROUTING -o "$host_iface" -j MASQUERADE 2>/dev/null || true
    iptables -C FORWARD -i "$bridge" -o "$host_iface" -j ACCEPT 2>/dev/null \
      || iptables -A FORWARD -i "$bridge" -o "$host_iface" -j ACCEPT 2>/dev/null || true
    iptables -C FORWARD -i "$host_iface" -o "$bridge" \
        -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null \
      || iptables -A FORWARD -i "$host_iface" -o "$bridge" \
           -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true
  fi

  # ip_forward: read-only in WSL2 — attempt but don't fail
  if [[ "$(cat /proc/sys/net/ipv4/ip_forward 2>/dev/null)" != "1" ]]; then
    sysctl -qw net.ipv4.ip_forward=1 2>/dev/null \
      || warn "Could not enable ip_forward (read-only in WSL?) — microVM routing may not work"
  fi
}

# ─── Docker daemon DNS auto-fix (Docker CE only) ─────────────────────────────
# When Docker CE's daemon has DNS issues (resolv.conf / daemon.json not set up),
# this writes the daemon DNS config and reloads dockerd.
# For Docker Desktop, fix your host's DNS instead (see README).

_fix_docker_ce_dns() {
  local daemon_json="/etc/docker/daemon.json"
  mkdir -p /etc/docker
  python3 - "$daemon_json" <<'PYEOF'
import json, sys, os, tempfile
path = sys.argv[1]
try:
    cfg = json.loads(open(path).read().strip() or '{}')
except (FileNotFoundError, json.JSONDecodeError):
    cfg = {}
cfg['dns'] = ['8.8.8.8', '1.1.1.1']
d = os.path.dirname(path) or '.'
fd, tmp = tempfile.mkstemp(dir=d, prefix='.daemon.')
with os.fdopen(fd, 'w') as f:
    json.dump(cfg, f, indent=2); f.write('\n')
os.replace(tmp, path)
print(f'Updated {path}')
PYEOF
  if systemctl is-active --quiet docker 2>/dev/null; then
    systemctl reload docker 2>/dev/null || systemctl restart docker 2>/dev/null || true
  elif [[ -f /var/run/docker.pid ]]; then
    kill -HUP "$(cat /var/run/docker.pid)" 2>/dev/null || true
  fi
  sleep 2
}

# Verify Docker daemon can reach Docker Hub; offer DNS fix hint if not.
_ensure_docker_dns() {
  $USE_DOCKER || return 0

  local probe_out
  probe_out=$(docker pull hello-world:latest 2>&1) && {
    docker rmi hello-world:latest >/dev/null 2>&1 || true
    return 0
  }

  # Only act on clear DNS errors; skip auth / rate-limit errors
  if echo "$probe_out" | grep -qiE 'no such host|lookup .+ on .+:[0-9]+|dial tcp.*i/o timeout'; then
    warn "Docker daemon DNS issue detected."
    warn "Fix: ensure /etc/resolv.conf has a working nameserver (e.g. 8.8.8.8) and retry."
    # Auto-fix for Docker CE (not Docker Desktop — fix host DNS instead)
    if ! _is_wsl || command -v dockerd &>/dev/null; then
      warn "Attempting Docker CE daemon DNS fix…"
      _as_root _fix_docker_ce_dns
    fi
  else
    # Non-DNS error (e.g. TLS, auth) — show once and continue
    info "Docker registry test: $(echo "$probe_out" | tail -1)"
  fi
}

# ─── TinyGo + Go 1.23 installation (runs as root) ────────────────────────────
# TinyGo is the WASM compiler for Caspar creature modules.
# TinyGo ≤ 0.34 requires Go ≤ 1.23, so we install Go 1.23 to /usr/local/go123
# and symlink tinygo to /usr/local/bin so it is in every user's PATH.
TINYGO_VERSION="0.34.0"

_install_tinygo() {
  set -e
  # _as_root re-invokes this function under `sudo bash` after dumping only
  # function definitions, so top-level scalars like TINYGO_VERSION are NOT
  # carried into that subshell. Default at function entry to keep the
  # download URL correct in both direct and via-sudo call paths.
  local tinygo_version="${TINYGO_VERSION:-0.34.0}"

  command -v tinygo &>/dev/null && {
    ok "TinyGo already installed: $(tinygo version 2>&1 | head -1)"
    return 0
  }

  info "Installing TinyGo ${tinygo_version}…"
  local hw_arch; hw_arch=$(uname -m)
  local tg_arch
  case "$hw_arch" in
    x86_64)        tg_arch="amd64" ;;
    aarch64|arm64) tg_arch="arm64" ;;
    *) warn "TinyGo: unsupported arch ${hw_arch} — skipping"; return 0 ;;
  esac

  _pkg_update; _pkg_install curl tar ca-certificates

  # ── Go 1.23 (required by TinyGo ≤ 0.34) ─────────────────────────────────
  if [[ ! -x "/usr/local/go123/bin/go" ]]; then
    info "Installing Go 1.23 (required by TinyGo ≤ 0.34)…"
    local go_tgz="go1.23.9.linux-${tg_arch}.tar.gz"
    curl -fsSL "https://go.dev/dl/${go_tgz}" -o "/tmp/${go_tgz}"
    tar -xzf "/tmp/${go_tgz}" -C /tmp
    mv /tmp/go /usr/local/go123
    rm -f "/tmp/${go_tgz}"
    ok "Go 1.23 installed at /usr/local/go123"
  else
    ok "Go 1.23 already present: $(/usr/local/go123/bin/go version)"
  fi

  # ── TinyGo binary ─────────────────────────────────────────────────────────
  local tg_tgz="tinygo${tinygo_version}.linux-${tg_arch}.tar.gz"
  curl -fsSL \
    "https://github.com/tinygo-org/tinygo/releases/download/v${tinygo_version}/${tg_tgz}" \
    -o "/tmp/${tg_tgz}"
  tar -xzf "/tmp/${tg_tgz}" -C /usr/local
  rm -f "/tmp/${tg_tgz}"
  ln -sf /usr/local/tinygo/bin/tinygo /usr/local/bin/tinygo

  ok "TinyGo installed: $(GOROOT=/usr/local/go123 /usr/local/bin/tinygo version 2>&1 | head -1)"
}

# ─── Python creature-deploy dependencies (runs as root) ──────────────────────
_install_python_deps() {
  python3 -c "from Crypto.PublicKey import RSA" 2>/dev/null && return 0
  info "Installing pycryptodome…"
  pip3 install pycryptodome --quiet \
    || warn "pycryptodome install failed — creature deployment may not work"
}

# ─── Clone / verify decillionai-server repo ───────────────────────────────────
# Sets global DECILLIONAI_SERVER_DIR.  Returns 1 on failure.
DECILLIONAI_SERVER_DIR=""
_ensure_decillionai_server() {
  local server_dir
  # Honour an explicit override (the benchmark workflow sets this to the
  # checkout it already performed, which avoids a redundant clone and
  # guarantees both run-nodes.sh and bench-all.sh use the same commit).
  if [[ -n "${DECILLIONAI_SERVER:-}" ]] && [[ -d "${DECILLIONAI_SERVER}/.git" ]]; then
    server_dir="$DECILLIONAI_SERVER"
    ok "decillionai-server present (DECILLIONAI_SERVER): $server_dir"
    DECILLIONAI_SERVER_DIR="$server_dir"
    return 0
  fi
  server_dir="$(dirname "$REPO_DIR")/decillionai-server"
  if [[ ! -d "$server_dir/.git" ]]; then
    info "Cloning decillionai-server…"
    git clone --depth=1 \
      https://github.com/DecillionAI/decillionai-server.git \
      "$server_dir" 2>&1 \
      || { warn "git clone failed — skipping creature deployment"; return 1; }
    ok "Cloned decillionai-server → $server_dir"
  else
    ok "decillionai-server present: $server_dir"
  fi
  DECILLIONAI_SERVER_DIR="$server_dir"
}

# ─── Build WASM creatures via decillionai-server/build-all.sh ────────────────
_build_creatures() {
  local server_dir="$1"
  local wasm_dir="$server_dir/wasm"
  local wasm_count; wasm_count=$(find "$wasm_dir" -name "*.wasm" 2>/dev/null | wc -l)
  if [[ $wasm_count -ge 6 ]]; then
    ok "WASM creatures already built (${wasm_count} .wasm files)"
    return 0
  fi
  [[ -f "$server_dir/build-all.sh" ]] \
    || { warn "build-all.sh not found in $server_dir — skipping build"; return 1; }
  info "Building WASM creatures via build-all.sh (~2-3 min)…"
  export PATH="/usr/local/go123/bin:/usr/local/tinygo/bin:${PATH}"
  export GOROOT="/usr/local/go123"
  bash "$server_dir/build-all.sh" \
    || { warn "build-all.sh failed — creature deployment may be incomplete"; return 1; }
  ok "WASM creatures built: $(find "$wasm_dir" -name '*.wasm' 2>/dev/null | wc -l) files"
}

# ─── Application-level readiness probe ───────────────────────────────────────
# TCP-open is necessary but not sufficient; wait until the node actually
# responds to a /auths/getServerPublicKey request before deploying.
#
# We require N *consecutive* successful round-trips so a flake in the first
# few seconds (e.g. the listener thread accepted but the action registry is
# still being populated) does not let deploy start prematurely. The 2026-05-30
# run failed because the probe passed but the node was not yet handling
# authenticated requests — node2/3 stalled at first login.
_probe_node_app_ready() {
  # $1 = TCP port.  Returns 0 when the node responds, 1 on timeout/error.
  local port="$1"
  python3 -c "
import sys, socket, struct, uuid
port = int('$port')
def lp(x): b=x.encode(); return struct.pack('>I',len(b))+b
pkt = str(uuid.uuid4())
body = lp('') + lp('') + lp('/auths/getServerPublicKey') + lp(pkt) + b'{}'
frame = struct.pack('>I',len(body)) + body
s = socket.socket(); s.settimeout(5)
try:
    s.connect(('127.0.0.1', port))
    s.sendall(frame)
    hdr = b''
    while len(hdr) < 4:
        c = s.recv(4 - len(hdr))
        if not c: sys.exit(1)
        hdr += c
    length = struct.unpack('>I', hdr)[0]
    data = b''
    while len(data) < length:
        c = s.recv(length - len(data))
        if not c: sys.exit(1)
        data += c
    sys.exit(0 if length > 0 else 1)
except Exception:
    sys.exit(1)
finally:
    try: s.close()
    except: pass
" 2>/dev/null
}

# Require the node to handle several requests back-to-back without a stall.
# The "API stops responding after a few requests" failure mode would let the
# first probe pass and the second time out, so we keep probing until we get
# N consecutive successes.
_probe_node_steady() {
  local port="$1"
  local need="${2:-3}"
  local got=0
  while [[ $got -lt $need ]]; do
    if _probe_node_app_ready "$port"; then
      got=$((got + 1))
    else
      return 1
    fi
    sleep 0.2
  done
  return 0
}

_wait_nodes_app_ready() {
  local max_wait=180
  info "Waiting for node(s) to be application-ready (up to ${max_wait}s)…"
  for n in "${NODES[@]}"; do
    local port=${NODE_TCP[$n]}
    local elapsed=0
    while ! _probe_node_steady "$port" 3; do
      sleep 2; elapsed=$((elapsed + 2))
      if [[ $elapsed -ge $max_wait ]]; then
        warn "node$n did not become application-ready (3-probe steady) after ${max_wait}s — deploy may fail"
        break
      fi
    done
    _probe_node_steady "$port" 3 \
      && ok "node$n application-ready (3 probes steady)" \
      || warn "node$n not steadily responding to probes — continuing anyway"
  done
}

# ─── Deploy WASM creatures to the running Caspar nodes ───────────────────────
_deploy_creatures() {
  local server_dir="$1"
  local deploy_script="$server_dir/bench/deploy.py"
  [[ -f "$deploy_script" ]] \
    || { warn "deploy.py not found at $deploy_script"; return 1; }
  python3 -c "from Crypto.PublicKey import RSA" 2>/dev/null \
    || { warn "pycryptodome not available — skipping deployment"; return 1; }
  info "Deploying WASM creatures to node(s)…"
  mkdir -p "$DATA_ROOT"
  # PIPESTATUS[0] reflects deploy.py's real exit; the tee + grep chain otherwise
  # masks failures and the workflow reports green on a broken deploy.
  python3 "$deploy_script" 2>&1 | tee "$DATA_ROOT/deploy.log" | \
    grep --line-buffered -iE '(deploy|\.wasm|creature|module|install|register|upload)' | \
    while IFS= read -r line; do info "  $line"; done
  local deploy_rc=${PIPESTATUS[0]}
  local report="${HOME:-/root}/deployment_report.json"
  if [[ $deploy_rc -ne 0 ]]; then
    warn "deploy.py exited with code $deploy_rc — see $DATA_ROOT/deploy.log"
    return $deploy_rc
  fi
  if [[ ! -f "$report" ]]; then
    warn "deploy.py finished but deployment_report.json not found"
    return 1
  fi
  # Validate the report: every node must have at least one successful module,
  # and the overall error count must be zero. Anything less is a silent
  # half-deploy (e.g. node1 OK + node2/3 timed out) that bench-all.sh would
  # later run against, producing a meaningless "no nodes reachable" abort.
  local stats
  stats=$(python3 -c "
import json, sys
try:
    reps = json.load(open('$report'))
except Exception as e:
    print('parse_error', e); sys.exit(2)
if not isinstance(reps, list) or not reps:
    print('empty_report'); sys.exit(2)
ok = sum(r.get('ok_count', 0) for r in reps if isinstance(r, dict))
err = sum(r.get('error_count', 0) for r in reps if isinstance(r, dict))
nodes_ok = sum(1 for r in reps if isinstance(r, dict) and r.get('ok_count', 0) > 0)
nodes_total = len(reps)
print(f'{ok} {err} {nodes_ok} {nodes_total}')
" 2>&1) || { warn "deployment_report.json malformed: $stats"; return 1; }
  read -r ok_count err_count nodes_ok nodes_total <<<"$stats"
  if [[ "${err_count:-0}" -gt 0 || "${nodes_ok:-0}" -ne "${nodes_total:-0}" ]]; then
    warn "Creature deployment partial: ok=${ok_count} err=${err_count} nodes_ok=${nodes_ok}/${nodes_total}"
    return 1
  fi
  ok "Creature deployment complete — ${ok_count} WASM modules deployed across ${nodes_ok}/${nodes_total} nodes"
}

# ─── Dependency checks ───────────────────────────────────────────────────────
check_dep() {
  local name="$1" cmd="$2" hint="$3"
  if ! command -v "$cmd" &>/dev/null; then
    die "Missing dependency: $name\n  Install: $hint"
  fi
}

info "Checking dependencies…"

if $USE_DOCKER; then
  # ── Snap docker → Docker CE migration ────────────────────────────────────────
  # Docker CE (apt) is required for full gVisor support.
  # If snap docker is the only docker present, replace it with Docker CE.
  _docker_bin=$(command -v docker 2>/dev/null || true)
  # _docker_ce_installed: distro-agnostic check for Docker CE (apt/rpm)
  _docker_ce_installed() {
    command -v dpkg &>/dev/null && dpkg -l docker-ce &>/dev/null 2>&1 && return 0
    command -v rpm  &>/dev/null && rpm -q docker-ce  &>/dev/null 2>&1 && return 0
    return 1
  }
  if snap list docker &>/dev/null 2>&1 && ! _docker_ce_installed; then
    warn "Snap docker detected — migrating to Docker CE (apt) for gVisor compatibility…"
    _as_root _migrate_snap_to_docker_ce
    export PATH="/usr/bin:/usr/local/bin:$PATH"
    unset _docker_bin
  fi
  unset _docker_bin

  # ── Auto-install Docker CE if no NATIVE docker exists ────────────────────
  # _native_docker_exists rejects Windows .exe wrappers accessible via WSL PATH
  if ! _native_docker_exists; then
    warn "Native docker not found — auto-installing Docker CE…"
    _as_root _install_docker
    export PATH="/usr/bin:/usr/local/bin:$PATH"
  fi
  check_dep "docker" "docker" "https://docs.docker.com/engine/install/"

  # ── Fix socket permissions (Docker CE fresh install: user not yet in group) ──
  if ! docker info >/dev/null 2>&1 && sudo docker info >/dev/null 2>&1; then
    warn "Docker socket not accessible to $(whoami) — fixing group membership…"
    getent group docker &>/dev/null || sudo groupadd docker 2>/dev/null || true
    sudo chown root:docker /var/run/docker.sock 2>/dev/null || true
    sudo chmod 660 /var/run/docker.sock 2>/dev/null || true
    sudo usermod -aG docker "$(whoami)" 2>/dev/null || true
    # Re-exec this script under the docker group (avoids needing a new login session)
    exec sg docker -c "bash $(printf '%q' "$0") $(printf '%q ' "$@")" \
      || die "Could not re-exec with docker group. Log out and back in, then re-run."
  fi

  # Ensure the daemon is reachable; start dockerd if needed (handles WSL2 no-systemd)
  if ! docker info >/dev/null 2>&1; then
    warn "Docker daemon not reachable — attempting to start it…"
    if command -v systemctl &>/dev/null && systemctl is-system-running 2>/dev/null | grep -qE 'running|degraded'; then
      sudo systemctl start docker 2>/dev/null || true
    else
      sudo nohup dockerd --host=unix:///var/run/docker.sock \
           > /tmp/dockerd.log 2>&1 &
    fi
    _di=0; while [[ $_di -lt 30 ]]; do docker info >/dev/null 2>&1 && break; sleep 1; _di=$((_di+1)); done
    docker info >/dev/null 2>&1 \
      || die "Docker daemon is not reachable. Check /tmp/dockerd.log for details."
  fi

  # ── Auto-fix Docker daemon DNS if Docker Hub is unreachable ──────────────
  # Detects 10.255.255.254 relay failures (common on corporate/VPN networks)
  # and reconfigures the daemon to use 8.8.8.8 / 1.1.1.1 with an auto-restart.
  _ensure_docker_dns
else
  # Skip cargo check when --no-rebuild is set and the binary already exists.
  if ! $NO_REBUILD || [[ ! -f "$BINARY" ]]; then
    check_dep "Rust/cargo" "cargo" "curl https://sh.rustup.rs -sSf | sh"
  fi
fi

# ─── gVisor (runsc) — default ON ──────────────────────────────────────────────
if ! $SETUP_GVISOR; then
  info "Skipping gVisor setup (--no-gvisor)"
elif ! command -v docker &>/dev/null; then
  warn "Docker not installed — skipping gVisor setup"
elif command -v runsc &>/dev/null && docker info 2>/dev/null | grep -q runsc; then
  ok "gVisor (runsc) already installed and registered with Docker"
else
  _as_root _setup_gvisor
  docker info 2>/dev/null | grep -q runsc \
    && ok "gVisor registered with Docker" \
    || warn "gVisor setup completed but runsc not visible in docker info"
fi

# ─── VM gateway network (kasper) ──────────────────────────────────────────────
# Every docker/firecracker creature the node spawns is attached to the
# user-defined ``kasper`` bridge network (see VmNetworkService::gateway_network_name
# in apps/aseman-node/src/drivers/vmm/network). The node does not create it, so we ensure it
# exists here — otherwise container creation fails with "network kasper not found".
#
# We pin an explicit subnet/gateway so the bridge gateway IP is deterministic
# (172.18.0.1). That address matters for more than NAT: the docker-host bridge
# gateway authenticates each creature purely from its connection's *source IP*
# (apps/aseman-node/src/drivers/vmm/.../server.rs → find_container_name_by_ip, which matches
# the container's kasper endpoint IP). A creature must therefore reach the
# gateway over the SAME kasper bridge it is attached to — if it instead dials
# `host.docker.internal` (Docker's `host-gateway`, the *default* docker0 bridge
# 172.17.0.1), its source IP is masqueraded across bridges and no longer matches
# any kasper endpoint, so the HELLO handshake is rejected with "could not
# identify a docker creature for source ip ..." and every tool/agent silently
# fails to serve. See `DOCKER_HOST_GATEWAY_ADVERTISE_HOST` export below.
if command -v docker &>/dev/null && docker info >/dev/null 2>&1; then
  if docker network inspect kasper >/dev/null 2>&1; then
    ok "Docker network 'kasper' already present"
  elif docker network create --subnet 172.18.0.0/16 --gateway 172.18.0.1 kasper >/dev/null 2>&1; then
    ok "Created docker network 'kasper' (172.18.0.0/16) for VM creatures"
  elif docker network create kasper >/dev/null 2>&1; then
    ok "Created docker network 'kasper' for VM creatures (auto subnet)"
  else
    warn "Could not create docker network 'kasper' — docker creatures may fail to start"
  fi

  # Advertise the docker-host bridge gateway to creatures on the kasper bridge
  # itself (its gateway IP, where the node listens on 0.0.0.0:8079), NOT on
  # `host.docker.internal`. This keeps each creature's source IP equal to its
  # kasper endpoint so the gateway can identify it (see comment above). Respect
  # an explicit override if the operator already set one.
  if [[ -z "${DOCKER_HOST_GATEWAY_ADVERTISE_HOST:-}" ]]; then
    KASPER_GW="$(docker network inspect kasper \
      -f '{{ range .IPAM.Config }}{{ .Gateway }}{{ end }}' 2>/dev/null | head -n1)"
    export DOCKER_HOST_GATEWAY_ADVERTISE_HOST="${KASPER_GW:-172.18.0.1}"
    ok "Gateway advertise host for creatures: $DOCKER_HOST_GATEWAY_ADVERTISE_HOST (kasper bridge)"
  fi
fi

# ─── Firecracker — default ON ─────────────────────────────────────────────────
if ! $SETUP_FIRECRACKER; then
  info "Skipping Firecracker setup (--no-firecracker)"
else
  if command -v firecracker &>/dev/null && [[ -f /opt/firecracker/kernel/vmlinux ]]; then
    ok "Firecracker already installed: $(firecracker --version 2>&1 | head -1)"
  else
    _as_root _install_firecracker
  fi

  info "Configuring Firecracker host network (bridge + NAT)…"
  _as_root _setup_firecracker_network \
    && ok "Firecracker network ready (br0 172.16.0.1/24)" \
    || warn "Firecracker network setup failed — microVM networking may not work"
fi

ok "All dependency checks passed"

# ─── Fresh start ─────────────────────────────────────────────────────────────
if $FRESH; then
  warn "--fresh: wiping $DATA_ROOT and stale deployment/bench reports"
  rm -rf "$DATA_ROOT"
  # Remove stale reports so bench-all.sh doesn't think deployment is done
  rm -f "${HOME:-/root}/deployment_report.json" \
        "${HOME:-/root}/workflow_results.json"  \
        "${HOME:-/root}/workflow_report.md"
fi

mkdir -p "$DATA_ROOT"

# ─── Stop existing processes / containers ───────────────────────────────────
stop_existing() {
  local pids
  pids=$(ps -eo pid,cmd 2>/dev/null | awk '/caspar-node/ && !/awk/ && !/grep/ && !/run-nodes/ {print $1}')
  if [[ -n "$pids" ]]; then
    info "Stopping existing caspar-node processes: $pids"
    for p in $pids; do kill "$p" 2>/dev/null || true; done
    sleep 2
    for p in $pids; do kill -9 "$p" 2>/dev/null || true; done
  fi

  if command -v docker &>/dev/null; then
    for n in 1 2 3; do
      if docker ps -a --format '{{.Names}}' 2>/dev/null | grep -q "^caspar-node${n}$"; then
        info "Removing existing container: caspar-node${n}"
        docker rm -f "caspar-node${n}" >/dev/null 2>&1 || true
      fi
    done
  fi

  local jpids
  # Match 'questdb' as a standalone word (not in flags like --no-questdb or --skip-questdb).
  jpids=$(ps -eo pid,cmd 2>/dev/null | awk '/[^-]questdb/ && !/awk/ && !/grep/ && !/run-nodes/ {print $1}')
  if [[ -n "$jpids" ]]; then
    info "Stopping existing QuestDB processes: $jpids"
    for p in $jpids; do kill "$p" 2>/dev/null || true; done
    sleep 1
  fi
}
stop_existing

# ─── Helper: wait for a TCP port ────────────────────────────────────────────
wait_for_port() {
  local host="$1" port="$2" name="$3" timeout="${4:-30}"
  local elapsed=0
  while ! python3 -c "import socket; s=socket.socket(); s.settimeout(1); s.connect(('$host',$port)); s.close()" 2>/dev/null; do
    sleep 1; elapsed=$((elapsed+1))
    [[ $elapsed -ge $timeout ]] && return 1
  done
  return 0
}

# ─── Start QuestDB (mandatory) ───────────────────────────────────────────────
# caspar nodes hardcode a connection to localhost:8812; they cannot start
# without QuestDB running. We start it here, after stop_existing has cleaned
# up stale processes, so a fresh QuestDB always backs each cluster run.
_java_hint() {
  case "$(_pkg_mgr)" in
    apt)    echo "apt-get install -y default-jre" ;;
    dnf|yum) echo "dnf install -y java-11-openjdk" ;;
    pacman) echo "pacman -S jre-openjdk" ;;
    *)      echo "install Java 11+ for your distro" ;;
  esac
}

# In docker mode each container starts its own QuestDB (see
# deploy/legacy/docker-entrypoint.sh) so it has an isolated tsdb on the
# host-network port assigned to that node. The host does not run QuestDB
# at all in docker mode.
#
# In non-docker (local) mode we start one QuestDB per node on the host,
# bound to the per-node ports (8812/8912/9012 for PG, plus matching HTTP
# and ILP ports) with separate data directories. Single-node runs use
# only node1's ports, so the layout is identical between single and triple.
declare -a QUESTDB_PIDS=()
_ensure_questdb_jar() {
  if [[ -f "$QUESTDB_JAR" ]]; then
    return 0
  fi
  command -v curl &>/dev/null \
    || die "QuestDB jar not found: $QUESTDB_JAR — caspar nodes cannot start without it"
  warn "QuestDB jar not found at $QUESTDB_JAR — downloading…"
  mkdir -p "$(dirname "$QUESTDB_JAR")"
  local QDB_VER="8.3.1"
  curl -fsSL \
    "https://github.com/questdb/questdb/releases/download/$QDB_VER/questdb-$QDB_VER-no-jre-bin.tar.gz" \
    -o /tmp/questdb.tar.gz
  tar -xzf /tmp/questdb.tar.gz -C "$(dirname "$QUESTDB_JAR")" --strip-components=1
  rm -f /tmp/questdb.tar.gz
  ok "QuestDB downloaded to $QUESTDB_JAR"
}

if $USE_DOCKER; then
  info "Docker mode: each node container will run its own QuestDB inside it (skipping host QuestDB)"
else
  check_dep "java" "java" "$(_java_hint)"
  _ensure_questdb_jar
  jver=$(java -version 2>&1 | grep -oP '(?<=version ")[0-9]+' | head -1)
  [[ -z "$jver" ]] && jver=$(java -version 2>&1 | grep -oP '"[0-9]+\.' | grep -oP '[0-9]+')
  if [[ "${jver:-0}" -lt 11 ]]; then
    die "Java $jver found but QuestDB needs Java 11+ — caspar nodes cannot start without QuestDB"
  fi
  for n in "${NODES[@]}"; do
    local_pg=$((8812 + (n - 1) * 100))
    local_http=$((9000 + (n - 1) * 100))
    local_http_min=$((9003 + (n - 1) * 100))
    local_ilp=$((9009 + (n - 1) * 100))
    local_data="$DATA_ROOT/node${n}/questdb"
    if python3 -c "import socket; s=socket.socket(); s.settimeout(1); s.connect(('127.0.0.1',$local_pg)); s.close()" 2>/dev/null; then
      ok "QuestDB for node$n already running on port $local_pg"
      continue
    fi
    mkdir -p "$local_data"
    info "Starting QuestDB for node$n (PG=$local_pg, HTTP=$local_http, MIN=$local_http_min, ILP=$local_ilp)…"
    # Override every listener QuestDB opens. The min HTTP server and the
    # UDP line-protocol listener default to 9003 / 9009 — without these
    # overrides a second QuestDB instance on the same host network would
    # crash on bind().
    QDB_PG_NET_BIND_TO="0.0.0.0:${local_pg}" \
    QDB_HTTP_NET_BIND_TO="0.0.0.0:${local_http}" \
    QDB_HTTP_MIN_NET_BIND_TO="0.0.0.0:${local_http_min}" \
    QDB_LINE_TCP_NET_BIND_TO="0.0.0.0:${local_ilp}" \
    QDB_LINE_UDP_ENABLED=false \
    java -jar "$QUESTDB_JAR" -m io.questdb/io.questdb.ServerMain \
         -d "$local_data" >> "$DATA_ROOT/node${n}/questdb.log" 2>&1 &
    QUESTDB_PIDS+=("$!")
    if wait_for_port localhost "$local_pg" "QuestDB(node$n)" 300; then
      ok "QuestDB for node$n ready on port $local_pg (pid $!)"
    else
      die "QuestDB for node$n did not start within 300s — check $DATA_ROOT/node${n}/questdb.log"
    fi
  done
fi

# ─── Build / fetch the artifact we need ──────────────────────────────────────
# Ensure ~/.cargo/bin is in PATH so build-dist.sh can find cargo if needed
[[ -d "$HOME/.cargo/bin" ]] && export PATH="$HOME/.cargo/bin:$PATH"

if $USE_DOCKER; then
  # --no-rebuild scopes purely to skipping the dist/ refresh (build-dist.sh).
  # The docker image is still built every time docker mode is selected so the
  # Dockerfile + dist/ payload that ends up running is always derived from
  # the current checkout, not whatever stale image the daemon may have cached
  # from a previous run. The Dockerfile COPYs from dist/, so the image content
  # naturally follows whichever dist/ the previous step prepared.
  if $NO_REBUILD; then
    info "--no-rebuild: reusing checked-in dist/ (skipping build-dist.sh)"
    [[ -x "$REPO_DIR/dist/bin/caspar-node" ]] \
      || die "--no-rebuild set but $REPO_DIR/dist/bin/caspar-node is missing"
  else
    info "Refreshing dist/ via build-dist.sh…"
    bash "$REPO_DIR/build-dist.sh"
  fi
  info "Building $DOCKER_IMAGE from dist/ …"
  _fc_build_arg="true"; $SETUP_FIRECRACKER || _fc_build_arg="false"
  docker build -f "$REPO_DIR/deploy/legacy/node.Dockerfile" \
    --build-arg "INSTALL_FIRECRACKER=${_fc_build_arg}" \
    -t "$DOCKER_IMAGE" "$REPO_DIR"
  ok "Docker image ready: $DOCKER_IMAGE"
else
  if $NO_REBUILD; then
    info "--no-rebuild: skipping build, using existing dist/ binaries"
    [[ -f "$BINARY" ]] || die "--no-rebuild set but binary not found at $BINARY"
  else
    info "Building caspar-node via build-dist.sh…"
    bash "$REPO_DIR/build-dist.sh" --skip-ctl
    [[ -x "$REPO_DIR/dist/bin/caspar-node" ]] && BINARY="$REPO_DIR/dist/bin/caspar-node"
  fi
  [[ -f "$BINARY" ]] || die "Build failed: binary not found at $BINARY"
  ok "Binary ready: $BINARY ($(ls -lh "$BINARY" | awk '{print $5}'))"
fi

# ─── Per-node config ──────────────────────────────────────────────────────────
# NODE LAYOUT:
#  node1: TCP=8074  WS=8076  FED=8077  CHAIN=8078  ENTITY=8079  VM=8080  TEL=9099
#  node2: TCP=8174  WS=8176  FED=8177  CHAIN=8178  ENTITY=8179  VM=8180  TEL=9199
#  node3: TCP=8274  WS=8276  FED=8277  CHAIN=8278  ENTITY=8279  VM=8280  TEL=9299

# _generate_node_config: create .env + babble key for a node from scratch.
# Safe to call on an existing node — exits immediately if .env already exists.
_generate_node_config() {
  local n="$1"
  local node_dir="$DATA_ROOT/node${n}"
  local env_file="$node_dir/.env"

  [[ -f "$env_file" ]] && return 0   # already configured

  info "Generating fresh config for node${n}…"
  mkdir -p "$node_dir"/{storage,db,applet,search,store_logs,telemetry,babble}

  # ── Babble secp256k1 consensus key ──────────────────────────────────────────
  # caspar-keygen writes to $HOME/.babble/{priv_key,key.pub}.  We override HOME
  # so each node gets its own key, and we copy BOTH files so _gen_peers_genesis
  # can build peers.genesis.json from the SEC1-encoded key.pub directly —
  # without re-deriving the pubkey in Python (which would require the
  # `cryptography` package to be installed at runtime).
  local keygen_bin="$REPO_DIR/dist/bin/caspar-keygen"
  [[ -x "$keygen_bin" ]] \
    || die "caspar-keygen not found at $keygen_bin — run build-dist.sh first"
  local tmp_home; tmp_home=$(mktemp -d)
  HOME="$tmp_home" "$keygen_bin" >/dev/null 2>&1 || true
  if [[ ! -f "$tmp_home/.babble/priv_key" || ! -f "$tmp_home/.babble/key.pub" ]]; then
    rm -rf "$tmp_home"
    die "caspar-keygen did not produce priv_key + key.pub for node${n}"
  fi
  cp "$tmp_home/.babble/priv_key" "$node_dir/babble/priv_key"
  cp "$tmp_home/.babble/key.pub"  "$node_dir/babble/key.pub"
  rm -rf "$tmp_home"

  # ── RSA identity key (OWNER_PRIVATE_KEY, PKCS#8 PEM) ───────────────────────
  # Prefer openssl (always present) over pycryptodome.
  local priv_pem=""
  if command -v openssl &>/dev/null; then
    priv_pem=$(openssl genrsa 2048 2>/dev/null \
               | openssl pkcs8 -topk8 -nocrypt 2>/dev/null)
  fi
  if [[ -z "$priv_pem" ]]; then
    priv_pem=$(python3 -c "
from Crypto.PublicKey import RSA
print(RSA.generate(2048).export_key().decode(), end='')
" 2>/dev/null) || true
  fi
  [[ -z "$priv_pem" ]] && die "Cannot generate RSA key for node${n}: install openssl or pycryptodome"

  # ── Port layout (matches the NODE LAYOUT above) ──────────────────────────────
  local tcp_port=${NODE_TCP[$n]}            # 8074 / 8174 / 8274
  local ws_port=$((tcp_port + 2))           # 8076 / 8176 / 8276
  local fed_port=$((tcp_port + 3))          # 8077 / 8177 / 8277
  local chain_port=$((tcp_port + 4))        # 8078 / 8178 / 8278
  local entity_port=$((tcp_port + 5))       # 8079 / 8179 / 8279
  local vm_port=$((tcp_port + 6))           # 8080 / 8180 / 8280
  local tel_port=$((9099 + (n - 1) * 100))  # 9099 / 9199 / 9299
  # pprof profiling HTTP server. The node defaults PPROF_PORT to 9999 when
  # unset; since both docker (--network host) and local triple-node runs
  # share one host network, all three nodes would otherwise collide on 9999.
  local pprof_port=$((9999 + (n - 1) * 100)) # 9999 / 10099 / 10199

  # Per-node QuestDB ports — each node gets its own QuestDB instance so
  # docker containers (which share host net via --network host) do not
  # collide on the default 8812. Every listener QuestDB opens must be
  # remapped, otherwise the second/third JVM crashes on bind() and the
  # entrypoint exits before caspar-node ever starts.
  local qdb_pg_port=$((8812 + (n - 1) * 100))        # 8812 / 8912 / 9012
  local qdb_http_port=$((9000 + (n - 1) * 100))      # 9000 / 9100 / 9200
  local qdb_http_min_port=$((9003 + (n - 1) * 100))  # 9003 / 9103 / 9203
  local qdb_ilp_port=$((9009 + (n - 1) * 100))       # 9009 / 9109 / 9209

  local is_head root_node
  [[ $n -eq 1 ]] && is_head="true" || is_head="false"
  root_node="localhost:${NODE_TCP[1]}"

  # Docker mounts $node_dir as /app/data inside the container, so all paths
  # inside .env use the container prefix.  Local mode overrides them via
  # the env vars set in local_start_node.
  cat > "$env_file" <<EOF
OWNER_ID=owner-node${n}
OWNER_PRIVATE_KEY="${priv_pem}"
STORAGE_ROOT_PATH=/app/data/storage
BASE_DB_PATH=/app/data/db
APPLET_DB_PATH=/app/data/applet
SEARCH_INDEX_PATH=/app/data/search
STORE_LOGS_DB=/app/data/store_logs
CLIENT_WS_API_PORT=${ws_port}
CLIENT_TCP_API_PORT=${tcp_port}
FEDERATION_API_PORT=${fed_port}
BLOCKCHAIN_API_PORT=${chain_port}
ENTITY_API_PORT=${entity_port}
VM_API_PORT=${vm_port}
PPROF_PORT=${pprof_port}
ORIGIN=http://localhost:${tcp_port}
IPADDR=127.0.0.1
ROOT_NODE=${root_node}
IS_HEAD=${is_head}
AdminPassword=admin123
VM_EXEC_COST_PER_SECOND=0
VM_RAM_COST_PER_MB_PER_MINUTE=0
VM_CPU_CORE_COST_PER_MINUTE=0
VM_DISK_COST_PER_GB_PER_MINUTE=0
TELEMETRY_API_PORT=${tel_port}
TELEMETRY_DB_PATH=/app/data/telemetry
BABBLE_DIR=/app/data/babble
BABBLE_DATA_DIR=/app/data/babble
QUESTDB_PORT=${qdb_pg_port}
QUESTDB_HTTP_PORT=${qdb_http_port}
QUESTDB_HTTP_MIN_PORT=${qdb_http_min_port}
QUESTDB_ILP_PORT=${qdb_ilp_port}
QUESTDB_DATA_DIR=/app/data/questdb
EOF

  ok "node${n} config generated (TCP=${tcp_port}, IS_HEAD=${is_head})"
}

ensure_node_config() {
  local n="$1"
  local node_dir="$DATA_ROOT/node${n}"
  mkdir -p "$node_dir"/{storage,db,applet,search,store_logs,telemetry,babble}
  # Generate .env + babble key if not already present (fresh environment).
  _generate_node_config "$n"
}

# ─── Local-mode launch ───────────────────────────────────────────────────────
local_start_node() {
  local n="$1"
  local node_dir="$DATA_ROOT/node${n}"
  local env_file="$node_dir/.env"
  local log_file="$node_dir/node.log"

  # Load all vars from .env (keys, ports, etc.), then override the /app/data
  # container paths with real per-node host paths so nodes don't collide.
  [[ -f "$env_file" ]] && { set -a; source "$env_file"; set +a; }

  info "Starting node$n locally (TCP=${NODE_TCP[$n]})…"
  # Without gVisor, tell the node to launch docker creatures under the stock
  # runc runtime and skip the storage_opt disk quota (which needs overlay2+XFS
  # pquota), so containers start on hosts where `runsc` isn't registered.
  # With gVisor (the default), these stay unset and the node keeps its
  # sandboxed `runsc` + disk-quota posture.
  if ! $SETUP_GVISOR; then
    export CASPAR_DOCKER_RUNTIME=runc
    export CASPAR_DOCKER_DISK_QUOTA=0
  fi
  # Ensure dist/lib/wasmedge is on the dynamic linker path so libwasmedge.so.0 is found.
  local wasmedge_lib_dir="$REPO_DIR/dist/lib/wasmedge"
  # The real libwasmedge.so is stored in Git LFS; if the repo was cloned/pulled
  # without Git LFS the symlink resolves to a tiny LFS pointer and the node
  # fails at dlopen. Detect that early and print a clear fix.
  local _wasmedge_real
  _wasmedge_real="$(readlink -f "$wasmedge_lib_dir/libwasmedge.so.0" 2>/dev/null || true)"
  if [[ -n "$_wasmedge_real" && "$(head -c 4 "$_wasmedge_real" 2>/dev/null)" != $'\x7fELF' ]]; then
    echo "ERROR: $_wasmedge_real is not a valid ELF library (unresolved Git LFS pointer)." >&2
    echo "       Install Git LFS and fetch it:  git lfs install && git lfs pull" >&2
    exit 1
  fi
  local launch_ld_path="${wasmedge_lib_dir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  # Override the /app/data container paths with per-node host paths.
  LD_LIBRARY_PATH="$launch_ld_path" \
  BABBLE_DIR="$node_dir/babble" \
  BABBLE_DATA_DIR="$node_dir/babble" \
  STORAGE_ROOT_PATH="$node_dir/storage" \
  BASE_DB_PATH="$node_dir/db" \
  APPLET_DB_PATH="$node_dir/applet" \
  SEARCH_INDEX_PATH="$node_dir/search" \
  STORE_LOGS_DB="$node_dir/store_logs" \
  TELEMETRY_DB_PATH="$node_dir/telemetry" \
  QUESTDB_DATA_DIR="$node_dir/questdb" \
  "$BINARY" >> "$log_file" 2>&1 &
  echo $! > "$node_dir/caspar.pid"
  echo $!
}

# ─── Docker-mode launch ──────────────────────────────────────────────────────
# Path translation: .env host paths (/tmp/caspar/nodeN/) → container (/app/data/)
# --network host: ports match local mode exactly; no NAT or port-mapping needed.
docker_start_node() {
  local n="$1"
  local node_dir="$DATA_ROOT/node${n}"
  local env_file="$node_dir/.env"
  local docker_env="$node_dir/.env.docker"
  local container="caspar-node${n}"

  if [[ ! -f "$env_file" ]]; then
    warn "No $env_file for node$n — container may fail to start without keys"
    touch "$env_file"
  fi

  sed "s|${DATA_ROOT}/node${n}|/app/data|g" "$env_file" > "$docker_env"

  info "Starting node$n in docker (container=$container, TCP=${NODE_TCP[$n]})…"

  local docker_args=(
    --name "$container"
    --network host
    --restart unless-stopped
    -v "$docker_env":/app/.env:ro
    -v "$node_dir":/app/data
  )
  [[ -S /var/run/docker.sock ]] \
    && docker_args+=( -v /var/run/docker.sock:/var/run/docker.sock )
  docker run -d "${docker_args[@]}" "$DOCKER_IMAGE" >/dev/null
  echo "$container"
}

# ─── Generate babble peers.genesis.json for all nodes ────────────────────────
# The node's Rust bootstrap copies
# BABBLE_DATA_DIR/{priv_key,key.pub,peers.genesis.json} into each shard
# directory. We pre-generate the peer list here before any node starts so every
# node can find it on first boot without a network call.
#
# The pubkey is read from each node's already-written key.pub (produced by
# caspar-keygen as SEC1-encoded uncompressed-point hex — exactly the format
# babble's PeerSet expects after upper-casing and a "0X" prefix). Doing it
# this way means peers.genesis.json generation has no Python crypto
# dependency, so it works in any CI environment where the build artefacts
# are available.
_gen_peers_genesis() {
  info "Generating babble peers.genesis.json for all nodes…"
  local pub_keys=()
  for n in "${NODES[@]}"; do
    local pub_file="$DATA_ROOT/node${n}/babble/key.pub"
    if [[ ! -f "$pub_file" ]]; then
      warn "babble key.pub missing for node$n — peers.genesis.json skipped"
      return 1
    fi
    local pub_hex; pub_hex=$(tr -d '[:space:]' < "$pub_file")
    if [[ -z "$pub_hex" ]]; then
      warn "babble key.pub empty for node$n — peers.genesis.json skipped"
      return 1
    fi
    pub_keys+=("$pub_hex")
  done

  # Build the peers JSON array.
  local peers_json="["
  local sep=""
  local i=0
  for n in "${NODES[@]}"; do
    local chain_port=$((NODE_TCP[$n] + 4))   # 8078 / 8178 / 8278
    local pub_upper; pub_upper=$(echo "${pub_keys[$i]}" | tr '[:lower:]' '[:upper:]')
    peers_json+="${sep}{\"NetAddr\":\"127.0.0.1:${chain_port}\",\"PubKeyHex\":\"0X${pub_upper}\",\"Moniker\":\"node${n}\"}"
    sep=","
    i=$((i+1))
  done
  peers_json+="]"

  # Write to every node's babble directory.
  for n in "${NODES[@]}"; do
    echo "$peers_json" > "$DATA_ROOT/node${n}/babble/peers.genesis.json"
  done
  ok "Babble peers.genesis.json written for ${#NODES[@]} node(s)"
}

# ─── Launch nodes ────────────────────────────────────────────────────────────
declare -a STARTED_PIDS=()
declare -a STARTED_CONTAINERS=()

for n in "${NODES[@]}"; do
  ensure_node_config "$n"
done

_gen_peers_genesis \
  || die "Babble peer bootstrap failed — refusing to start cluster (followers can't bootstrap shards, head can't reach consensus on chains/registerNode)"

for n in "${NODES[@]}"; do
  if $USE_DOCKER; then
    container=$(docker_start_node "$n")
    STARTED_CONTAINERS+=("$container")
  else
    pid=$(local_start_node "$n")
    STARTED_PIDS+=("$pid")
  fi
done

# ─── Wait for nodes to accept connections ────────────────────────────────────
info "Waiting for node(s) to accept connections…"
all_up=true
for n in "${NODES[@]}"; do
  port=${NODE_TCP[$n]}
  if wait_for_port localhost "$port" "node$n" 120; then
    ok "node$n up on TCP port $port"
  else
    $USE_DOCKER \
      && warn "node$n (port $port): timeout — check: docker logs caspar-node${n}" \
      || warn "node$n (port $port): timeout — check: tail $DATA_ROOT/node${n}/node.log"
    all_up=false
  fi
done

# ─── Clone, build, and deploy WASM creatures ─────────────────────────────────
# run-nodes.sh owns the full setup so the cluster is ready for benchmarking
# the moment the script exits. bench-all.sh auto-detects this and skips the
# deploy step when deployment_report.json already contains successful entries.
#
# The TCP-port check above is a fast initial probe; some nodes (especially
# on a freshly-built Docker image) take longer than that to accept connections.
# We therefore always proceed with the clone/build/deploy pipeline and rely on
# _wait_nodes_app_ready (180 s) as the definitive readiness gate right before
# creatures are pushed to the nodes.
if $DEPLOY_CREATURES; then
  if ! $all_up; then
    warn "One or more node(s) did not respond on TCP within the initial 120 s window."
    warn "Proceeding with creature build — node(s) may still be initialising."
    warn "_wait_nodes_app_ready will verify readiness (up to 180 s) before deploying."
  fi
  if _ensure_decillionai_server; then
    _as_root _install_tinygo
    _as_root _install_python_deps
    # Make Go 1.23 / TinyGo visible in the current shell after root install
    export PATH="/usr/local/go123/bin:/usr/local/tinygo/bin:${PATH}"
    export GOROOT="/usr/local/go123"
    if _build_creatures "$DECILLIONAI_SERVER_DIR"; then
      _wait_nodes_app_ready
      if ! _deploy_creatures "$DECILLIONAI_SERVER_DIR"; then
        # Mark the deploy step as failed via a sentinel file the workflow can
        # check after this script exits — set -e is not used here and we still
        # want the surrounding "summary" block to print.
        mkdir -p "$DATA_ROOT" && : > "$DATA_ROOT/deploy.failed"
        warn "Creature deployment failed — bench-all.sh will refuse to run"
      fi
    fi
  fi
else
  info "Skipping creature build/deploy (--skip-deploy)"
fi

# ─── Summary ─────────────────────────────────────────────────────────────────
echo ""
$all_up && ok "All ${#NODES[@]} node(s) running." || warn "Some node(s) may not have started correctly."

echo ""
echo "  Mode:    $MODE (${#NODES[@]} node(s)) — $($USE_DOCKER && echo 'docker' || echo 'local')"
$USE_DOCKER  && echo "  Image:   $DOCKER_IMAGE"
$USE_DOCKER  || echo "  Binary:  $BINARY"
echo "  Data:    $DATA_ROOT"
if $USE_DOCKER; then
  echo "  QuestDB: one instance inside each node container (PG 8812/8912/9012 on host net)"
else
  qdb_summary=""
  for n in "${NODES[@]}"; do
    qdb_summary+="node${n}=localhost:$((8812 + (n - 1) * 100)) "
  done
  echo "  QuestDB: ${qdb_summary}"
fi
echo ""
if $USE_DOCKER; then
  echo "  Containers:"
  for c in "${STARTED_CONTAINERS[@]}"; do echo "    $c → docker logs -f $c"; done
  echo ""
  echo "  Stop all:  $REPO_DIR/stop-nodes.sh"
else
  echo "  Logs:"
  for n in "${NODES[@]}"; do echo "    node$n → $DATA_ROOT/node${n}/node.log"; done
  echo ""
  echo "  Stop all:  $REPO_DIR/stop-nodes.sh   (or Ctrl-C in this terminal)"
fi
echo ""

# ─── Foreground vs detached ──────────────────────────────────────────────────
# Both docker and local mode exit immediately unless --foreground is passed.
# In local mode the node processes run as disowned background jobs; their PIDs
# are saved to /tmp/caspar/nodeN/caspar.pid and can be stopped via stop-nodes.sh.
if ! $FOREGROUND; then
  $USE_DOCKER \
    && info "Containers running detached. Run with --foreground to tail logs." \
    || info "Nodes running detached. Run with --foreground to tail logs."
  exit 0
fi

cleanup() {
  echo ""
  info "Shutting down…"
  for p in "${STARTED_PIDS[@]:-}";      do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  for c in "${STARTED_CONTAINERS[@]:-}"; do [[ -n "$c" ]] && docker stop --time 10 "$c" >/dev/null 2>&1 || true; done
  for p in "${QUESTDB_PIDS[@]:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null || true; done
  exit 0
}
trap cleanup INT TERM

info "Press Ctrl-C to stop everything."
if $USE_DOCKER; then
  docker logs -f --tail=20 "${STARTED_CONTAINERS[0]}" &
  wait $!
else
  wait
fi
