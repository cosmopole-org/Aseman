//! Legacy adapters for the creature identity use cases: the node transaction behind
//! [`CreatureDirectory`] and [`CreatureBalances`] (RL-004 strangler). Key encodings
//! are exactly the legacy `Creature` object, its `username` index, and its `balance`
//! column.

use std::collections::HashMap;

use aseman_domain::creature::{legacy_page, CreatureRecord, MetadataKind, METADATA_ROOT};
use aseman_ports::{
    CreatureBalances, CreatureDirectory, CreatureMetadata, CreatureTypes, PortError, PortResult,
};

use crate::models::transaction::ITrx;
use crate::shell::api::model::Creature;

pub(crate) struct LegacyCreatures<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

fn record(creature: Creature) -> CreatureRecord {
    CreatureRecord {
        id: creature.id,
        creature_type: creature.type_name,
        username: creature.username,
        public_key: creature.public_key,
        chain_id: creature.chain_id,
        subchain_id: creature.subchain_id,
        owner_id: creature.owner_id,
    }
}

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

fn identity_columns(record: &CreatureRecord) -> HashMap<String, Vec<u8>> {
    HashMap::from([
        ("type".to_owned(), record.creature_type.as_bytes().to_vec()),
        ("username".to_owned(), record.username.as_bytes().to_vec()),
        (
            "publicKey".to_owned(),
            record.public_key.as_bytes().to_vec(),
        ),
        ("chainId".to_owned(), record.chain_id.as_bytes().to_vec()),
        (
            "subchainId".to_owned(),
            record.subchain_id.as_bytes().to_vec(),
        ),
        ("ownerId".to_owned(), record.owner_id.as_bytes().to_vec()),
    ])
}

impl LegacyCreatures<'_> {
    fn exists(&self, creature_id: &str) -> bool {
        self.trx.has_obj(Creature::type_(), creature_id)
    }

    /// The derived `ownerof` link of a record, if it has one: a non-human creature
    /// with an owner (the A308 export verifies exactly this).
    fn owner_link(record: &CreatureRecord) -> Option<(&str, &str)> {
        (!record.is_human() && !record.owner_id.is_empty())
            .then_some((record.owner_id.as_str(), record.id.as_str()))
    }

    fn put_owner_link(&self, (owner, creature): (&str, &str)) {
        self.trx
            .put_link(&format!("ownerof::{owner}::{creature}"), "true");
    }

    fn delete_owner_link(&self, (owner, creature): (&str, &str)) {
        self.trx
            .del_key(&format!("link::ownerof::{owner}::{creature}"));
    }

    fn put_username(&self, username: &str, creature_id: &str) {
        if !username.is_empty() {
            self.trx.put_index(
                Creature::type_(),
                "username",
                "id",
                username,
                creature_id.as_bytes().to_vec(),
            );
        }
    }
}

impl CreatureDirectory for LegacyCreatures<'_> {
    fn creature(&self, creature_id: &str) -> PortResult<Option<CreatureRecord>> {
        if !self.exists(creature_id) {
            return Ok(None);
        }
        Ok(Some(record(
            Creature {
                id: creature_id.to_owned(),
                ..Default::default()
            }
            .pull(self.trx),
        )))
    }

    fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>> {
        let id = self
            .trx
            .get_index(Creature::type_(), "username", "id", username);
        Ok((!id.is_empty()).then_some(id))
    }

    fn find_by_username_fragment(&self, fragment: &str) -> PortResult<Option<CreatureRecord>> {
        let found = Creature::search(self.trx, 0, 1, "username", fragment, &HashMap::new())
            .map_err(|error| PortError::Failed(error.to_string()))?;
        Ok(found.into_iter().next().map(record))
    }

    fn creatures(
        &self,
        creature_type: Option<&str>,
        offset: i64,
        count: Option<i64>,
    ) -> PortResult<Vec<CreatureRecord>> {
        let filter = creature_type
            .map(|creature_type| HashMap::from([("type".to_owned(), creature_type.to_owned())]))
            .unwrap_or_default();
        // The full filtered list in identity order, then the shared legacy window.
        let mut all = self
            .trx
            .get_obj_list(Creature::type_(), &["*".to_owned()], &filter, &[])
            .map_err(|error| PortError::Failed(error.to_string()))?
            .into_iter()
            .map(|(id, columns)| Creature::from_columns(id, &columns))
            .collect::<Vec<_>>();
        all.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(legacy_page(all.into_iter().map(record), offset, count))
    }

    fn create(&self, record: &CreatureRecord) -> PortResult<()> {
        if self.exists(&record.id) || self.creature_id_by_username(&record.username)?.is_some() {
            return Err(PortError::Conflict);
        }
        self.trx
            .put_obj(Creature::type_(), &record.id, identity_columns(record));
        self.put_username(&record.username, &record.id);
        if let Some(link) = Self::owner_link(record) {
            self.put_owner_link(link);
        }
        Ok(())
    }

    fn update(&self, record: &CreatureRecord) -> PortResult<()> {
        let current = self.creature(&record.id)?.ok_or(PortError::NotFound)?;
        if current.username != record.username {
            if self.creature_id_by_username(&record.username)?.is_some() {
                return Err(PortError::Conflict);
            }
            if !current.username.is_empty() {
                self.trx
                    .del_index(Creature::type_(), "username", "id", &current.username);
            }
        }
        self.trx
            .put_obj(Creature::type_(), &record.id, identity_columns(record));
        self.put_username(&record.username, &record.id);
        // LD-16: the owner link follows the owner and the type.
        let (old_link, new_link) = (Self::owner_link(&current), Self::owner_link(record));
        if old_link != new_link {
            if let Some(link) = old_link {
                self.delete_owner_link(link);
            }
            if let Some(link) = new_link {
                self.put_owner_link(link);
            }
        }
        Ok(())
    }

    fn delete(&self, creature_id: &str) -> PortResult<()> {
        if let Some(current) = self.creature(creature_id)? {
            // LD-16: a deleted creature takes its owner link with it.
            if let Some(link) = Self::owner_link(&current) {
                self.delete_owner_link(link);
            }
            Creature {
                id: current.id,
                username: current.username,
                ..Default::default()
            }
            .delete(self.trx);
        }
        Ok(())
    }
}

