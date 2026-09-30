//! The creature ports of one state action (ADR 0036): the creature, user, wallet,
//! metadata, and type-registry models, through the capsule repositories over the
//! action's transaction.

use aseman_capsule::creature::CapsuleCreaturePorts;
use aseman_domain::creature::{CreatureRecord, MetadataKind};
use aseman_ports::{
    CreatureBalances, CreatureDirectory, CreatureMetadata, CreatureTypes, PortError, PortResult,
};

use crate::state::Creature;
use crate::util::Trx;
use crate::state::{BALANCE_CURRENCY, BALANCE_SCALE};

/// The wire shape of a creature: its identity plus its balance.
pub fn creature_view(record: CreatureRecord, balance: i64) -> Creature {
    Creature {
        id: record.id,
        type_name: record.creature_type,
        username: record.username,
        public_key: record.public_key,
        chain_id: record.chain_id,
        subchain_id: record.subchain_id,
        owner_id: record.owner_id,
        balance,
        ..Default::default()
    }
}

/// A creature type's JSON spec (`None` when unregistered).
pub fn creature_type(
    trx: &Trx,
    name: &str,
) -> anyhow::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let spec = aseman_ports::CreatureTypes::creature_type(&CreaturePorts { trx }, name)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(spec.and_then(|text| serde_json::from_str(&text).ok()))
}

/// Store a creature type's JSON spec.
pub fn put_creature_type(trx: &Trx, name: &str, spec: &serde_json::Value) -> anyhow::Result<()> {
    aseman_ports::CreatureTypes::put_creature_type(
        &CreaturePorts { trx },
        name,
        &serde_json::to_string(spec)?,
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

/// A creature type's initial balance. The built-in types have one before their
/// registration runs; any other unregistered type is refused.
pub fn initial_balance(trx: &Trx, name: &str) -> anyhow::Result<i64> {
    match creature_type(trx, name)? {
        Some(spec) => Ok(spec
            .get("initialBalance")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)),
        None => match name {
            "human" | "machine" => Ok(0),
            other => Err(anyhow::anyhow!("unknown creature type: {other}")),
        },
    }
}

/// The creature ports of one state action.
pub struct CreaturePorts<'a> {
    pub trx: &'a Trx,
}

impl CreaturePorts<'_> {
    /// The legacy id of the creature a typed subject id names.
    pub fn legacy_id_of(&self, subject_id: aseman_domain::Uuid) -> PortResult<Option<String>> {
        self.ports().creature_legacy_id(subject_id)
    }

    fn ports(&self) -> CapsuleCreaturePorts<'_> {
        CapsuleCreaturePorts {
            repository: self.trx,
            currency: BALANCE_CURRENCY,
            scale: BALANCE_SCALE,
        }
    }
}

impl CreatureDirectory for CreaturePorts<'_> {
    fn creature(&self, creature_id: &str) -> PortResult<Option<CreatureRecord>> {
        self.ports().creature(creature_id)
    }

    fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>> {
        self.ports().creature_id_by_username(username)
    }

    fn find_by_username_fragment(&self, fragment: &str) -> PortResult<Option<CreatureRecord>> {
        self.ports().find_by_username_fragment(fragment)
    }

    fn creatures(
        &self,
        creature_type: Option<&str>,
        offset: i64,
        count: Option<i64>,
    ) -> PortResult<Vec<CreatureRecord>> {
        self.ports().creatures(creature_type, offset, count)
    }

    fn create(&self, record: &CreatureRecord) -> PortResult<()> {
        self.ports().create(record)
    }

    fn update(&self, record: &CreatureRecord) -> PortResult<()> {
        self.ports().update(record)
    }

    fn delete(&self, creature_id: &str) -> PortResult<()> {
        self.ports().delete(creature_id)
    }
}

impl CreatureMetadata for CreaturePorts<'_> {
    fn metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        self.ports().metadata(kind, creature_id, path)
    }

    fn replace_metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        document: &str,
    ) -> PortResult<()> {
        self.ports().replace_metadata(kind, creature_id, document)
    }

    fn delete_metadata(&self, kind: MetadataKind, creature_id: &str) -> PortResult<()> {
        self.ports().delete_metadata(kind, creature_id)
    }
}

impl CreatureTypes for CreaturePorts<'_> {
    fn creature_type(&self, name: &str) -> PortResult<Option<String>> {
        self.ports().creature_type(name)
    }

    fn creature_types(&self) -> PortResult<Vec<(String, String)>> {
        self.ports().creature_types()
    }

    fn put_creature_type(&self, name: &str, spec: &str) -> PortResult<()> {
        self.ports().put_creature_type(name, spec)
    }
}

impl CreatureBalances for CreaturePorts<'_> {
    fn open(&self, creature_id: &str, opening_balance: i64) -> PortResult<()> {
        self.ports().open(creature_id, opening_balance)
    }

    fn close(&self, creature_id: &str) -> PortResult<()> {
        self.ports().close(creature_id)
    }

    fn balance(&self, creature_id: &str) -> PortResult<i64> {
        self.ports().balance(creature_id)
    }

    fn set_balance(&self, creature_id: &str, balance: i64) -> PortResult<()> {
        self.ports().set_balance(creature_id, balance)
    }
}

/// A creature's balance as finance code reads and writes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Account {
    pub id: String,
    pub balance: i64,
}

impl CreaturePorts<'_> {
    /// The creature's balance account, or `None` when the creature is absent.
    pub fn account(&self, creature_id: &str) -> anyhow::Result<Option<Account>> {
        match self.balance(creature_id) {
            Ok(balance) => Ok(Some(Account {
                id: creature_id.to_owned(),
                balance,
            })),
            Err(PortError::NotFound) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }

    /// The creature as the wire expects it: a missing creature reads
    /// as an empty record carrying the requested id.
    pub fn creature_or_empty(&self, creature_id: &str) -> Creature {
        let found = self.creature(creature_id).ok().flatten();
        match found {
            Some(record) => {
                let balance = self.balance(creature_id).unwrap_or_default();
                creature_view(record, balance)
            }
            None => Creature {
                id: creature_id.to_owned(),
                ..Default::default()
            },
        }
    }

    /// Like [`Self::account`], reading an absent creature as a zero balance, as
    /// the wire expects. Writing that account back fails (LD-13).
    pub fn account_or_empty(&self, creature_id: &str) -> anyhow::Result<Account> {
        Ok(self.account(creature_id)?.unwrap_or(Account {
            id: creature_id.to_owned(),
            balance: 0,
        }))
    }

    /// Write back only the balance.
    pub fn store_account(&self, account: &Account) -> anyhow::Result<()> {
        self.set_balance(&account.id, account.balance)
            .map_err(|error| anyhow::anyhow!("{error}"))
    }
}

impl CreaturePorts<'_> {
    /// The metadata object at `path`, as the wire expects it.
    pub fn metadata_object(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        path: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let text = self.metadata(kind, creature_id, path).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Replace the metadata with `document`. A non-object
    /// is ignored rather than rejected.
    pub fn replace_metadata_value(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        document: &serde_json::Value,
    ) -> PortResult<()> {
        if !document.is_object() {
            return Ok(());
        }
        let text = serde_json::to_string(document)
            .map_err(|error| PortError::Failed(error.to_string()))?;
        self.replace_metadata(kind, creature_id, &text)
    }
}