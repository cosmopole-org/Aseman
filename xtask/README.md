# xtask

- `cargo xtask arch` checks the dependency direction without building anything.
- `cargo xtask fast` formats, checks the architecture, verifies every generated
  contract and inventory is current, runs the contract checks, and tests and lints
  the workspace's core packages.
- `cargo xtask full` runs `fast`, then the node's binaries and the native VMM
  backend with every runtime plugin it links.
