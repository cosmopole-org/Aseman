//! The creature ports of one state action (ADR 0036): the creature, user, wallet,
//! metadata, and type-registry models, through the capsule repositories over the
//! action's transaction.

use aseman_capsule::creature::CapsuleCreaturePorts;
use aseman_domain::creature::{CreatureRecord, MetadataKind};
use aseman_ports::{
    CreatureBalances, CreatureDirectory, CreatureMetadata, CreatureTypes, PortError, PortResult,
};

use crate::api::model::Creature;
use crate::core::trx::{BALANCE_CURRENCY, BALANCE_SCALE, Trx};

/// The legacy wire shape of a creature: its identity plus its balance.
pub(crate) fn creature_view(record: CreatureRecord, balance: i64) -> Creature {
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

/// The creature ports of one state action.
pub(crate) struct CreaturePorts<'a> {
    pub(crate) trx: &'a Trx,
}

impl CreaturePorts<'_> {
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
pub(crate) struct Account {
    pub(crate) id: String,
    pub(crate) balance: i64,
}

impl CreaturePorts<'_> {
    /// The creature's balance account, or `None` when the creature is absent.
    pub(crate) fn account(&self, creature_id: &str) -> anyhow::Result<Option<Account>> {
        match self.balance(creature_id) {
            Ok(balance) => Ok(Some(Account {
                id: creature_id.to_owned(),
                balance,
            })),
            Err(PortError::NotFound) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }

    /// The creature as legacy `Creature::pull` returned it: a missing creature reads
    /// as an empty record carrying the requested id.
    pub(crate) fn creature_or_empty(&self, creature_id: &str) -> Creature {
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
    /// legacy `Creature::pull` did. Writing that account back fails (LD-13).
    pub(crate) fn account_or_empty(&self, creature_id: &str) -> anyhow::Result<Account> {
        Ok(self.account(creature_id)?.unwrap_or(Account {
            id: creature_id.to_owned(),
            balance: 0,
        }))
    }

    /// Write back only the balance. The legacy path re-pushed the whole record.
    pub(crate) fn store_account(&self, account: &Account) -> anyhow::Result<()> {
        self.set_balance(&account.id, account.balance)
            .map_err(|error| anyhow::anyhow!("{error}"))
    }
}

impl CreaturePorts<'_> {
    /// The metadata object at `path`, as legacy `get_json(..).ok()` returned it.
    pub(crate) fn metadata_object(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        path: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let text = self.metadata(kind, creature_id, path).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Replace the metadata with `document`. As legacy `put_json` did, a non-object
    /// is ignored rather than rejected.
    pub(crate) fn replace_metadata_value(
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
