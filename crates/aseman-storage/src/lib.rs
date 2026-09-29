//! The storage API (ADR 0036).
//!
//! - [`schema`]: the model catalog, from the provider-neutral schemas.
//! - [`engine`]: the Prisma-style transaction every node module uses.
//! - [`provider`]: the plugin contract a storage provider implements.
//! - [`memory`]: the in-memory reference provider.
#![forbid(unsafe_code)]

pub mod client;
pub mod codec;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod engine;
pub mod error;
pub mod eval;
pub mod memory;
pub mod provider;
pub mod query;
pub mod schema;
pub mod typed;
pub mod value;

pub use client::Models;
pub use engine::{Storage, Trx};
pub use error::{StorageError, StorageResult};
pub use provider::{Mode, ProviderPlugin, ProviderSettings, Registry, StorageProvider};
pub use query::{Case, Cond, Direction, FindMany, Order, Unique, Where};
pub use value::{Data, Id, Row, Value};

#[cfg(all(test, feature = "conformance"))]
mod tests {
    #[test]
    fn the_memory_provider_passes_the_conformance_suite() {
        let schema = crate::schema::Schema::catalog().unwrap();
        let storage = crate::Storage::new(crate::memory::MemoryProvider::new(), schema);
        crate::conformance::storage_provider(&storage);
    }
}

#[cfg(test)]
mod typed_client_tests {
    use crate::client::core::{store, user};
    use crate::{FindMany, Mode, Models, Storage, StorageError};

    #[test]
    fn the_generated_client_reads_like_prisma() {
        let storage = Storage::new(
            crate::memory::MemoryProvider::new(),
            crate::schema::Schema::catalog().unwrap(),
        );
        let trx = storage.begin(Mode::ReadWrite).unwrap();
        let ada = trx
            .user()
            .create(user::Create {
                username: "ada".into(),
                public_key: vec![1; 8],
                status: "active".into(),
                email: Some("ada@x.io".into()),
            })
            .unwrap();
        assert_eq!(ada.email.as_deref(), Some("ada@x.io"));
        assert!(matches!(
            trx.user().create(user::Create {
                username: "ada".into(),
                public_key: vec![2; 8],
                status: "active".into(),
                email: None,
            }),
            Err(StorageError::Conflict(_))
        ));
        let found = trx
            .user()
            .find_unique(user::by_username("ada"))
            .unwrap()
            .unwrap();
        assert_eq!(found.id, ada.id);
        let renamed = trx
            .user()
            .update(
                user::by_id(ada.id),
                user::update().status("away").email(None),
            )
            .unwrap()
            .unwrap();
        assert_eq!((renamed.status.as_str(), renamed.email), ("away", None));
        let listed = trx
            .user()
            .find_many(
                FindMany::filter(
                    user::username()
                        .starts_with("a")
                        .and(user::status().eq("away")),
                )
                .order_by(user::username().asc())
                .take(10),
            )
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            trx.store()
                .count(Some(store::is_public().eq(true)))
                .unwrap(),
            0
        );
        trx.commit().unwrap();
    }
}
