//! Storage errors.

use thiserror::Error;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StorageError {
    /// The request is malformed or names something the schema does not declare.
    #[error("invalid storage request: {0}")]
    Invalid(String),
    /// A revision or uniqueness conflict; the caller may retry.
    #[error("storage conflict: {0}")]
    Conflict(String),
    /// The record the operation needs does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The provider cannot do this.
    #[error("unsupported by the storage provider: {0}")]
    Unsupported(String),
    /// The provider is unreachable or failed.
    #[error("storage unavailable: {0}")]
    Unavailable(String),
}

impl StorageError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub fn unavailable(message: impl std::fmt::Display) -> Self {
        Self::Unavailable(message.to_string())
    }
}

pub type StorageResult<T> = Result<T, StorageError>;
