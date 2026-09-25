use serde::{Deserialize, Serialize};

/// A transaction queued for execution by a virtual machine worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trx {
    #[serde(rename = "key")]
    pub key: String,
    #[serde(rename = "payload")]
    pub payload: String,
    #[serde(rename = "signature")]
    pub signature: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "machineId")]
    pub machine_id: String,
    #[serde(rename = "runtime")]
    pub runtime: String,
    #[serde(rename = "gasLimit")]
    pub gas_limit: i64,
    #[serde(rename = "callbackId")]
    pub callback_id: String,
}