/// A creature's balance as finance code reads and writes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Account {
    pub(crate) id: String,
    pub(crate) balance: i64,
}

impl LegacyCreatures<'_> {
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

/// The legacy document key of a creature's metadata.
fn metadata_key(kind: MetadataKind, creature_id: &str) -> String {
    match kind {
        MetadataKind::Creature => format!("CreatMeta::{creature_id}"),
        MetadataKind::User => format!("UserMeta::{creature_id}"),
    }
}

impl LegacyCreatures<'_> {
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

impl CreatureMetadata for LegacyCreatures<'_> {
    fn metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        match self.trx.get_json(&metadata_key(kind, creature_id), path) {
            Ok(object) => serde_json::to_string(&object)
                .map(Some)
                .map_err(|error| PortError::Failed(error.to_string())),
            Err(_) => Ok(None),
        }
    }

    fn replace_metadata(
        &self,
        kind: MetadataKind,
        creature_id: &str,
        document: &str,
    ) -> PortResult<()> {
        let document = match serde_json::from_str::<serde_json::Value>(document) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => {
                return Err(PortError::Failed(
                    "metadata must be a JSON object".to_owned(),
                ))
            }
        };
        let key = metadata_key(kind, creature_id);
        // A non-merge `put_json` keeps old child splats, so clear the tree first.
        self.trx.del_json(&key, METADATA_ROOT);
        self.trx
            .put_json(&key, METADATA_ROOT, &document, false)
            .map_err(|error| PortError::Failed(error.to_string()))
    }

    fn delete_metadata(&self, kind: MetadataKind, creature_id: &str) -> PortResult<()> {
        self.trx
            .del_json(&metadata_key(kind, creature_id), METADATA_ROOT);
        Ok(())
    }
}

const CREATURE_TYPE_FLAG: &str = "CreatureTypeExists::";

fn creature_type_key(name: &str) -> String {
    format!("Json::CreatureType::{name}")
}

impl CreatureTypes for LegacyCreatures<'_> {
    fn creature_type(&self, name: &str) -> PortResult<Option<String>> {
        match self.trx.get_json(&creature_type_key(name), "spec") {
            Ok(spec) if !spec.is_empty() => serde_json::to_string(&spec)
                .map(Some)
                .map_err(|error| PortError::Failed(error.to_string())),
            _ => Ok(None),
        }
    }

    fn creature_types(&self) -> PortResult<Vec<(String, String)>> {
        let mut types = Vec::new();
        for link in self
            .trx
            .get_links_list(CREATURE_TYPE_FLAG, -1, -1, &[])
            .unwrap_or_default()
        {
            let name = link.strip_prefix(CREATURE_TYPE_FLAG).unwrap_or(&link);
            if let Some(spec) = self.creature_type(name)? {
                types.push((name.to_owned(), spec));
            }
        }
        Ok(types)
    }

    fn put_creature_type(&self, name: &str, spec: &str) -> PortResult<()> {
        let spec = match serde_json::from_str::<serde_json::Value>(spec) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => {
                return Err(PortError::Failed(
                    "a creature type spec must be a JSON object".to_owned(),
                ))
            }
        };
        let key = creature_type_key(name);
        // A non-merge `put_json` keeps old child splats, so clear the tree first.
        self.trx.del_json(&key, "spec");
        self.trx
            .put_json(&key, "spec", &spec, false)
            .map_err(|error| PortError::Failed(error.to_string()))?;
        self.trx
            .put_link(&format!("{CREATURE_TYPE_FLAG}{name}"), "true");
        Ok(())
    }
}

