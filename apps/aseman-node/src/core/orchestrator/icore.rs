//! The `ICore` trait implementation for the `Core` compatibility orchestrator, the
//! ADR-0026 transaction/state-modification helpers, and the `checked_trx` /
//! `weak_self` internals they share with the weak view.
//!
//! Translation of `core/module/core/core.go`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use rsa::RsaPrivateKey;
use serde_json::Value;

use crate::adapters::rocksdb::trx::TrxWrapper;
use crate::api::model::core_storage::{StateFailure, run_action};
use crate::core::orchestrator::types::{Core, CoreWeakHandles, WeakCoreView};
use crate::core::{Info as BaseInfo, State as ActorState};
use crate::models::action::IActor;
use crate::models::action::TrxClosure;
use crate::models::core::{ICore, StateClosure};
use crate::models::globe::IGlobe;
use crate::models::info::IInfo;
use crate::models::ports::{IStorage, ITools, StateBackend};
use crate::models::state::IState;
use crate::models::transaction::ITrx;

impl ICore for Core {
    fn owner_id(&self) -> String {
        self.owner_id.clone()
    }
    fn id(&self) -> String {
        self.id.clone()
    }
    fn gods(&self) -> Vec<String> {
        self.gods.lock().unwrap().clone()
    }
    fn add_god(&self, username: &str) {
        if username.is_empty() {
            return;
        }
        let mut gods = self.gods.lock().unwrap();
        if !gods.iter().any(|g| g == username) {
            gods.push(username.to_string());
        }
    }
    fn tools(&self) -> Arc<dyn ITools> {
        self.tools
            .lock()
            .unwrap()
            .clone()
            .expect("Core.tools accessed before Load()")
    }
    fn free_nodes(&self) -> HashMap<String, bool> {
        self.finance.free_nodes()
    }
    fn add_free_node(&self, node_id: &str) {
        self.finance.add_free_node(node_id);
    }
    fn actor(&self) -> Arc<dyn IActor> {
        self.actor.clone()
    }
    fn load(&self, args: Vec<String>, config: HashMap<String, Value>) {
        // The `args`/`config` here are the same shape as Go's variadic
        // `args ...interface{}` map. We route through `Core::load_inner`
        // which expects strongly-typed paths; this trait method exists so
        // the abstraction signature stays Go-compatible. main.rs uses
        // `load_inner` directly.
        let _ = (args, config);
    }
    fn close(&self) {
        if let Some(tools) = self.tools.lock().unwrap().clone() {
            tools.network().chain().close();
            // The key/value store and the private QuestDB pool close on drop via their Arc owners.
        }
    }
    fn plant_chain_trigger(
        &self,
        count: i64,
        user_id: &str,
        tag: &str,
        machine_id: &str,
        store_id: &str,
        input: &str,
    ) {
        let user_id_owned = user_id.to_string();
        let tag_owned = tag.to_string();
        let machine_id_owned = machine_id.to_string();
        let store_id_owned = store_id.to_string();
        let input_owned = input.to_string();
        self.modify_state(
            false,
            Box::new(move |trx: &dyn ITrx| {
                let tail = crate::api::utils::crypto::secure_unique_string();
                let prefix = format!("chainCallback::{}_{}", user_id_owned, tag_owned);
                let already = !trx.get_by_prefix(&format!("{}|>", prefix)).is_empty();
                trx.put_bytes(&format!("{}|>{}", prefix, tail), vec![0x01]);
                trx.put_bytes(
                    &format!("{}|{}::machineId", prefix, tail),
                    machine_id_owned.as_bytes().to_vec(),
                );
                trx.put_bytes(
                    &format!("{}|{}::storeId", prefix, tail),
                    store_id_owned.as_bytes().to_vec(),
                );
                trx.put_bytes(
                    &format!("{}|{}::attachment", prefix, tail),
                    input_owned.as_bytes().to_vec(),
                );
                if !already {
                    trx.put_bytes(
                        &format!("{}::targetCount", prefix),
                        (count as u32).to_be_bytes().to_vec(),
                    );
                    trx.put_bytes(
                        &format!("{}::tempCount", prefix),
                        0u32.to_be_bytes().to_vec(),
                    );
                }
                Ok(())
            }),
        );
    }
    fn ip_addr(&self) -> String {
        self.ip.clone()
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
        let key = self.priv_key.lock().unwrap().clone();
        match key {
            Some(k) => Core::sign_with(&k, data),
            None => String::new(),
        }
    }
    fn sign_packet_as_owner(&self, data: &[u8]) -> String {
        Core::sign_with(&self.owner_priv_key, data)
    }
    fn execution_cost_per_second(&self) -> i64 {
        self.finance.execution_cost_per_second()
    }
    fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        self.finance.vm_ram_cost_per_mb_per_minute()
    }
    fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        self.finance.vm_cpu_core_cost_per_minute()
    }
    fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        self.finance.vm_disk_cost_per_gb_per_minute()
    }
    fn globe(&self) -> Arc<dyn IGlobe> {
        self.globe
            .lock()
            .unwrap()
            .clone()
            .expect("Core.globe accessed before Load()")
    }

    fn consensus_provider(&self) -> Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>> {
        // RL-011: the consensus provider is owned by the chain/tool module,
        // not the core orchestrator; reach it through the chain adapter.
        self.tools().network().chain().consensus_provider()
    }
}

