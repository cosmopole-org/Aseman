//! Creature identity values shared by the creature use cases and their adapters.
//!
//! Creature identities are the legacy string identities (`{n}@{origin}`); capsule
//! adapters map them to canonical IDs. Balances are not part of the identity record:
//! the target model keeps them in finance wallets, behind their own port.

use serde::{Deserialize, Serialize};

/// The creature type whose record is also a user account.
pub const HUMAN_CREATURE_TYPE: &str = "human";

/// The owner legacy records on every human: humans own themselves.
pub const HUMAN_OWNER: &str = "free";

/// One creature's identity, exactly as legacy exposes it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatureRecord {
    pub id: String,
    pub creature_type: String,
    /// `{name}@{origin}`; unique across creatures.
    pub username: String,
    /// The RSA SPKI public key as PEM text.
    pub public_key: String,
    pub chain_id: String,
    pub subchain_id: String,
    /// The owning creature, or [`HUMAN_OWNER`] for a human.
    pub owner_id: String,
}

impl CreatureRecord {
    #[must_use]
    pub fn is_human(&self) -> bool {
        self.creature_type == HUMAN_CREATURE_TYPE
    }
}

/// A creature's metadata documents (ADR 0016): the creature's own and its user's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MetadataKind {
    /// Legacy `CreatMeta::{id}`, target `core.creature_metadata`.
    Creature,
    /// Legacy `UserMeta::{id}`, target `core.user_metadata`.
    User,
}

/// The root path of every creature metadata document.
pub const METADATA_ROOT: &str = "metadata";

/// The legacy object-list window: skip `offset` items, then yield while fewer than
/// `offset + count` have been counted. A negative offset skips nothing, and without a
/// count the window is unbounded. Both creature adapters page with this, so a caller
/// sees the same page from either provider.
pub fn legacy_page<T>(
    items: impl IntoIterator<Item = T>,
    offset: i64,
    count: Option<i64>,
) -> Vec<T> {
    let mut page = Vec::new();
    let mut index: i64 = 0;
    for item in items {
        if index < offset {
            index += 1;
            continue;
        }
        if count.is_some_and(|count| index >= offset.saturating_add(count)) {
            break;
        }
        index += 1;
        page.push(item);
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_page_matches_the_object_list_window() {
        let items = || 0..6;
        assert_eq!(legacy_page(items(), 2, Some(3)), [2, 3, 4]);
        assert_eq!(legacy_page(items(), 0, None), [0, 1, 2, 3, 4, 5]);
        assert_eq!(legacy_page(items(), 4, Some(10)), [4, 5]);
        // Legacy quirks, preserved: a negative count is empty, a negative offset
        // shortens the window.
        assert!(legacy_page(items(), 0, Some(-1)).is_empty());
        assert_eq!(legacy_page(items(), -2, Some(4)), [0, 1]);
    }
}
