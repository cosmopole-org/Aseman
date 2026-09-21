//! Creature repositories on the capsule protocol (RL-004 strangler, target side).
//!
//! A creature is stored exactly as the A308 export writes it:
//! - `core.creature` holds the identity.
//! - `core.user` exists for humans only.
//! - `finance.wallet` holds the balance in the configured currency.
//! - `core.legacy_identity` rows map canonical IDs back to legacy identities.
//!
//! Every multi-capsule change is one `put_all` transaction.

use crate::store::{body, next_revision, now_micros, port_error};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, CapsuleRelationship,
    CapsuleValue, ComparisonOperator, MAX_QUERY_LIMIT, OwnerScope, QueryPredicate, QuerySort,
    SortDirection, StorageClass,
};
use aseman_contracts::legacy_documents::{
    capsule_value_to_json, legacy_document_fields, legacy_document_object_at,
};
use aseman_contracts::legacy_keys::{decode_legacy_rsa_public_key, encode_legacy_rsa_public_key};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::creature::{
    CreatureRecord, HUMAN_OWNER, METADATA_ROOT, MetadataKind, legacy_page,
};
use aseman_ports::{
    CreatureBalances, CreatureDirectory, CreatureMetadata, CreatureTypes, PortError, PortResult,
};
use std::collections::{BTreeMap, BTreeSet};

const CREATURE: &str = "core.creature";
const USER: &str = "core.user";
const WALLET: &str = "finance.wallet";
const LEGACY_IDENTITY: &str = "core.legacy_identity";
const MAX_CAS_ATTEMPTS: usize = 8;

/// Creature ports over any [`CapsuleStore`]; balances live in wallets of `currency`.
pub struct CapsuleCreaturePorts<'a> {
    pub repository: &'a dyn CapsuleStore,
    /// The installation's finance epoch currency (ADR 0017).
    pub currency: &'a str,
    pub scale: u8,
}

fn kind(name: &str) -> CapsuleKind {
    CapsuleKind(name.to_owned())
}

fn creature_id(legacy_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("Creature", legacy_id.as_bytes())
}

fn user_id(legacy_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("User", legacy_id.as_bytes())
}

fn text(body: &BTreeMap<String, CapsuleValue>, field: &str) -> String {
    match body.get(field) {
        Some(CapsuleValue::Text(value)) => value.clone(),
        _ => String::new(),
    }
}

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

/// A new revision-1 capsule, sealed.
fn new_capsule(
    id: [u8; 16],
    kind_name: &str,
    storage_class: StorageClass,
    owner_scope: OwnerScope,
    relationships: Vec<CapsuleRelationship>,
    fields: BTreeMap<String, CapsuleValue>,
) -> PortResult<CapsuleEnvelope> {
    let now = now_micros();
    CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(id),
        kind: kind(kind_name),
        storage_class,
        owner_scope,
        schema_version: 1,
        revision: 1,
        created_at_micros: now,
        updated_at_micros: now,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships,
        body: Some(CapsuleValue::Object(fields)),
    }
    .seal()
    .map_err(failed)
}

/// The tombstone revision of `current`.
fn tombstone(current: &CapsuleEnvelope) -> PortResult<CapsuleEnvelope> {
    CapsuleEnvelope {
        revision: current.revision + 1,
        previous_integrity: Some(current.integrity_hash.clone()),
        updated_at_micros: now_micros().max(current.updated_at_micros),
        tombstone: true,
        body: None,
        ..current.clone()
    }
    .seal()
    .map_err(failed)
}

fn relationship(name: &str, target_kind: &str, target: [u8; 16]) -> CapsuleRelationship {
    CapsuleRelationship {
        name: name.to_owned(),
        target_kind: kind(target_kind),
        target_id: CapsuleId(target),
    }
}

fn legacy_identity(
    family: &str,
    legacy_id: &str,
    target_kind: &str,
) -> PortResult<CapsuleEnvelope> {
    let target = deterministic_legacy_capsule_id(family, legacy_id.as_bytes());
    new_capsule(
        deterministic_legacy_capsule_id(
            "LegacyIdentity",
            format!("{family}\0{legacy_id}").as_bytes(),
        ),
        LEGACY_IDENTITY,
        StorageClass::Core,
        OwnerScope::Global,
        Vec::new(),
        BTreeMap::from([
            ("family".to_owned(), CapsuleValue::Text(family.to_owned())),
            (
                "legacy_id".to_owned(),
                CapsuleValue::Text(legacy_id.to_owned()),
            ),
            (
                "target_kind".to_owned(),
                CapsuleValue::Text(target_kind.to_owned()),
            ),
            ("target_id".to_owned(), CapsuleValue::Bytes(target.to_vec())),
        ]),
    )
}

