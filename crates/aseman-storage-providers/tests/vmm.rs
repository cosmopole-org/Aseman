//! The VMM service's stores (A501, A503) on the storage module pass their suite on
//! every provider.

mod support;

use aseman_capsule::vmm::StorageVmmStore;
use aseman_ports::conformance::vmm::vmm_stores;

#[test]
fn vmm_stores_pass_the_suite() {
    for store in support::stores() {
        eprintln!("vmm stores on {}", store.name);
        let vmm = StorageVmmStore::new(store.storage.clone());
        vmm_stores(&vmm, &vmm, &vmm, &vmm);
    }
}