impl CreatureBalances for LegacyCreatures<'_> {
    fn close(&self, creature_id: &str) -> PortResult<()> {
        // The legacy record holds the balance as one of its columns.
        self.trx.del_key(&format!(
            "obj::{}::{creature_id}::balance",
            Creature::type_()
        ));
        Ok(())
    }

    fn open(&self, creature_id: &str, opening_balance: i64) -> PortResult<()> {
        if !self.exists(creature_id) {
            return Err(PortError::NotFound);
        }
        if self
            .trx
            .get_obj(Creature::type_(), creature_id)
            .contains_key("balance")
        {
            return Err(PortError::Conflict);
        }
        self.set_balance(creature_id, opening_balance)
    }

    fn balance(&self, creature_id: &str) -> PortResult<i64> {
        if !self.exists(creature_id) {
            return Err(PortError::NotFound);
        }
        Ok(Creature {
            id: creature_id.to_owned(),
            ..Default::default()
        }
        .pull(self.trx)
        .balance)
    }

    fn set_balance(&self, creature_id: &str, balance: i64) -> PortResult<()> {
        if !self.exists(creature_id) {
            return Err(PortError::NotFound);
        }
        self.trx.put_obj(
            Creature::type_(),
            creature_id,
            HashMap::from([(
                "balance".to_owned(),
                (balance as u64).to_le_bytes().to_vec(),
            )]),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::core::actor::model::trx::TrxWrapper;
    use crate::models::ports::storage::IStorage;
    use std::sync::Arc;

    pub(crate) fn test_public_keys() -> [String; 4] {
        use rsa::pkcs8::{EncodePublicKey, LineEnding};
        use rsa::rand_core::OsRng;
        [0, 1, 2, 3].map(|_| {
            rsa::RsaPublicKey::from(&rsa::RsaPrivateKey::new(&mut OsRng, 1024).unwrap())
                .to_public_key_pem(LineEnding::LF)
                .unwrap()
        })
    }

    #[test]
    fn legacy_creatures_pass_the_directory_conformance_suite() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let creatures = LegacyCreatures { trx: &*trx };
        let keys = test_public_keys();
        aseman_ports::conformance::creature_directory(
            &creatures,
            &creatures,
            [&keys[0], &keys[1], &keys[2]],
        );
        aseman_ports::conformance::creature_metadata(&creatures, &creatures, &keys[3]);
        aseman_ports::conformance::creature_types(&creatures);
        // The legacy encodings are unchanged.
        let alice = Creature {
            id: "1@conformance".into(),
            ..Default::default()
        }
        .pull(&*trx);
        assert_eq!(alice.balance, 10);
        assert_eq!(
            trx.get_index("Creature", "username", "id", "alice@conformance"),
            "1@conformance"
        );
    }

    /// LD-16: the derived `ownerof` link follows create, owner and type changes, and
    /// delete, exactly as the A308 export verifies it.
    #[test]
    fn owner_links_follow_the_record() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let creatures = LegacyCreatures { trx: &*trx };
        let link = |owner: &str| trx.get_link(&format!("ownerof::{owner}::3@global"));
        let mut machine = CreatureRecord {
            id: "3@global".into(),
            creature_type: "machine".into(),
            username: "bot@global".into(),
            public_key: "pem".into(),
            chain_id: "main".into(),
            subchain_id: "main".into(),
            owner_id: "1@global".into(),
        };
        creatures.create(&machine).unwrap();
        assert_eq!(link("1@global"), "true");
        machine.owner_id = "2@global".into();
        creatures.update(&machine).unwrap();
        assert_eq!(
            (link("1@global").as_str(), link("2@global").as_str()),
            ("", "true")
        );
        machine.creature_type = "human".into();
        creatures.update(&machine).unwrap();
        assert_eq!(link("2@global"), "");
        machine.creature_type = "machine".into();
        creatures.update(&machine).unwrap();
        assert_eq!(link("2@global"), "true");
        creatures.delete(&machine.id).unwrap();
        assert_eq!(link("2@global"), "");
    }

    /// Finance reads and writes balances through `Account`. A missing creature is
    /// absent (LD-13), and a write-back changes only the `balance` column.
    #[test]
    fn accounts_write_only_the_balance_and_never_create_ghost_creatures() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let creatures = LegacyCreatures { trx: &*trx };
        assert_eq!(creatures.account("9@global").unwrap(), None);
        let ghost = creatures.account_or_empty("9@global").unwrap();
        assert_eq!(ghost.balance, 0);
        assert!(creatures.store_account(&ghost).is_err());
        assert!(!trx.has_obj("Creature", "9@global"));

        Creature {
            id: "2@global".into(),
            type_name: "human".into(),
            username: "alice@global".into(),
            public_key: "pem".into(),
            balance: 5,
            ..Default::default()
        }
        .push(&*trx);
        let mut account = creatures.account("2@global").unwrap().unwrap();
        assert_eq!(account.balance, 5);
        account.balance = 12;
        creatures.store_account(&account).unwrap();
        let stored = Creature {
            id: "2@global".into(),
            ..Default::default()
        }
        .pull(&*trx);
        assert_eq!(
            (stored.balance, stored.username.as_str()),
            (12, "alice@global")
        );
    }
}
