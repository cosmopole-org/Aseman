ARG RUNTIME_IMAGE=debian:12-slim
FROM ${RUNTIME_IMAGE}

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 65532 -g 65532 /var/lib/aseman
COPY --chmod=0555 dist/bin/aseman-vmm /usr/local/bin/aseman-vmm

USER 65532:65532
WORKDIR /var/lib/aseman
EXPOSE 8443 8444
HEALTHCHECK --interval=30s --timeout=3s --start-period=15s --retries=3 CMD ["/bin/sh", "-c", "kill -0 1"]
ENTRYPOINT ["/usr/local/bin/aseman-vmm"]
