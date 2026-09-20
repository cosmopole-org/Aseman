//! Legacy transaction adapter for domain-owned store permissions.

use aseman_domain::store_permissions::legacy_access_link_key;
pub use aseman_domain::store_permissions::{StorePermissions, PERM_MANAGE, PERM_READ, PERM_SIGNAL};

/// The legacy `onaccess::<storeId>::<memberId>` link key.
#[must_use]
pub fn access_link_key(store_id: &str, member_id: &str) -> String {
    legacy_access_link_key(store_id, member_id)
}

/// Read a member's permissions through the legacy transaction adapter.
pub fn read_permissions(
    transaction: &dyn crate::models::transaction::ITrx,
    store_id: &str,
    member_id: &str,
) -> StorePermissions {
    StorePermissions::parse(&transaction.get_link(&access_link_key(store_id, member_id)))
}