impl Core {
    /// A transaction over this core's storage, when the tools are loaded.
    pub(crate) fn checked_trx(&self, readonly: bool) -> Option<Arc<dyn ITrx>> {
        let tools = self.tools.lock().unwrap().clone()?;
        begin_trx(self.weak_self(), &tools.storage(), readonly)
    }

    /// Build a fresh `Arc<dyn ICore>` pointing at the same underlying
    /// `Core` state. Used by paths that need to hand an `Arc<dyn ICore>`
    /// to drivers / closures.
    pub(crate) fn weak_self(&self) -> Arc<dyn ICore> {
        // We can't recover the real `Arc<Core>` from `&self` without an
        // upgrade target, so construct a forwarding wrapper that holds
        // references to every interior field. For our use sites the
        // wrapper is short-lived (one transaction), so the extra Arc
        // allocations are not a hot path.
        Arc::new(WeakCoreView {
            inner: CoreWeakHandles {
                tools: self.tools.lock().unwrap().clone(),
                actor: self.actor.clone(),
                owner_id: self.owner_id.clone(),
                id: self.id.clone(),
                ip: self.ip.clone(),
                owner_priv_key: self.owner_priv_key.clone(),
                priv_key: self.priv_key.lock().unwrap().clone(),
                finance: self.finance.clone(),
                globe: self.globe.lock().unwrap().clone(),
                gods: self.gods.lock().unwrap().clone(),
            },
        })
    }
}

/// Run a transaction closure with ADR 0026 commit ordering; a storage failure is
/// logged (LD-10), an action failure is the closure's own answer.
/// A transaction on the selected storage provider (ADR 0033), or `None` when the
/// provider cannot begin one (reported).
pub(crate) fn begin_trx(
    core: Arc<dyn ICore>,
    storage: &Arc<dyn IStorage>,
    readonly: bool,
) -> Option<Arc<dyn ITrx>> {
    match storage.state() {
        StateBackend::RocksDb(db) => Some(TrxWrapper::new(core, db, readonly) as Arc<dyn ITrx>),
        StateBackend::Postgres(factory) => match factory.begin(readonly) {
            Ok(trx) => Some(trx as Arc<dyn ITrx>),
            Err(error) => {
                eprintln!("storage: cannot begin a PostgreSQL transaction: {error}");
                None
            }
        },
    }
}

pub(crate) fn run_trx_closure(trx: &Arc<dyn ITrx>, mut fn_: TrxClosure) {
    if let Err(StateFailure::Storage(error)) = run_action(
        trx.readonly(),
        || fn_(&**trx),
        || trx.commit(),
        || trx.discard(),
    ) {
        eprintln!("modify_state: {error}");
    }
}

/// Run a secured state closure with ADR 0026 commit ordering.
pub(crate) fn run_state_closure(
    trx: &Arc<dyn ITrx>,
    info: Arc<dyn IInfo>,
    src: &str,
    mut fn_: StateClosure,
) -> Result<(), StateFailure> {
    let state: Arc<dyn IState> = Arc::new(ActorState::new(Some(info), Some(trx.clone()), src));
    run_action(
        trx.readonly(),
        || fn_(state),
        || trx.commit(),
        || trx.discard(),
    )
}

// Kept for signature parity / import calm.
const _: fn() -> Option<RsaPrivateKey> = || None;
const _: fn() -> Option<BaseInfo> = || None;
const _: fn() -> Option<Value> = || None;
