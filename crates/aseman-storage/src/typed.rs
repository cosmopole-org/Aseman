//! The typed layer under the generated client (`crate::client`): field handles that
//! build filters and orderings, and one generic client per model.

use crate::engine::Trx;
use crate::error::{StorageError, StorageResult};
use crate::query::{Case, Cond, FindMany, Order, Unique, Where};
use crate::value::{Data, Id, Row, Value};
use std::marker::PhantomData;

/// A generated model.
pub trait Model: Sized {
    /// The model's kind (`core.store`).
    const NAME: &'static str;
    /// The values `create` takes.
    type Create: Into<Data>;
    /// Decode a stored row.
    fn from_row(row: Row) -> StorageResult<Self>;
}

/// Read a required field of a row.
pub fn required<T: FromValue>(row: &Row, model: &str, field: &str) -> StorageResult<T> {
    T::from_value(row.get(field)).ok_or_else(|| {
        StorageError::invalid(format!("{model}: stored record lacks required field {field}"))
    })
}

/// Read an optional field of a row.
pub fn optional<T: FromValue>(row: &Row, field: &str) -> Option<T> {
    T::from_value(row.get(field))
}

/// A Rust type a field value converts to.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Option<Self>;
}

impl FromValue for String {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_text().map(str::to_owned)
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_int()
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Float(value) => Some(*value),
            Value::Int(value) => Some(*value as f64),
            _ => None,
        }
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_bool()
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_bytes().map(<[u8]>::to_vec)
    }
}

impl FromValue for Id {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_id()
    }
}

impl FromValue for serde_json::Value {
    fn from_value(value: &Value) -> Option<Self> {
        value.as_json().cloned()
    }
}

/// A typed handle on one field (or relation) of a model.
pub struct Field<T> {
    name: &'static str,
    kind: PhantomData<fn() -> T>,
}

impl<T> Clone for Field<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Field<T> {}

impl<T: Into<Value>> Field<T> {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            kind: PhantomData,
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn cond(&self, cond: Cond) -> Where {
        Where::Field(self.name.to_owned(), cond)
    }

    #[must_use]
    pub fn eq(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Equals(value.into().into()))
    }

    #[must_use]
    pub fn not(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Not(value.into().into()))
    }

    #[must_use]
    pub fn is_in<I: IntoIterator<Item = V>, V: Into<T>>(&self, values: I) -> Where {
        self.cond(Cond::In(
            values.into_iter().map(|value| value.into().into()).collect(),
        ))
    }

    #[must_use]
    pub fn not_in<I: IntoIterator<Item = V>, V: Into<T>>(&self, values: I) -> Where {
        self.cond(Cond::NotIn(
            values.into_iter().map(|value| value.into().into()).collect(),
        ))
    }

    #[must_use]
    pub fn lt(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Lt(value.into().into()))
    }

    #[must_use]
    pub fn lte(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Lte(value.into().into()))
    }

    #[must_use]
    pub fn gt(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Gt(value.into().into()))
    }

    #[must_use]
    pub fn gte(&self, value: impl Into<T>) -> Where {
        self.cond(Cond::Gte(value.into().into()))
    }

    #[must_use]
    pub fn is_null(&self) -> Where {
        self.cond(Cond::IsNull(true))
    }

    #[must_use]
    pub fn is_set(&self) -> Where {
        self.cond(Cond::IsNull(false))
    }

    #[must_use]
    pub fn asc(&self) -> Order {
        Order::asc(self.name)
    }

    #[must_use]
    pub fn desc(&self) -> Order {
        Order::desc(self.name)
    }
}

impl Field<String> {
    #[must_use]
    pub fn contains(&self, text: impl Into<String>) -> Where {
        self.cond(Cond::Contains(text.into(), Case::Sensitive))
    }

    #[must_use]
    pub fn contains_insensitive(&self, text: impl Into<String>) -> Where {
        self.cond(Cond::Contains(text.into(), Case::Insensitive))
    }

    #[must_use]
    pub fn starts_with(&self, text: impl Into<String>) -> Where {
        self.cond(Cond::StartsWith(text.into(), Case::Sensitive))
    }

