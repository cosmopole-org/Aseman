# glibc 2.41: at least the release builders (ubuntu-24.04, glibc 2.39); aseman-node needs GLIBC_2.39.
ARG RUNTIME_IMAGE=debian:13-slim
FROM ${RUNTIME_IMAGE}

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*
COPY --chmod=0555 dist/bin/aseman-meter /usr/local/bin/aseman-meter

USER 65532:65532
WORKDIR /var/empty
HEALTHCHECK --interval=30s --timeout=3s --start-period=15s --retries=3 CMD ["/bin/sh", "-c", "kill -0 1"]
ENTRYPOINT ["/usr/local/bin/aseman-meter"]
