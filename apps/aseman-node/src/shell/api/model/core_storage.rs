//! Core storage routing (ADR 0026): which provider serves the core port families.
//!
//! By default the legacy provider serves everything. When the node is configured for
//! PostgreSQL, every state closure runs inside one PostgreSQL unit of work. The
//! family adapters (`CreaturePorts`, `ProgramPorts`, ...) find it on the current
//! thread and route the core families to capsules, while the families ADR 0026 keeps
//! on legacy (balances and finance, identity credentials, VMM runtime, chains, id
//! allocation) stay on the legacy transaction.
//!
//! Commit order: PostgreSQL first, then legacy. If the legacy commit fails after
//! PostgreSQL committed, the compensations registered during the action run in a new
//! unit of work.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use anyhow::{Result, anyhow};
use aseman_capsule::CapsuleStore;
use aseman_storage_postgres::unit_of_work::{PostgresUnitOfWork, PostgresUnitOfWorkFactory};

static FACTORY: OnceLock<Arc<PostgresUnitOfWorkFactory>> = OnceLock::new();

/// Undoes a capsule write whose legacy counterpart failed to commit.
pub(crate) type Compensation = Box<dyn FnOnce(&dyn CapsuleStore) -> Result<()>>;

#[cfg(test)]
thread_local! {
    /// Tests route one thread to PostgreSQL without touching the process setting.
    static TEST_FACTORY: RefCell<Option<Arc<PostgresUnitOfWorkFactory>>> = const { RefCell::new(None) };
}

/// Route this test thread's actions to `factory` (or back to legacy with `None`).
#[cfg(test)]
pub(crate) fn set_test_factory(factory: Option<Arc<PostgresUnitOfWorkFactory>>) {
    TEST_FACTORY.with(|slot| *slot.borrow_mut() = factory);
}

fn factory() -> Option<Arc<PostgresUnitOfWorkFactory>> {
    #[cfg(test)]
    if let Some(factory) = TEST_FACTORY.with(|slot| slot.borrow().clone()) {
        return Some(factory);
    }
    FACTORY.get().cloned()
}

thread_local! {
    static UNITS: RefCell<Vec<Rc<PostgresUnitOfWork>>> = const { RefCell::new(Vec::new()) };
    static COMPENSATIONS: RefCell<Vec<Vec<Compensation>>> = const { RefCell::new(Vec::new()) };
}

/// Route the core families to PostgreSQL for the rest of the process.
pub(crate) fn install_postgres(factory: PostgresUnitOfWorkFactory) -> Result<()> {
    FACTORY
        .set(Arc::new(factory))
        .map_err(|_| anyhow!("core storage is already installed"))
}

/// The unit of work of the innermost running action, when core families are on
/// PostgreSQL.
pub(crate) fn current_unit() -> Option<Rc<PostgresUnitOfWork>> {
    UNITS.with(|units| units.borrow().last().cloned())
}

/// Register an undo for a capsule write of the current action (ADR 0026 point 4).
pub(crate) fn register_compensation(compensation: Compensation) {
    COMPENSATIONS.with(|stack| {
        if let Some(current) = stack.borrow_mut().last_mut() {
            current.push(compensation);
        }
    });
}

/// Why a state action did not take effect.
pub(crate) enum StateFailure {
    /// The action itself refused; its writes were discarded (LD-15).
    Action(anyhow::Error),
    /// A provider could not begin or commit (LD-10).
    Storage(anyhow::Error),
}

impl StateFailure {
    pub(crate) fn into_error(self) -> anyhow::Error {
        match self {
            Self::Action(error) | Self::Storage(error) => error,
        }
    }
}

/// Run one state action with ADR 0026 commit ordering.
///
/// Without PostgreSQL, `action` runs and the legacy transaction commits or is
/// discarded as before.
pub(crate) fn run_action(
    action: impl FnOnce() -> Result<()>,
    commit_legacy: impl FnOnce() -> Result<()>,
    discard_legacy: impl FnOnce(),
) -> Result<(), StateFailure> {
    let Some(factory) = factory() else {
        return match action() {
            Ok(()) => commit_legacy().map_err(StateFailure::Storage),
            Err(error) => {
                discard_legacy();
                Err(StateFailure::Action(error))
            }
        };
    };
    let unit = Rc::new(
        factory
            .begin()
            .map_err(|error| StateFailure::Storage(anyhow!("{error}")))?,
    );
    UNITS.with(|units| units.borrow_mut().push(unit.clone()));
    COMPENSATIONS.with(|stack| stack.borrow_mut().push(Vec::new()));
    let outcome = action();
    UNITS.with(|units| units.borrow_mut().pop());
    let compensations = COMPENSATIONS
        .with(|stack| stack.borrow_mut().pop())
        .unwrap_or_default();
    let unit = Rc::try_unwrap(unit)
        .map_err(|_| StateFailure::Storage(anyhow!("unit of work is still in use")))?;
    if let Err(error) = outcome {
        discard_legacy();
        let _ = unit.rollback();
        return Err(StateFailure::Action(error));
    }
    if let Err(error) = unit.commit() {
        discard_legacy();
        return Err(StateFailure::Storage(anyhow!(
            "core storage commit failed: {error}"
        )));
    }
    if let Err(error) = commit_legacy() {
        compensate(&factory, compensations).map_err(StateFailure::Storage)?;
        return Err(StateFailure::Storage(error));
    }
    Ok(())
}

