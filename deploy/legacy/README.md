# Legacy combined-node deployment

This directory contains the compatibility image used by `run-nodes.sh`. It
runs the Caspar-compatible node and QuestDB in one container and is not the
target production topology. Current one-process images live in `deploy/images`
and the authoritative topology is `contracts/deploy/topology.json`.

The files are retained under RL-016 as a rollback path. The former `node/`
source root was removed; no application code or shard-bootstrap shell script is
owned here.
