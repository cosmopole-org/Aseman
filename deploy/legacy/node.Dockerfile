# =============================================================================
# Caspar Node — pre-built image
#
# IMPORTANT: Run `./build-dist.sh` at the repo root BEFORE `docker build`.
# That script compiles caspar-node, casparctl, and copies all native libraries
# into dist/.  This Dockerfile copies those artifacts directly, so the build
# takes ~30s instead of ~45 min.
#
# Build:
#   ./build-dist.sh
#   docker build -f deploy/legacy/node.Dockerfile -t caspar-node:latest .
#
# Run (single node, minimal):
#   docker run -d \
#     --env-file /tmp/caspar/node1/.env \
#     -e BABBLE_DIR=/app/data/babble \
#     -p 8074:8074 -p 8076:8076 -p 8077:8077 \
#     caspar-node:latest
# =============================================================================

FROM ubuntu:24.04
WORKDIR /app

ENV DEBIAN_FRONTEND=noninteractive

# ─── Runtime dependencies only (no compiler, no Rust) ───────────────────────
# Everything else is statically linked into caspar-node.
RUN apt-get update -y && apt-get install -y --no-install-recommends \
    libstdc++6 \
    zlib1g \
    libtinfo6 \
    ca-certificates \
    curl \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# ─── WasmEdge runtime library (pre-built, from dist/lib/wasmedge/) ──────────
# The real libwasmedge.so is stored in Git LFS. The build context must be a
# checkout with LFS objects fetched (git lfs pull); otherwise the copied file
# is a small LFS pointer and the node fails at dlopen.
COPY dist/lib/wasmedge/ /usr/local/lib/wasmedge/

RUN real="$(readlink -f /usr/local/lib/wasmedge/libwasmedge.so.0)" \
 && if [ "$(head -c 4 "$real" | od -An -tx1 | tr -d ' \n')" != "7f454c46" ]; then \
        echo "ERROR: $real is not an ELF library — the build context has an unresolved Git LFS pointer. Run 'git lfs install && git lfs pull' before building." >&2; \
        exit 1; \
    fi \
 && echo "/usr/local/lib/wasmedge" > /etc/ld.so.conf.d/wasmedge.conf \
 && ldconfig

ENV LD_LIBRARY_PATH="/usr/local/lib/wasmedge"
ENV WASMEDGE_LIB_DIR="/usr/local/lib/wasmedge"

# ─── Node binaries (pre-built, from dist/bin/) ──────────────────────────────
COPY dist/bin/aseman-node    /usr/local/bin/aseman-node
COPY dist/bin/aseman-keygen  /usr/local/bin/aseman-keygen
COPY dist/bin/asemanctl      /usr/local/bin/asemanctl
COPY dist/bin/caspar-node    /usr/local/bin/caspar-node
COPY dist/bin/caspar-keygen  /usr/local/bin/caspar-keygen
COPY dist/bin/casparctl      /usr/local/bin/casparctl

RUN chmod +x /usr/local/bin/aseman-node \
             /usr/local/bin/aseman-keygen \
             /usr/local/bin/asemanctl \
             /usr/local/bin/caspar-node \
             /usr/local/bin/caspar-keygen \
             /usr/local/bin/casparctl

# ─── Java (for QuestDB telemetry process) ───────────────────────────────────
RUN apt-get update -y && apt-get install -y --no-install-recommends \
    openjdk-17-jre-headless \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

# ─── QuestDB telemetry server (pre-fetched, from dist/questdb/) ─────────────
COPY dist/questdb/questdb.jar /opt/questdb/questdb.jar

# ─── Firecracker microVM manager (optional) ─────────────────────────────────
# Pass --build-arg INSTALL_FIRECRACKER=false to skip all downloads (saves
# ~120 MB and avoids external fetches when Firecracker is not needed, e.g.
# CI benchmark runs or environments without /dev/kvm).
# run-nodes.sh passes this flag automatically when --no-firecracker is used.
# Network bridge setup (br0 / NAT) is always a host-level operation in
# run-nodes.sh and is never performed inside this image.
ARG INSTALL_FIRECRACKER=true
ARG FC_VERSION=v1.10.1

# All three steps (deps, binary, kernel) are consolidated into one layer so
# that setting INSTALL_FIRECRACKER=false creates a single no-op layer instead
# of three skipped layers.
RUN set -e; \
    [ "${INSTALL_FIRECRACKER}" = "true" ] || exit 0; \
    apt-get update -y && apt-get install -y --no-install-recommends \
        libelf-dev e2fsprogs \
    && apt-get clean && rm -rf /var/lib/apt/lists/*; \
    FC_ARCH=$(uname -m); \
    TGZ="firecracker-${FC_VERSION}-${FC_ARCH}.tgz"; \
    curl -fsSL \
      "https://github.com/firecracker-microvm/firecracker/releases/download/${FC_VERSION}/${TGZ}" \
      -o "/tmp/${TGZ}"; \
    tar -xzf "/tmp/${TGZ}" -C /tmp; \
    mv "/tmp/release-${FC_VERSION}-${FC_ARCH}/firecracker-${FC_VERSION}-${FC_ARCH}" \
       /usr/local/bin/firecracker; \
    chmod +x /usr/local/bin/firecracker; \
    rm -rf "/tmp/${TGZ}" "/tmp/release-${FC_VERSION}-${FC_ARCH}"; \
    mkdir -p /opt/firecracker/{vms,kernel,rootfs,snapshots}; \
    curl -fsSL \
      "https://s3.amazonaws.com/spec.ccfc.min/img/quickstart_guide/${FC_ARCH}/kernels/vmlinux.bin" \
      -o /opt/firecracker/kernel/vmlinux; \
    chmod +x /opt/firecracker/kernel/vmlinux

# ─── Entrypoint ─────────────────────────────────────────────────────────────
COPY deploy/legacy/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

RUN mkdir -p /app/data

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
