//! Legacy transaction adapter for domain-owned store permissions.

pub use aseman_domain::store_permissions::StorePermissions;
use aseman_domain::store_permissions::legacy_access_link_key;

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
