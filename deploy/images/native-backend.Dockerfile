# The native VMM backend (the compact `native` profile): the runtime plugins that need
# no host device, and the WasmEdge library the backend links. WasmEdge is pinned in
# contracts/release/runtime-dependencies.json (A906); the build refuses an archive
# whose SHA-256 differs. The arm64 build passes that archive's URL and digest.
# glibc 2.41: at least the release builders (ubuntu-24.04, glibc 2.39).
ARG RUNTIME_IMAGE=debian:13-slim
FROM ${RUNTIME_IMAGE}

ARG WASMEDGE_URL=https://github.com/WasmEdge/WasmEdge/releases/download/0.17.1/WasmEdge-0.17.1-manylinux_2_28_x86_64.tar.gz
ARG WASMEDGE_SHA256=27a1abec072ddf45b40e2e81e33c1e5fe9b241f31fd1bbf0182f05097489a07a

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl libgcc-s1 libstdc++6 zlib1g \
    && curl --fail --silent --show-error --location "$WASMEDGE_URL" --output /tmp/wasmedge.tgz \
    && echo "$WASMEDGE_SHA256  /tmp/wasmedge.tgz" | sha256sum --check --strict - \
    && tar -xzf /tmp/wasmedge.tgz -C /tmp lib64/libwasmedge.so.0.1.1 \
    && install -d /usr/local/lib/aseman \
    && install -m 0644 /tmp/lib64/libwasmedge.so.0.1.1 /usr/local/lib/aseman/ \
    && ln -s libwasmedge.so.0.1.1 /usr/local/lib/aseman/libwasmedge.so.0 \
    && rm -rf /tmp/wasmedge.tgz /tmp/lib64 \
    && apt-get purge --yes curl \
    && apt-get autoremove --yes \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 65532 -g 65532 /var/lib/aseman-backend
# Found through the binary's $ORIGIN/../lib/aseman RPATH.
COPY --chmod=0555 bin/aseman-vmm-backend-native /usr/local/bin/aseman-vmm-backend-native

USER 65532:65532
WORKDIR /var/lib/aseman-backend
HEALTHCHECK --interval=30s --timeout=3s --start-period=15s --retries=3 CMD ["/bin/sh", "-c", "kill -0 1"]
ENTRYPOINT ["/usr/local/bin/aseman-vmm-backend-native"]