    #[must_use]
    pub fn starts_with_insensitive(&self, text: impl Into<String>) -> Where {
        self.cond(Cond::StartsWith(text.into(), Case::Insensitive))
    }

    #[must_use]
    pub fn ends_with(&self, text: impl Into<String>) -> Where {
        self.cond(Cond::EndsWith(text.into(), Case::Sensitive))
    }
}

/// The changes an `update` applies; generated setters fill it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Update(pub Data);

impl Update {
    #[must_use]
    pub fn set(mut self, field: &str, value: impl Into<Value>) -> Self {
        self.0.insert(field.to_owned(), value.into());
        self
    }

    /// Clear an optional field.
    #[must_use]
    pub fn clear(mut self, field: &str) -> Self {
        self.0.insert(field.to_owned(), Value::Null);
        self
    }
}

impl From<Update> for Data {
    fn from(update: Update) -> Self {
        update.0
    }
}

/// The typed client of one model inside a transaction (`trx.store()`).
pub struct ModelClient<'a, M> {
    trx: &'a Trx,
    model: PhantomData<fn() -> M>,
}

impl<'a, M: Model> ModelClient<'a, M> {
    #[must_use]
    pub fn new(trx: &'a Trx) -> Self {
        Self {
            trx,
            model: PhantomData,
        }
    }

    fn decode(rows: Vec<Row>) -> StorageResult<Vec<M>> {
        rows.into_iter().map(M::from_row).collect()
    }

    pub fn find_unique(&self, by: Unique) -> StorageResult<Option<M>> {
        self.trx.find_unique(M::NAME, &by)?.map(M::from_row).transpose()
    }

    /// The record `by` names, or `NotFound`.
    pub fn get(&self, by: Unique) -> StorageResult<M> {
        self.find_unique(by.clone())?
            .ok_or_else(|| StorageError::NotFound(format!("{} {by:?}", M::NAME)))
    }

    pub fn find_first(&self, query: FindMany) -> StorageResult<Option<M>> {
        self.trx.find_first(M::NAME, &query)?.map(M::from_row).transpose()
    }

    pub fn find_many(&self, query: FindMany) -> StorageResult<Vec<M>> {
        Self::decode(self.trx.find_many(M::NAME, &query)?)
    }

    /// Every record matching `filter` (a filtered `find_many` without a page bound).
    pub fn find_where(&self, filter: Where) -> StorageResult<Vec<M>> {
        self.find_many(FindMany::filter(filter))
    }

    pub fn count(&self, filter: Option<Where>) -> StorageResult<u64> {
        self.trx.count(M::NAME, filter.as_ref())
    }

    pub fn exists(&self, filter: Where) -> StorageResult<bool> {
        self.trx.exists(M::NAME, &filter)
    }

    pub fn create(&self, data: M::Create) -> StorageResult<M> {
        M::from_row(self.trx.create(M::NAME, data.into())?)
    }

    pub fn update(&self, by: Unique, update: impl Into<Update>) -> StorageResult<Option<M>> {
        self.trx
            .update(M::NAME, &by, Data::from(update.into()))?
            .map(M::from_row)
            .transpose()
    }

    pub fn upsert(
        &self,
        by: Unique,
        create: M::Create,
        update: impl Into<Update>,
    ) -> StorageResult<M> {
        M::from_row(self.trx.upsert(
            M::NAME,
            &by,
            create.into(),
            Data::from(update.into()),
        )?)
    }

    pub fn delete(&self, by: Unique) -> StorageResult<Option<M>> {
        self.trx.delete(M::NAME, &by)?.map(M::from_row).transpose()
    }

    pub fn update_many(&self, filter: Option<Where>, update: impl Into<Update>) -> StorageResult<u64> {
        self.trx
            .update_many(M::NAME, filter.as_ref(), Data::from(update.into()))
    }

    pub fn delete_many(&self, filter: Option<Where>) -> StorageResult<u64> {
        self.trx.delete_many(M::NAME, filter.as_ref())
    }
}
