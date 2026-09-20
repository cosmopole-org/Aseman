//! Framework-neutral observability vocabulary.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationContext {
    pub request_id: String,
    pub trace_id: String,
    pub node_id: Option<String>,
    pub creature_id: Option<String>,
    pub workload_id: Option<String>,
    pub operation_id: Option<String>,
}
