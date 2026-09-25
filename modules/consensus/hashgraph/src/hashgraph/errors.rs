//! Translation of `chain/hashgraph/errors.go`.

/// Differentiates errors that are normal when the hashgraph is used correctly
/// by multiple goroutines/tasks, from errors that should not occur even in a
/// concurrent context.
#[derive(Debug, Clone)]
pub struct SelfParentError {
    pub msg: String,
    pub normal: bool,
}

impl SelfParentError {
    /// Creates a new `SelfParentError`.
    pub fn new(msg: &str, normal: bool) -> SelfParentError {
        SelfParentError {
            msg: msg.to_string(),
            normal,
        }
    }
}

impl std::fmt::Display for SelfParentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for SelfParentError {}

/// Checks that an error is a [`SelfParentError`] and that it is normal.
///
/// Because of the asynchronous nature of Babble, some routines simultaneously
/// try to append the same event, which is not allowed and throws a self-parent
/// error — but such errors are not real errors worth reporting in the logs.
pub fn is_normal_self_parent_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SelfParentError>()
        .map(|sp| sp.normal)
        .unwrap_or(false)
}
