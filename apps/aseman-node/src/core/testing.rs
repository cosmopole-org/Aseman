//! Test support: a minimal [`ICore`] whose state closures run on the node's storage
//! module over the in-memory provider (ADR 0036).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::core::orchestrator::icore::{begin_trx, run_trx_closure};
use crate::models::action::{IActor, TrxClosure};
use crate::models::core::{ICore, StateClosure};
use crate::models::globe::IGlobe;
use crate::models::info::IInfo;
use crate::models::ports::{IStorage, ITools};

/// The node's storage module over a fresh in-memory provider.
pub(crate) fn test_node_storage() -> Arc<dyn IStorage> {
    crate::adapters::storage::Storage::new("", crate::core::trx::test_storage())
}

/// An `ICore` that only runs state closures, each in its own committed transaction.
pub(crate) struct StubCore {
    pub(crate) storage: Arc<dyn IStorage>,
}

impl StubCore {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            storage: test_node_storage(),
        })
    }
}

impl ICore for StubCore {
    fn owner_id(&self) -> String {
        String::new()
    }
    fn id(&self) -> String {
        "test-node".into()
    }
    fn gods(&self) -> Vec<String> {
        Vec::new()
    }
    fn add_god(&self, _: &str) {}
    fn tools(&self) -> Arc<dyn ITools> {
        unimplemented!("the stub core has no tools")
    }
    fn free_nodes(&self) -> HashMap<String, bool> {
        HashMap::new()
    }
    fn add_free_node(&self, _: &str) {}
    fn actor(&self) -> Arc<dyn IActor> {
        unimplemented!("the stub core has no actor")
    }
    fn load(&self, _: Vec<String>, _: HashMap<String, Value>) {}
    fn close(&self) {}
    fn plant_chain_trigger(&self, _: i64, _: &str, _: &str, _: &str, _: &str, _: &str) {}
    fn ip_addr(&self) -> String {
        String::new()
    }
    fn modify_state(&self, readonly: bool, fn_: TrxClosure) {
        if let Some(trx) = begin_trx(&self.storage, readonly) {
            run_trx_closure(&trx, fn_);
        }
    }
    fn modify_state_securly_with_source(
        &self,
        _: bool,
        _: Arc<dyn IInfo>,
        _: &str,
        _: StateClosure,
    ) {
    }
    fn modify_state_securly(&self, _: bool, _: Arc<dyn IInfo>, _: StateClosure) {}
    fn sign_packet(&self, _: &[u8]) -> String {
        String::new()
    }
    fn sign_packet_as_owner(&self, _: &[u8]) -> String {
        String::new()
    }
    fn execution_cost_per_second(&self) -> i64 {
        0
    }
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        0
    }
    fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        0
    }
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        0
    }
    fn globe(&self) -> Arc<dyn IGlobe> {
        unimplemented!("the stub core has no globe")
    }
}
