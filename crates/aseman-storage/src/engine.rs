//! The Prisma-style transaction engine (ADR 0036), built once over any provider.

use crate::codec;
use crate::error::{StorageError, StorageResult};
use crate::provider::{CapsuleTransaction, Mode, ProviderSettings, Registry, StorageProvider};
use crate::query::{FindMany, Where};
use crate::schema::{Model, Schema};
use crate::value::{Data, Id, Row, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Rows one engine step reads while walking a `*_many` operation.
const PAGE: u64 = 1_000;

/// An opened storage: one provider and the model catalog.
#[derive(Clone)]
pub struct Storage {
    provider: Arc<dyn StorageProvider>,
    schema: Arc<Schema>,
}

impl Storage {
    #[must_use]
    pub fn new(provider: Arc<dyn StorageProvider>, schema: Arc<Schema>) -> Self {
        Self { provider, schema }
    }

    /// Open the plugin `name` from `registry`.
    pub fn open(
        registry: &Registry,
        name: &str,
        settings: &ProviderSettings,
    ) -> StorageResult<Self> {
        let provider = registry.open(name, settings)?;
        if let Some(layout) = provider.legacy_layout()? {
            return Err(StorageError::Unsupported(format!(
                "the {name} store holds data in the {layout} layout; convert it with \
                 `asemanctl storage migrate` before starting the node"
            )));
        }
        Ok(Self::new(provider, settings.schema.clone()))
    }

    pub fn begin(&self, mode: Mode) -> StorageResult<Trx> {
        Ok(Trx {
            inner: self.provider.begin(mode)?,
            schema: self.schema.clone(),
            mode,
            finished: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn provider(&self) -> &Arc<dyn StorageProvider> {
        &self.provider
    }

    #[must_use]
    pub fn schema(&self) -> &Arc<Schema> {
        &self.schema
    }
}

/// One transaction. Dropping it without [`Trx::commit`] rolls it back.
pub struct Trx {
    inner: Box<dyn CapsuleTransaction>,
    schema: Arc<Schema>,
    mode: Mode,
    finished: AtomicBool,
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}

impl Trx {
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.mode == Mode::ReadOnly
    }

    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    fn model(&self, name: &str) -> StorageResult<&Model> {
        self.schema.model(name)
    }

    fn writable(&self, model: &Model) -> StorageResult<()> {
        if self.read_only() {
            return Err(StorageError::invalid(format!(
                "{}: a read-only transaction cannot write",
                model.name
            )));
        }
        Ok(())
    }

    fn check_filter(model: &Model, filter: Option<&Where>) -> StorageResult<()> {
        let mut names = Vec::new();
        if let Some(filter) = filter {
            crate::eval::fields(filter, &mut names);
        }
        if let Some(unknown) = names.iter().find(|name| **name != "id" && !model.has(name)) {
            return Err(StorageError::invalid(format!(
                "{}: unknown field {unknown}",
                model.name
            )));
        }
        Ok(())
    }

    /// The id a unique selector names, when it names one without a query.
    fn direct_id(model: &Model, by: &crate::query::Unique) -> StorageResult<Option<Id>> {
        use crate::query::Unique;
        Ok(match by {
            Unique::Id(id) => Some(*id),
            Unique::Key(key) => {
                let family = model.key_family.as_deref().ok_or_else(|| {
                    StorageError::invalid(format!("{} has no natural key", model.name))
                })?;
                Some(Id::for_key(family, key))
            }
            Unique::Fields(_) => None,
        })
    }

    fn live(&self, model: &Model, id: Id) -> StorageResult<Option<Row>> {
        match self.inner.get(model, id)? {
            Some(capsule) if !capsule.tombstone => codec::decode(model, &capsule).map(Some),
            _ => Ok(None),
        }
    }

    /// The record `by` names.
    pub fn find_unique(
        &self,
        model: &str,
        by: &crate::query::Unique,
    ) -> StorageResult<Option<Row>> {
        let model = self.model(model)?;
        if let Some(id) = Self::direct_id(model, by)? {
            return self.live(model, id);
        }
        let crate::query::Unique::Fields(fields) = by else {
            return Ok(None);
        };
        let names = fields.keys().cloned().collect::<Vec<_>>();
        if !model.unique.iter().any(|index| {
            let mut index = index.clone();
            index.sort();
            index == names
        }) {
            return Err(StorageError::invalid(format!(
                "{}: ({}) is not a unique index",
                model.name,
                names.join(", ")
            )));
        }
        let filter = Where::all(
            fields
                .iter()
                .map(|(name, value)| Where::eq(name, value.clone()))
                .collect(),
        );
        self.inner
            .find(
                model,
                &FindMany {
                    filter,
                    take: Some(1),
                    ..FindMany::default()
                },
            )?
            .first()
            .map(|capsule| codec::decode(model, capsule))
            .transpose()
    }

    pub fn find_many(&self, model: &str, query: &FindMany) -> StorageResult<Vec<Row>> {
        let model = self.model(model)?;
        Self::check_filter(model, query.filter.as_ref())?;
        if let Some(order) = query.order_by.iter().find(|order| !model.has(&order.field)) {
            return Err(StorageError::invalid(format!(
                "{}: cannot order by unknown field {}",
                model.name, order.field
            )));
        }
        self.inner
            .find(model, query)?
            .iter()
            .map(|capsule| codec::decode(model, capsule))
            .collect()
    }

    pub fn find_first(&self, model: &str, query: &FindMany) -> StorageResult<Option<Row>> {
        let mut query = query.clone();
        query.take = Some(1);
        Ok(self.find_many(model, &query)?.into_iter().next())
    }

    pub fn count(&self, model: &str, filter: Option<&Where>) -> StorageResult<u64> {
        let model = self.model(model)?;
        Self::check_filter(model, filter)?;
        self.inner.count(model, filter)
    }

    pub fn exists(&self, model: &str, filter: &Where) -> StorageResult<bool> {
        Ok(self
            .find_first(model, &FindMany::filter(filter.clone()))?
            .is_some())
    }

    /// Create a record. A keyed model takes its id from `data["key"]`.
    pub fn create(&self, model: &str, data: Data) -> StorageResult<Row> {
        let model = self.model(model)?;
        self.writable(model)?;
        codec::validate(model, &data, true)?;
        let id = match &model.key_family {
            Some(family) => {
                let key = data.get("key").and_then(Value::as_text).ok_or_else(|| {
                    StorageError::invalid(format!("{}: a keyed record needs `key`", model.name))
                })?;
                Id::for_key(family, key)
            }
            None => Id::generate(),
        };
        let previous = self.inner.get(model, id)?;
        if previous
            .as_ref()
            .is_some_and(|previous| !previous.tombstone)
        {
            return Err(StorageError::conflict(format!(
                "{}: record {id} exists",
                model.name
            )));
        }
        let capsule = codec::encode(model, id, previous.as_ref(), now_micros(), &data)?;
        self.inner.put(
            model,
            &capsule,
            previous.as_ref().map(|previous| previous.revision),
        )?;
        codec::decode(model, &capsule)
    }

    /// Merge `data` into the record `by` names; a `Null` value clears a field.
    /// `None` when there is no such record.
    pub fn update(
        &self,
        model: &str,
        by: &crate::query::Unique,
        data: Data,
    ) -> StorageResult<Option<Row>> {
        let Some(current) = self.find_unique(model, by)? else {
            return Ok(None);
        };
        self.rewrite(model, current, data).map(Some)
    }

    fn rewrite(&self, model: &str, current: Row, data: Data) -> StorageResult<Row> {
        let model = self.model(model)?;
        self.writable(model)?;
        if model.append_only {
            return Err(StorageError::invalid(format!(
                "{} is append-only",
                model.name
            )));
        }
        codec::validate(model, &data, false)?;
        if let (Some(Value::Text(new)), Some(Value::Text(old))) =
            (data.get("key"), current.data.get("key"))
            && new != old
        {
            return Err(StorageError::invalid(format!(
                "{}: a record's key never changes",
                model.name
            )));
        }
        let mut merged = current.data;
        for (name, value) in data {
            if value.is_null() {
                merged.remove(&name);
            } else {
                merged.insert(name, value);
            }
        }
        codec::validate(model, &merged, true)?;
        let previous = self
            .inner
            .get(model, current.id)?
            .ok_or_else(|| StorageError::NotFound(current.id.to_string()))?;
        let capsule = codec::encode(model, current.id, Some(&previous), now_micros(), &merged)?;
        self.inner.put(model, &capsule, Some(previous.revision))?;
        codec::decode(model, &capsule)
    }

    /// Update the record `by` names, or create it from `create` (plus the selector's
    /// fields) when there is none.
    pub fn upsert(
        &self,
        model: &str,
        by: &crate::query::Unique,
        create: Data,
        update: Data,
    ) -> StorageResult<Row> {
        if let Some(current) = self.find_unique(model, by)? {
            return self.rewrite(model, current, update);
        }
        let mut create = create;
        match by {
            crate::query::Unique::Fields(fields) => {
                for (name, value) in fields {
                    create.entry(name.clone()).or_insert_with(|| value.clone());
                }
            }
            crate::query::Unique::Key(key) => {
                create
                    .entry("key".to_owned())
                    .or_insert_with(|| Value::Text(key.clone()));
            }
            crate::query::Unique::Id(_) => {
                return Err(StorageError::invalid(
                    "upsert by id cannot create: ids are assigned by the model",
                ));
            }
        }
        self.create(model, create)
    }

    /// Delete the record `by` names; `None` when there is none.
    pub fn delete(&self, model: &str, by: &crate::query::Unique) -> StorageResult<Option<Row>> {
        let Some(current) = self.find_unique(model, by)? else {
            return Ok(None);
        };
        let model = self.model(model)?;
        self.writable(model)?;
        if model.append_only {
            return Err(StorageError::invalid(format!(
                "{} is append-only",
                model.name
            )));
        }
        let previous = self
            .inner
            .get(model, current.id)?
            .ok_or_else(|| StorageError::NotFound(current.id.to_string()))?;
        let capsule = codec::tombstone(model, &previous, now_micros())?;
        self.inner.put(model, &capsule, Some(previous.revision))?;
        Ok(Some(current))
    }

    /// Every record `filter` matches, in id order (the walk behind the `*_many`
    /// writes).
    fn all(&self, model: &str, filter: Option<&Where>) -> StorageResult<Vec<Row>> {
        let mut rows = Vec::new();
        loop {
            let page = self.find_many(
                model,
                &FindMany {
                    filter: filter.cloned(),
                    order_by: Vec::new(),
                    skip: rows.len() as u64,
                    take: Some(PAGE),
                },
            )?;
            let done = (page.len() as u64) < PAGE;
            rows.extend(page);
            if done {
                return Ok(rows);
            }
        }
    }

    pub fn update_many(
        &self,
        model: &str,
        filter: Option<&Where>,
        data: Data,
    ) -> StorageResult<u64> {
        let rows = self.all(model, filter)?;
        let count = rows.len() as u64;
        for row in rows {
            self.rewrite(model, row, data.clone())?;
        }
        Ok(count)
    }

    pub fn delete_many(&self, model: &str, filter: Option<&Where>) -> StorageResult<u64> {
        let rows = self.all(model, filter)?;
        let count = rows.len() as u64;
        for row in rows {
            self.delete(model, &crate::query::Unique::Id(row.id))?;
        }
        Ok(count)
    }

    /// The stored capsule of a record, tombstones included (repositories that
    /// manage capsule revisions themselves).
    pub fn capsule(
        &self,
        model: &str,
        id: Id,
    ) -> StorageResult<Option<aseman_contracts::capsule::CapsuleEnvelope>> {
        self.inner.get(self.model(model)?, id)
    }

    /// Live capsules of `model` matching `query`.
    pub fn capsules(
        &self,
        model: &str,
        query: &FindMany,
    ) -> StorageResult<Vec<aseman_contracts::capsule::CapsuleEnvelope>> {
        let model = self.model(model)?;
        Self::check_filter(model, query.filter.as_ref())?;
        self.inner.find(model, query)
    }

    /// Write a sealed capsule revision (repositories that manage capsule revisions
    /// themselves). The capsule must belong to a catalogued model.
    pub fn put_capsule(
        &self,
        capsule: &aseman_contracts::capsule::CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> StorageResult<()> {
        let model = self.model(&capsule.kind.0)?;
        self.writable(model)?;
        if !capsule.tombstone {
            codec::validate(model, &codec::decode(model, capsule)?.data, true)?;
        }
        self.inner.put(model, capsule, expected_revision)
    }

    pub fn commit(&self) -> StorageResult<()> {
        if self.finished.swap(true, Ordering::AcqRel) {
            return Err(StorageError::invalid("the transaction is already finished"));
        }
        self.inner.commit()
    }

    pub fn rollback(&self) -> StorageResult<()> {
        if self.finished.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.rollback()
    }
}

impl Drop for Trx {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Acquire) {
            let _ = self.inner.rollback();
        }
    }
}