fn equal(field: &str, value: CapsuleValue) -> QueryPredicate {
    QueryPredicate::Compare {
        field: field.to_owned(),
        operator: ComparisonOperator::Equal,
        value,
    }
}

impl CapsuleCreaturePorts<'_> {
    fn get(&self, kind_name: &str, id: [u8; 16]) -> PortResult<Option<CapsuleEnvelope>> {
        self.repository
            .get(&kind(kind_name), &CapsuleId(id))
            .map_err(port_error)
    }

    fn live(&self, kind_name: &str, id: [u8; 16]) -> PortResult<Option<CapsuleEnvelope>> {
        Ok(self
            .get(kind_name, id)?
            .filter(|capsule| !capsule.tombstone))
    }

    fn wallet_id(&self, legacy_id: &str) -> [u8; 16] {
        let mut source = legacy_id.as_bytes().to_vec();
        source.push(0);
        source.extend_from_slice(self.currency.as_bytes());
        deterministic_legacy_capsule_id("Wallet", &source)
    }

    /// Every live capsule of `kind_name` matching `predicates`, paged by keyset on
    /// the unique text field `key`. The provider's collation orders the pages; callers
    /// re-sort by bytes wherever legacy order matters.
    fn scan(
        &self,
        kind_name: &str,
        predicates: Vec<QueryPredicate>,
        key: &str,
    ) -> PortResult<Vec<CapsuleEnvelope>> {
        let mut rows = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut page_predicates = predicates.clone();
            if let Some(last) = &after {
                page_predicates.push(QueryPredicate::Compare {
                    field: key.to_owned(),
                    operator: ComparisonOperator::GreaterThan,
                    value: CapsuleValue::Text(last.clone()),
                });
            }
            let page = self
                .repository
                .query(&CapsuleQuery {
                    kind: kind(kind_name),
                    predicate: match page_predicates.len() {
                        0 => None,
                        1 => page_predicates.pop(),
                        _ => Some(QueryPredicate::And {
                            predicates: page_predicates,
                        }),
                    },
                    projection: BTreeSet::new(),
                    sort: vec![QuerySort {
                        field: key.to_owned(),
                        direction: SortDirection::Ascending,
                    }],
                    aggregates: Vec::new(),
                    traversals: Vec::new(),
                    limit: MAX_QUERY_LIMIT,
                    cursor: None,
                })
                .map_err(port_error)?;
            let full = page.len() == MAX_QUERY_LIMIT as usize;
            after = page
                .iter()
                .rev()
                .find_map(|capsule| body(capsule).map(|fields| text(fields, key)));
            rows.extend(page.into_iter().filter(|capsule| !capsule.tombstone));
            if !full || after.is_none() {
                return Ok(rows);
            }
        }
    }

    /// Canonical ID -> legacy identity, for every identity of `family`.
    fn legacy_ids(&self, family: &str) -> PortResult<BTreeMap<[u8; 16], String>> {
        let mut map = BTreeMap::new();
        for row in self.scan(
            LEGACY_IDENTITY,
            vec![equal("family", CapsuleValue::Text(family.to_owned()))],
            "legacy_id",
        )? {
            let Some(fields) = body(&row) else { continue };
            if let Some(CapsuleValue::Bytes(target)) = fields.get("target_id")
                && let Ok(target) = <[u8; 16]>::try_from(target.as_slice())
            {
                map.insert(target, text(fields, "legacy_id"));
            }
        }
        Ok(map)
    }

    /// The legacy identity of one canonical ID, by the unique target index.
    fn legacy_id_of(&self, target_kind: &str, target: [u8; 16]) -> PortResult<String> {
        let rows = self
            .repository
            .query(&CapsuleQuery {
                kind: kind(LEGACY_IDENTITY),
                predicate: Some(QueryPredicate::And {
                    predicates: vec![
                        equal("target_kind", CapsuleValue::Text(target_kind.to_owned())),
                        equal("target_id", CapsuleValue::Bytes(target.to_vec())),
                    ],
                }),
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: 1,
                cursor: None,
            })
            .map_err(port_error)?;
        match rows
            .first()
            .and_then(body)
            .map(|fields| text(fields, "legacy_id"))
        {
            Some(legacy_id) if !legacy_id.is_empty() => Ok(legacy_id),
            _ => Err(failed(format!("{target_kind} has no legacy identity"))),
        }
    }

    fn owner_legacy_id(&self, capsule: &CapsuleEnvelope, legacy_id: &str) -> PortResult<String> {
        let fields = body(capsule).ok_or(PortError::NotFound)?;
        if text(fields, "creature_type") == aseman_domain::creature::HUMAN_CREATURE_TYPE {
            return Ok(HUMAN_OWNER.to_owned());
        }
        let owner = capsule
            .relationships
            .iter()
            .find(|relationship| relationship.name == "owner")
            .ok_or_else(|| failed(format!("creature {legacy_id} has no owner")))?;
        self.legacy_id_of(USER, owner.target_id.0)
    }

    fn record(&self, capsule: &CapsuleEnvelope, legacy_id: &str) -> PortResult<CreatureRecord> {
        let fields = body(capsule).ok_or(PortError::NotFound)?;
        let public_key = match fields.get("public_key") {
            Some(CapsuleValue::Bytes(encoded)) => {
                decode_legacy_rsa_public_key(encoded).map_err(failed)?
            }
            _ => return Err(failed(format!("creature {legacy_id} has no public key"))),
        };
        Ok(CreatureRecord {
            id: legacy_id.to_owned(),
            creature_type: text(fields, "creature_type"),
            username: text(fields, "username"),
            public_key,
            chain_id: text(fields, "chain_id"),
            subchain_id: text(fields, "subchain_id"),
            owner_id: self.owner_legacy_id(capsule, legacy_id)?,
        })
    }

    fn creature_fields(record: &CreatureRecord) -> PortResult<BTreeMap<String, CapsuleValue>> {
        let public_key = encode_legacy_rsa_public_key(&record.public_key).map_err(failed)?;
        Ok(BTreeMap::from([
            (
                "username".to_owned(),
                CapsuleValue::Text(record.username.clone()),
            ),
            (
                "creature_type".to_owned(),
                CapsuleValue::Text(record.creature_type.clone()),
            ),
            ("public_key".to_owned(), CapsuleValue::Bytes(public_key)),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            (
                "chain_id".to_owned(),
                CapsuleValue::Text(record.chain_id.clone()),
            ),
            (
                "subchain_id".to_owned(),
                CapsuleValue::Text(record.subchain_id.clone()),
            ),
        ]))
    }

    /// The `core.user` that owns `record`: itself for a human, else the owner's. A
    /// machine owned by a non-human fails, as the A308 export does.
    fn owning_user(&self, record: &CreatureRecord) -> PortResult<[u8; 16]> {
        if record.is_human() {
            return Ok(user_id(&record.id));
        }
        let owner = user_id(&record.owner_id);
        if self.live(USER, owner)?.is_none() {
            return Err(failed(format!(
                "creature owner {} is not a human user",
                record.owner_id
            )));
        }
        Ok(owner)
    }

    fn user_fields(
        record: &CreatureRecord,
        email: Option<CapsuleValue>,
    ) -> PortResult<BTreeMap<String, CapsuleValue>> {
        let mut fields = BTreeMap::from([
            (
                "username".to_owned(),
                CapsuleValue::Text(record.username.clone()),
            ),
            (
                "public_key".to_owned(),
                CapsuleValue::Bytes(
                    encode_legacy_rsa_public_key(&record.public_key).map_err(failed)?,
                ),
            ),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
        ]);
        if let Some(email) = email {
            fields.insert("email".to_owned(), email);
        }
        Ok(fields)
    }

    fn conflict(error: CapsuleStoreError) -> PortError {
        port_error(error)
    }
}

