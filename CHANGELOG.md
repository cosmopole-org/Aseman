# Changelog

This file records release-visible changes.

## Unreleased

- The node runs every operation through one router shared by HTTP, the signed-packet
  transports, the chain, federation, and guest calls (ADR 0039).
- The node's operations are action plugins (ADR 0040): every action lives in its own
  crate under `modules/actions/`, is loaded into the node process at startup by the
  `aseman-action-plugins` aggregation crate, and is connected to the router from the
  SDK registry (`aseman-action-sdk`). The node itself names no operation.
- Configuration keys lose their `ASEMAN_LEGACY_` prefix: `ASEMAN_LEGACY_X` is now
  `ASEMAN_X`, `ASEMAN_LEGACY_CONSENSUS_PORT` is `ASEMAN_CHAIN_PORT`, and
  `ASEMAN_LEGACY_CASPARCTL_*` is `ASEMAN_CTL_*`.
- The command-line client is `aseman-client`, configured by `ASEMAN_HOST`,
  `ASEMAN_PROTO`, `ASEMAN_PORT`, `ASEMAN_TLS`, `ASEMAN_INSECURE`, and
  `ASEMAN_SIGNAL_TIMEOUT_MS`.
- Every persisted port runs on the storage module, on every provider (ADR 0038).
