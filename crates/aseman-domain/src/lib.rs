//! Pure Aseman domain values and state transitions.
#![forbid(unsafe_code)]

use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub mod creature;
pub mod signal_tags;
pub mod storage_migration;
pub mod store;
pub mod store_permissions;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

typed_id!(UserId);
typed_id!(CreatureId);
typed_id!(ProgramId);
typed_id!(WorkloadId);
typed_id!(OperationId);
typed_id!(CapsuleId);
typed_id!(ModuleId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredWorkloadState {
    Stopped,
    Running,
    Paused,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedWorkloadState {
    Unknown,
    Pending,
    Running,
    Paused,
    Stopped,
    Failed,
    Lost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Generation(u64);

impl Generation {
    pub const INITIAL: Self = Self(1);
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
    pub fn next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::GenerationOverflow)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesiredWorkload {
    pub id: WorkloadId,
    pub creature_id: CreatureId,
    pub program_id: ProgramId,
    pub generation: Generation,
    pub state: DesiredWorkloadState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatureDatabaseBinding {
    pub creature_id: CreatureId,
    pub provider_id: String,
    pub database: String,
    pub role: String,
    pub generation: Generation,
}

impl CreatureDatabaseBinding {
    pub fn new(
        creature_id: CreatureId,
        provider_id: String,
        database: String,
        role: String,
    ) -> Result<Self, DomainError> {
        for (field, value) in [
            ("provider_id", &provider_id),
            ("database", &database),
            ("role", &role),
        ] {
            if value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(DomainError::InvalidBindingField(field));
            }
        }
        Ok(Self {
            creature_id,
            provider_id,
            database,
            role,
            generation: Generation::INITIAL,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Money {
    minor_units: i128,
    scale: u8,
}

impl Money {
    pub fn new(minor_units: i128, scale: u8) -> Result<Self, DomainError> {
        if scale > 18 {
            return Err(DomainError::InvalidMoneyScale(scale));
        }
        Ok(Self { minor_units, scale })
    }
    #[must_use]
    pub const fn minor_units(self) -> i128 {
        self.minor_units
    }
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }
    pub fn checked_add(self, other: Self) -> Result<Self, DomainError> {
        if self.scale != other.scale {
            return Err(DomainError::MoneyScaleMismatch);
        }
        let minor_units = self
            .minor_units
            .checked_add(other.minor_units)
            .ok_or(DomainError::MoneyOverflow)?;
        Ok(Self {
            minor_units,
            scale: self.scale,
        })
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DomainError {
    #[error("generation overflow")]
    GenerationOverflow,
    #[error("invalid creature database binding field: {0}")]
    InvalidBindingField(&'static str),
    #[error("money scale {0} exceeds 18")]
    InvalidMoneyScale(u8),
    #[error("money scales do not match")]
    MoneyScaleMismatch,
    #[error("money arithmetic overflow")]
    MoneyOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_do_not_compare_across_types() {
        let raw = Uuid::now_v7();
        assert_eq!(
            CreatureId::from_uuid(raw).to_string(),
            ProgramId::from_uuid(raw).to_string()
        );
    }

    #[test]
    fn binding_rejects_provider_syntax_escape() {
        let result = CreatureDatabaseBinding::new(
            CreatureId::new(),
            "postgres".into(),
            "other/db".into(),
            "role".into(),
        );
        assert_eq!(result, Err(DomainError::InvalidBindingField("database")));
    }

    #[test]
    fn money_requires_equal_scale_and_checked_arithmetic() {
        let left = Money::new(100, 2).unwrap();
        assert_eq!(
            left.checked_add(Money::new(50, 2).unwrap())
                .unwrap()
                .minor_units(),
            150
        );
        assert_eq!(
            left.checked_add(Money::new(1, 3).unwrap()),
            Err(DomainError::MoneyScaleMismatch)
        );
    }
}