/// Undo the capsule writes of an action whose legacy commit failed (ADR 0026).
fn compensate(factory: &PostgresUnitOfWorkFactory, compensations: Vec<Compensation>) -> Result<()> {
    if compensations.is_empty() {
        return Ok(());
    }
    let undo = factory.begin().map_err(|error| anyhow!("{error}"))?;
    for compensation in compensations {
        compensation(&undo)?;
    }
    undo.commit().map_err(|error| anyhow!("{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::TrxWrapper;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::models::ports::storage::IStorage;
    use crate::models::transaction::ITrx;
    use crate::shell::api::model::creature_ports::CreaturePorts;
    use aseman_application::creature::{CreateCreature, NewCreature};
    use aseman_ports::{CreatureBalances, CreatureDirectory};

    fn public_key() -> String {
        use rsa::pkcs8::{EncodePublicKey, LineEnding};
        rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap())
            .to_public_key_pem(LineEnding::LF)
            .unwrap()
    }

    fn create(trx: &dyn ITrx, id: &str, name: &str) -> Result<()> {
        let creatures = CreaturePorts { trx };
        CreateCreature {
            directory: &creatures,
            balances: &creatures,
        }
        .execute(NewCreature {
            id: id.to_owned(),
            creature_type: "human".to_owned(),
            name: name.to_owned(),
            origin: "global".to_owned(),
            public_key: public_key(),
            caller_id: id.to_owned(),
            opening_balance: 9,
            ..NewCreature::default()
        })
        .map(|_| ())
        .map_err(|error| anyhow!("{error}"))
    }

    fn capsule_creature(
        store: &dyn CapsuleStore,
        id: &str,
    ) -> Option<aseman_domain::creature::CreatureRecord> {
        aseman_capsule::creature::CapsuleCreaturePorts {
            repository: store,
            currency: "",
            scale: 0,
        }
        .creature(id)
        .unwrap()
    }

    /// ADR 0026 end to end: the identity goes to PostgreSQL and the balance to
    /// legacy in one action; a refused action leaves neither; a failed legacy
    /// commit compensates the committed identity.
    #[test]
    fn live_actions_route_core_families_to_postgres_with_ordered_commits() {
        let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url
        else {
            eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping core storage test");
            return;
        };
        let database = format!("aseman_core_routing_{}", uuid::Uuid::now_v7().simple());
        let mut admin = postgres::Client::connect(&admin_uri, postgres::NoTls).unwrap();
        admin
            .batch_execute(&format!("CREATE DATABASE {database}"))
            .unwrap();
        // The admin URL may carry a default database path; the new database must be
        // targeted instead, so keep only the authority and append the fresh database.
        let (scheme, rest) = admin_uri.split_once("://").unwrap();
        let authority = rest.split('/').next().unwrap();
        let uri = format!("{scheme}://{authority}/{database}");
        let repository = aseman_storage_postgres::PostgresCapsuleRepository::connect(&uri).unwrap();
        repository.migrate().unwrap();
        let factory = Arc::new(PostgresUnitOfWorkFactory::connect(&uri, 4, Some(1)).unwrap());
        set_test_factory(Some(factory.clone()));

        let storage: Arc<dyn IStorage> = StubStorage::new();
        let begin = || {
            TrxWrapper::new(
                Arc::new(StubCore {
                    storage: storage.clone(),
                }),
                storage.clone(),
                false,
            )
        };
        let balance_key = |id: &str| format!("obj::Creature::{id}::balance");

        // 1. One action: identity on PostgreSQL, balance on legacy.
        let trx = begin();
        run_action(
            || create(&*trx, "1@global", "alice"),
            || trx.commit(),
            || trx.discard(),
        )
        .map_err(StateFailure::into_error)
        .unwrap();
        assert_eq!(
            capsule_creature(&repository, "1@global").map(|record| record.username),
            Some("alice@global".to_owned())
        );
        let read = begin();
        assert_eq!(
            read.get_bytes(&balance_key("1@global")),
            9_u64.to_le_bytes()
        );
        // The legacy provider holds only the balance, not the identity.
        assert!(!read.has_obj("Creature", "1@global"));
        run_action(
            || {
                assert_eq!(
                    CreaturePorts { trx: &*read }.balance("1@global").unwrap(),
                    9
                );
                Ok(())
            },
            || Ok(()),
            || {},
        )
        .map_err(StateFailure::into_error)
        .unwrap();

        // 2. A refused action leaves nothing on either provider.
        let trx = begin();
        let refused = run_action(
            || {
                create(&*trx, "2@global", "bob")?;
                Err(anyhow!("refused after writing"))
            },
            || trx.commit(),
            || trx.discard(),
        );
        assert!(matches!(refused, Err(StateFailure::Action(_))));
        assert_eq!(capsule_creature(&repository, "2@global"), None);
        assert!(begin().get_bytes(&balance_key("2@global")).is_empty());

        // 3. A failed legacy commit compensates the committed identity.
        let trx = begin();
        let failed = run_action(
            || create(&*trx, "3@global", "carol"),
            || Err(anyhow!("legacy commit failed")),
            || trx.discard(),
        );
        assert!(matches!(failed, Err(StateFailure::Storage(_))));
        assert_eq!(capsule_creature(&repository, "3@global"), None);

        set_test_factory(None);
        drop(factory);
        drop(repository);
        admin
            .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .unwrap();
    }
}
