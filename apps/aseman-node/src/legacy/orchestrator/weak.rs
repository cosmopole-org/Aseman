//! The `ICore` implementation for the legacy `Core` weak forwarding view.
//!
//! `WeakCoreView` is a short-lived `Arc<dyn ICore>` shim built by
//! `Core::weak_self` so the `modify_state` family can hand a transaction an
//! `Arc<dyn ICore>` without holding a real reference to the orchestrator.
//!
//! Translation of `core/module/core/core.go`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

use crate::api::model::core_storage::StateFailure;
use crate::legacy::orchestrator::icore::{run_state_closure, run_trx_closure};
use crate::legacy::orchestrator::types::{Core, WeakCoreView};
use crate::models::action::IActor;
use crate::models::action::TrxClosure;
use crate::models::core::{ICore, StateClosure};
use crate::models::globe::IGlobe;
use crate::models::info::IInfo;
use crate::models::ports::ITools;

impl ICore for WeakCoreView {
    fn owner_id(&self) -> String {
        self.inner.owner_id.clone()
    }
    fn id(&self) -> String {
        self.inner.id.clone()
    }
    fn gods(&self) -> Vec<String> {
        self.inner.gods.clone()
    }
    fn add_god(&self, _: &str) {}
    fn tools(&self) -> Arc<dyn ITools> {
        self.inner.tools.clone().expect("tools unset on weak view")
    }
    fn free_nodes(&self) -> HashMap<String, bool> {
        self.inner.finance.free_nodes()
    }
    fn add_free_node(&self, node_id: &str) {
        self.inner.finance.add_free_node(node_id);
    }
    fn actor(&self) -> Arc<dyn IActor> {
        self.inner.actor.clone()
    }
    fn load(&self, _: Vec<String>, _: HashMap<String, Value>) {}
    fn close(&self) {}
    fn plant_chain_trigger(&self, _: i64, _: &str, _: &str, _: &str, _: &str, _: &str) {}
    fn ip_addr(&self) -> String {
        self.inner.ip.clone()
    }
    fn modify_state(&self, readonly: bool, fn_: TrxClosure) {
        if let Some(trx) = self.checked_trx(readonly) {
            run_trx_closure(&trx, fn_);
        }
    }
    fn modify_state_securly_with_source(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) {
        if let Some(trx) = self.checked_trx(readonly)
            && let Err(StateFailure::Storage(error)) = run_state_closure(&trx, info, src, fn_)
        {
            eprintln!("modify_state_securly: {error}");
        }
    }
    fn modify_state_securly(&self, readonly: bool, info: Arc<dyn IInfo>, fn_: StateClosure) {
        self.modify_state_securly_with_source(readonly, info, "", fn_);
    }
    fn modify_state_securly_checked(
        &self,
        readonly: bool,
        info: Arc<dyn IInfo>,
        src: &str,
        fn_: StateClosure,
    ) -> Result<()> {
        let Some(trx) = self.checked_trx(readonly) else {
            return Err(anyhow::anyhow!("state is not available"));
        };
        run_state_closure(&trx, info, src, fn_).map_err(StateFailure::into_error)
    }
    fn sign_packet(&self, data: &[u8]) -> String {
        match &self.inner.priv_key {
            Some(k) => Core::sign_with(k, data),
            None => String::new(),
        }
    }
    fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        Core::sign_with(&self.inner.owner_priv_key, data)
    }
    fn execution_cost_per_second(&self) -> i64 {
        self.inner.finance.execution_cost_per_second()
    }
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        self.inner.finance.vm_ram_cost_per_mb_per_minute()
    }
    fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        self.inner.finance.vm_cpu_core_cost_per_minute()
    }
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        self.inner.finance.vm_disk_cost_per_gb_per_minute()
    }
    fn globe(&self) -> Arc<dyn IGlobe> {
        self.inner.globe.clone().expect("Globe unset on weak view")
    }
}

// Kept for import calm.
const _: fn() -> Option<Core> = || None;
