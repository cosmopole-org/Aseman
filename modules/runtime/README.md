# Runtime implementations

This is the single source tree for Aseman's seven executable runtime plugins:
Docker, Elpian, Elpify, Firecracker, JavaScript, Modal, and WebAssembly. Each
runtime owns its implementation and `vm.config.json`; shared compatibility
traits live in `sdk-legacy/` while the runtime contract is migrated to the
versioned VMM contracts.

The enabled set is recorded in `vms.state.json`. `asemanctl vms sync`
regenerates the compatibility registry in
`modules/vmm-backend/native-legacy/crates/caspar-vm-plugins`; no runtime is
proxied through an extra top-level crate.

To add a runtime:

```sh
asemanctl vms new <key>
asemanctl vms list
asemanctl vms sync
```

Runtime projects must expose a Rust library, contain a `vm.config.json`, and
register an implementation of the compatibility SDK's `VmPlugin` contract.
