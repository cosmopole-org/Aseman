//! One operation an action plugin contributes.

use crate::origin::ActionOrigin;

/// One operation a plugin contributes: its path and where a signed packet runs
/// it. The action it is authorized as and its packet guard come from the A402
/// registry (ADR 0039), which the router checks the plugin table against.
#[derive(Debug, Clone)]
pub struct ActionOperationSpec {
    pub path: &'static str,
    pub origin: ActionOrigin,
}

impl ActionOperationSpec {
    #[must_use]
    pub const fn new(path: &'static str, origin: ActionOrigin) -> Self {
        Self { path, origin }
    }
}