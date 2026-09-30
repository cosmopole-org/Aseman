//! Why an operation did not take effect, and the strictly typed input parser
//! handlers use.

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Why an operation did not take effect.
#[derive(Debug)]
pub enum ActionError {
    /// The body is not the operation's input.
    Invalid(String),
    /// The policy or the operation refused the request; its writes were
    /// discarded.
    Refused(String),
    /// The node's storage could not serve or commit it.
    Unavailable(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::Refused(message) | Self::Unavailable(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for ActionError {}

/// A body that is JSON but not the operation's input.
#[derive(Debug)]
pub struct InvalidInput(pub String);

impl std::fmt::Display for InvalidInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidInput {}

/// The operation's input, strictly: a field of the wrong type is refused rather
/// than silently defaulted.
pub fn parse<T: DeserializeOwned>(input: &Value) -> anyhow::Result<T> {
    serde_json::from_value(input.clone())
        .map_err(|error| InvalidInput(format!("invalid input: {error}")).into())
}

/// Map a handler's `anyhow` error to the typed [`ActionError`]: a
/// [`InvalidInput`] is `Invalid`, everything else is a `Refused`.
pub fn action_error(error: anyhow::Error) -> ActionError {
    match error.downcast::<InvalidInput>() {
        Ok(InvalidInput(message)) => ActionError::Invalid(message),
        Err(error) => ActionError::Refused(error.to_string()),
    }
}