impl CreatureDirectory for CapsuleCreaturePorts<'_> {
    fn creature(&self, legacy_id: &str) -> PortResult<Option<CreatureRecord>> {
        match self.live(CREATURE, creature_id(legacy_id))? {
            Some(capsule) => self.record(&capsule, legacy_id).map(Some),
            None => Ok(None),
        }
    }

    fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>> {
        let rows = self.scan(
            CREATURE,
            vec![equal("username", CapsuleValue::Text(username.to_owned()))],
            "username",
        )?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        self.legacy_id_of(CREATURE, row.id.0).map(Some)
    }

    fn find_by_username_fragment(&self, fragment: &str) -> PortResult<Option<CreatureRecord>> {
        let mut rows = self
            .scan(CREATURE, Vec::new(), "username")?
            .into_iter()
            .filter_map(|capsule| {
                let username = text(body(&capsule)?, "username");
                username.contains(fragment).then_some((username, capsule))
            })
            .collect::<Vec<_>>();
        // Legacy walks the username index in byte order.
        rows.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        let Some((_, capsule)) = rows.into_iter().next() else {
            return Ok(None);
        };
        let legacy_id = self.legacy_id_of(CREATURE, capsule.id.0)?;
        self.record(&capsule, &legacy_id).map(Some)
    }

    fn creatures(
        &self,
        creature_type: Option<&str>,
        offset: i64,
        count: Option<i64>,
    ) -> PortResult<Vec<CreatureRecord>> {
        let legacy = self.legacy_ids("Creature")?;
        let predicates = creature_type
            .map(|creature_type| {
                vec![equal(
                    "creature_type",
                    CapsuleValue::Text(creature_type.to_owned()),
                )]
            })
            .unwrap_or_default();
        let mut rows = Vec::new();
        for capsule in self.scan(CREATURE, predicates, "username")? {
            let legacy_id = legacy
                .get(&capsule.id.0)
                .cloned()
                .ok_or_else(|| failed("creature has no legacy identity"))?;
            rows.push((legacy_id, capsule));
        }
        // Legacy lists objects in identity byte order.
        rows.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        legacy_page(rows, offset, count)
            .into_iter()
            .map(|(legacy_id, capsule)| self.record(&capsule, &legacy_id))
            .collect()
    }

    fn create(&self, record: &CreatureRecord) -> PortResult<()> {
        let id = creature_id(&record.id);
        if self.get(CREATURE, id)?.is_some()
            || self.creature_id_by_username(&record.username)?.is_some()
        {
            return Err(PortError::Conflict);
        }
        let owner = self.owning_user(record)?;
        let mut writes = Vec::new();
        if record.is_human() {
            writes.push(new_capsule(
                owner,
                USER,
                StorageClass::Core,
                OwnerScope::Global,
                Vec::new(),
                Self::user_fields(record, None)?,
            )?);
            writes.push(legacy_identity("User", &record.id, USER)?);
        }
        writes.push(new_capsule(
            id,
            CREATURE,
            StorageClass::Core,
            OwnerScope::Global,
            vec![relationship("owner", USER, owner)],
            Self::creature_fields(record)?,
        )?);
        writes.push(legacy_identity("Creature", &record.id, CREATURE)?);
        let writes = writes
            .into_iter()
            .map(|capsule| (capsule, None))
            .collect::<Vec<_>>();
        self.repository.put_all(&writes).map_err(Self::conflict)
    }

    fn update(&self, record: &CreatureRecord) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self
                .live(CREATURE, creature_id(&record.id))?
                .ok_or(PortError::NotFound)?;
            if let Some(holder) = self.creature_id_by_username(&record.username)?
                && holder != record.id
            {
                return Err(PortError::Conflict);
            }
            let owner = self.owning_user(record)?;
            let mut writes = Vec::new();
            if record.is_human() {
                let user = self.get(USER, owner)?;
                match user.filter(|user| !user.tombstone) {
                    Some(user) => {
                        let email = body(&user).and_then(|fields| fields.get("email").cloned());
                        writes.push((
                            next_revision(&user, Self::user_fields(record, email)?)?,
                            Some(user.revision),
                        ));
                    }
                    None => {
                        // A creature retyped as human becomes its own user.
                        writes.push((
                            new_capsule(
                                owner,
                                USER,
                                StorageClass::Core,
                                OwnerScope::Global,
                                Vec::new(),
                                Self::user_fields(record, None)?,
                            )?,
                            None,
                        ));
                        writes.push((legacy_identity("User", &record.id, USER)?, None));
                    }
                }
            }
            let next = CapsuleEnvelope {
                relationships: vec![relationship("owner", USER, owner)],
                ..next_revision(&current, Self::creature_fields(record)?)?
            }
            .seal()
            .map_err(failed)?;
            writes.push((next, Some(current.revision)));
            match self.repository.put_all(&writes) {
                Err(CapsuleStoreError::Conflict) => {
                    // A lost race retries; a taken username stays a conflict.
                    if let Some(holder) = self.creature_id_by_username(&record.username)?
                        && holder != record.id
                    {
                        return Err(PortError::Conflict);
                    }
                }
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete(&self, legacy_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.live(CREATURE, creature_id(legacy_id))? else {
                return Ok(());
            };
            let mut writes = vec![(tombstone(&current)?, Some(current.revision))];
            let human = body(&current).is_some_and(|fields| {
                text(fields, "creature_type") == aseman_domain::creature::HUMAN_CREATURE_TYPE
            });
            if human && let Some(user) = self.live(USER, user_id(legacy_id))? {
                writes.push((tombstone(&user)?, Some(user.revision)));
            }
            match self.repository.put_all(&writes) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

impl CreatureBalances for CapsuleCreaturePorts<'_> {
    fn close(&self, legacy_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(wallet) = self.live(WALLET, self.wallet_id(legacy_id))? else {
                return Ok(());
            };
            match self
                .repository
                .put(&tombstone(&wallet)?, Some(wallet.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn open(&self, legacy_id: &str, opening_balance: i64) -> PortResult<()> {
        let id = creature_id(legacy_id);
        if self.live(CREATURE, id)?.is_none() {
            return Err(PortError::NotFound);
        }
        let wallet = new_capsule(
            self.wallet_id(legacy_id),
            WALLET,
            StorageClass::Finance,
            OwnerScope::Creature(id),
            vec![relationship("creature", CREATURE, id)],
            BTreeMap::from([
                (
                    "currency".to_owned(),
                    CapsuleValue::Text(self.currency.to_owned()),
                ),
                (
                    "balance_minor".to_owned(),
                    CapsuleValue::Integer(opening_balance),
                ),
                (
                    "scale".to_owned(),
                    CapsuleValue::Integer(i64::from(self.scale)),
                ),
                ("state".to_owned(), CapsuleValue::Text("active".to_owned())),
            ]),
        )?;
        self.repository.put(&wallet, None).map_err(port_error)
    }

    fn balance(&self, legacy_id: &str) -> PortResult<i64> {
        if self.live(CREATURE, creature_id(legacy_id))?.is_none() {
            return Err(PortError::NotFound);
        }
        let wallet = self
            .live(WALLET, self.wallet_id(legacy_id))?
            .ok_or(PortError::NotFound)?;
        match body(&wallet).and_then(|fields| fields.get("balance_minor")) {
            Some(CapsuleValue::Integer(balance)) => Ok(*balance),
            _ => Err(failed(format!("wallet of {legacy_id} has no balance"))),
        }
    }

    fn set_balance(&self, legacy_id: &str, balance: i64) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            if self.live(CREATURE, creature_id(legacy_id))?.is_none() {
                return Err(PortError::NotFound);
            }
            let wallet = self
                .live(WALLET, self.wallet_id(legacy_id))?
                .ok_or(PortError::NotFound)?;
            let mut fields = body(&wallet).cloned().ok_or(PortError::NotFound)?;
            fields.insert("balance_minor".to_owned(), CapsuleValue::Integer(balance));
            match self
                .repository
                .put(&next_revision(&wallet, fields)?, Some(wallet.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

/// The capsule kind and identity family of a creature metadata document (ADR 0016).
fn metadata_kind(kind: MetadataKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        MetadataKind::Creature => ("core.creature_metadata", "CreatureMetadata", "CreatMeta::"),
        MetadataKind::User => ("core.user_metadata", "UserMetadata", "UserMeta::"),
    }
}

impl CreatureMetadata for CapsuleCreaturePorts<'_> {
    fn metadata(
        &self,
        kind: MetadataKind,
        legacy_id: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        let (kind_name, family, _) = metadata_kind(kind);
        let id = deterministic_legacy_capsule_id(family, legacy_id.as_bytes());
        let Some(capsule) = self.live(kind_name, id)? else {
            return Ok(None);
        };
        let document = match body(&capsule)
            .and_then(|fields| fields.get("document"))
            .map(capsule_value_to_json)
            .transpose()
            .map_err(failed)?
        {
            Some(serde_json::Value::Object(document)) => document,
            _ => {
                return Err(failed(format!(
                    "{kind_name} of {legacy_id} has no document"
                )));
            }
        };
        legacy_document_object_at(METADATA_ROOT, &document, path)
            .map(|object| serde_json::to_string(object).map_err(failed))
            .transpose()
    }

    fn replace_metadata(
        &self,
        kind: MetadataKind,
        legacy_id: &str,
        document: &str,
    ) -> PortResult<()> {
        let Ok(serde_json::Value::Object(document)) = serde_json::from_str(document) else {
            return Err(failed("metadata must be a JSON object"));
        };
        let (kind_name, family, key_prefix) = metadata_kind(kind);
        let fields = legacy_document_fields(
            &format!("{key_prefix}{legacy_id}"),
            METADATA_ROOT,
            &document,
        )
        .map_err(failed)?;
        let id = deterministic_legacy_capsule_id(family, legacy_id.as_bytes());
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.get(kind_name, id)? {
                // A replaced or revived document is the next revision of its chain.
                Some(current) => {
                    let next = CapsuleEnvelope {
                        tombstone: false,
                        ..next_revision(&current, fields.clone())?
                    }
                    .seal()
                    .map_err(failed)?;
                    self.repository.put(&next, Some(current.revision))
                }
                None => {
                    let capsule = new_capsule(
                        id,
                        kind_name,
                        StorageClass::Core,
                        OwnerScope::Global,
                        vec![relationship("creature", CREATURE, creature_id(legacy_id))],
                        fields.clone(),
                    )?;
                    self.repository.put(&capsule, None)
                }
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete_metadata(&self, kind: MetadataKind, legacy_id: &str) -> PortResult<()> {
        let (kind_name, family, _) = metadata_kind(kind);
        let id = deterministic_legacy_capsule_id(family, legacy_id.as_bytes());
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.live(kind_name, id)? else {
                return Ok(());
            };
            match self
                .repository
                .put(&tombstone(&current)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

const CREATURE_TYPE: &str = "core.creature_type";

impl CapsuleCreaturePorts<'_> {
    /// The spec of a live type capsule as a JSON object.
    fn type_spec(
        capsule: &CapsuleEnvelope,
    ) -> PortResult<serde_json::Map<String, serde_json::Value>> {
        match body(capsule)
            .and_then(|fields| fields.get("document"))
            .map(capsule_value_to_json)
            .transpose()
            .map_err(failed)?
        {
            Some(serde_json::Value::Object(spec)) => Ok(spec),
            _ => Err(failed("creature type has no spec document")),
        }
    }
}

impl CreatureTypes for CapsuleCreaturePorts<'_> {
    fn creature_type(&self, name: &str) -> PortResult<Option<String>> {
        let id = deterministic_legacy_capsule_id("CreatureType", name.as_bytes());
        let Some(capsule) = self.live(CREATURE_TYPE, id)? else {
            return Ok(None);
        };
        let spec = Self::type_spec(&capsule)?;
        if spec.is_empty() {
            return Ok(None);
        }
        serde_json::to_string(&spec).map(Some).map_err(failed)
    }

    fn creature_types(&self) -> PortResult<Vec<(String, String)>> {
        let mut types = Vec::new();
        for capsule in self.scan(CREATURE_TYPE, Vec::new(), "type_name")? {
            let spec = Self::type_spec(&capsule)?;
            if spec.is_empty() {
                continue;
            }
            let name = body(&capsule)
                .map(|fields| text(fields, "type_name"))
                .unwrap_or_default();
            types.push((name, serde_json::to_string(&spec).map_err(failed)?));
        }
        // Legacy lists the registry in flag-key byte order.
        types.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        Ok(types)
    }

    fn put_creature_type(&self, name: &str, spec: &str) -> PortResult<()> {
        let Ok(serde_json::Value::Object(spec)) = serde_json::from_str(spec) else {
            return Err(failed("a creature type spec must be a JSON object"));
        };
        let mut fields =
            legacy_document_fields(&format!("Json::CreatureType::{name}"), "spec", &spec)
                .map_err(failed)?;
        fields.insert("type_name".to_owned(), CapsuleValue::Text(name.to_owned()));
        let id = deterministic_legacy_capsule_id("CreatureType", name.as_bytes());
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.get(CREATURE_TYPE, id)? {
                Some(current) => {
                    let next = CapsuleEnvelope {
                        tombstone: false,
                        ..next_revision(&current, fields.clone())?
                    }
                    .seal()
                    .map_err(failed)?;
                    self.repository.put(&next, Some(current.revision))
                }
                None => self.repository.put(
                    &new_capsule(
                        id,
                        CREATURE_TYPE,
                        StorageClass::Core,
                        OwnerScope::Global,
                        Vec::new(),
                        fields.clone(),
                    )?,
                    None,
                ),
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}
