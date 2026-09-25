ARG RUNTIME_IMAGE=debian:12-slim
FROM ${RUNTIME_IMAGE}

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates libgcc-s1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 65532 -g 65532 /var/lib/aseman
COPY --chmod=0555 dist/bin/aseman-node /usr/local/bin/aseman-node
COPY --chmod=0555 dist/bin/aseman-keygen /usr/local/bin/aseman-keygen
COPY dist/lib/wasmedge/ /usr/local/lib/

ENV LD_LIBRARY_PATH=/usr/local/lib
USER 65532:65532
WORKDIR /var/lib/aseman
VOLUME ["/var/lib/aseman"]
EXPOSE 443 8080 8444
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 CMD ["/bin/sh", "-c", "kill -0 1"]
ENTRYPOINT ["/usr/local/bin/aseman-node"]
