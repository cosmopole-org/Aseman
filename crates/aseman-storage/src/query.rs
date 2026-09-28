//! Prisma-style filters, ordering, and pagination.

use crate::value::{Data, Id, Value};

/// A condition on one field.
#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    Equals(Value),
    Not(Value),
    In(Vec<Value>),
    NotIn(Vec<Value>),
    Lt(Value),
    Lte(Value),
    Gt(Value),
    Gte(Value),
    Contains(String, Case),
    StartsWith(String, Case),
    EndsWith(String, Case),
    /// `true`: the field is null; `false`: it is set.
    IsNull(bool),
}

/// Text matching mode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Case {
    #[default]
    Sensitive,
    Insensitive,
}

/// A filter (`where`).
#[derive(Clone, Debug, PartialEq)]
pub enum Where {
    Field(String, Cond),
    And(Vec<Where>),
    Or(Vec<Where>),
    Not(Box<Where>),
}

impl Where {
    #[must_use]
    pub fn field(name: &str, cond: Cond) -> Self {
        Self::Field(name.to_owned(), cond)
    }

    #[must_use]
    pub fn eq(name: &str, value: impl Into<Value>) -> Self {
        Self::field(name, Cond::Equals(value.into()))
    }

    #[must_use]
    pub fn and(self, other: Where) -> Self {
        match self {
            Self::And(mut all) => {
                all.push(other);
                Self::And(all)
            }
            first => Self::And(vec![first, other]),
        }
    }

    /// The conjunction of `filters`, or `None` when there are none.
    #[must_use]
    pub fn all(filters: Vec<Where>) -> Option<Where> {
        match filters.len() {
            0 => None,
            1 => filters.into_iter().next(),
            _ => Some(Self::And(filters)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Asc,
    Desc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub field: String,
    pub direction: Direction,
}

impl Order {
    #[must_use]
    pub fn asc(field: &str) -> Self {
        Self {
            field: field.to_owned(),
            direction: Direction::Asc,
        }
    }

    #[must_use]
    pub fn desc(field: &str) -> Self {
        Self {
            field: field.to_owned(),
            direction: Direction::Desc,
        }
    }
}

/// `find_many` arguments. Rows are ordered by `order_by`, then by id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindMany {
    pub filter: Option<Where>,
    pub order_by: Vec<Order>,
    pub skip: u64,
    /// At most this many rows; `None` is the provider's bound.
    pub take: Option<u64>,
}

impl FindMany {
    #[must_use]
    pub fn filter(filter: Where) -> Self {
        Self {
            filter: Some(filter),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn order_by(mut self, order: Order) -> Self {
        self.order_by.push(order);
        self
    }

    #[must_use]
    pub fn skip(mut self, skip: u64) -> Self {
        self.skip = skip;
        self
    }

    #[must_use]
    pub fn take(mut self, take: u64) -> Self {
        self.take = Some(take);
        self
    }
}

/// How `find_unique`, `update`, `upsert`, and `delete` name one record.
#[derive(Clone, Debug, PartialEq)]
pub enum Unique {
    Id(Id),
    /// The natural key of a keyed model.
    Key(String),
    /// The fields of one of the model's unique indexes.
    Fields(Data),
}

impl Unique {
    #[must_use]
    pub fn key(key: impl Into<String>) -> Self {
        Self::Key(key.into())
    }

    /// One unique index, as `(field, value)` pairs.
    #[must_use]
    pub fn fields<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Value>,
    {
        Self::Fields(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect::<Data>(),
        )
    }
}
