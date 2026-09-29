//! The node's transaction (ADR 0036).
//!
//! Every state action runs in one [`Trx`] on the storage provider plugin the node
//! loaded. Node code reads and writes models through the typed client
//! (`trx.store().find_many(…)`, [`Models`]); it never names a provider, a driver,
//! or a key layout.

pub use aseman_storage::Trx;

/// A storage error as the `anyhow` error node actions return.
pub fn failed(error: aseman_storage::StorageError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

/// The currency and scale of creature balances (the finance epoch of ADR 0017).
pub use aseman_domain::creature::{BALANCE_CURRENCY, BALANCE_SCALE};

/// An in-memory storage for tests: the reference provider over the model catalog.
#[cfg(test)]
pub(crate) fn test_storage() -> aseman_storage::Storage {
    aseman_storage::Storage::new(
        aseman_storage::memory::MemoryProvider::new(),
        aseman_storage::schema::Schema::catalog().expect("model catalog"),
    )
}

/// A read-write transaction on a fresh [`test_storage`].
#[cfg(test)]
pub(crate) fn test_trx() -> Trx {
    test_storage()
        .begin(aseman_storage::Mode::ReadWrite)
        .expect("memory transaction")
}
