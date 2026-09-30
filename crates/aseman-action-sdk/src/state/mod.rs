//! The node's wire models and the ports of one state action over the storage
//! module (ADR 0036), shared with the action plugins (ADR 0040).

pub mod access;
pub mod bridges;
pub mod creature;
pub mod creature_ports;
pub mod entity_ports;
pub mod finance_ports;
pub mod gateway_ports;
pub mod machine_program;
pub mod program_ports;
pub mod secrets;
pub mod session;
pub mod store;
pub mod store_ports;
pub mod token_locks;
pub mod vm_runtime;

pub use access::StorePermissions;
pub use creature::Creature;
pub use machine_program::Program;
pub use session::Session;
pub use store::Store;

/// A storage error as the `anyhow` error handlers return.
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
pub(crate) fn test_trx() -> crate::util::Trx {
    test_storage()
        .begin(aseman_storage::Mode::ReadWrite)
        .expect("memory transaction")
}

/// Seed human creatures `ids` (each needs a distinct key), for tests.
#[cfg(test)]
pub(crate) fn seed_humans(trx: &crate::util::Trx, ids: &[&str]) {
    use aseman_domain::creature::CreatureRecord;
    use aseman_ports::CreatureDirectory;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};

    let creatures = creature_ports::CreaturePorts { trx };
    for (id, key) in ids.iter().zip([0, 1, 2, 3, 4].map(|_| {
        rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap())
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    })) {
        creatures
            .create(&CreatureRecord {
                id: (*id).to_owned(),
                creature_type: "human".to_owned(),
                username: format!("{}.name", id.replace('@', "-")),
                public_key: key,
                chain_id: "main".to_owned(),
                subchain_id: "main".to_owned(),
                owner_id: aseman_domain::creature::HUMAN_OWNER.to_owned(),
            })
            .unwrap();
    }
}