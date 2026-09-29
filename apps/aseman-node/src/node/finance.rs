//! Core-owned finance state: the free-node exemption set and the cost model.
//!
//! Translation of the finance pieces of `core/module/core/core.go` that the
//! `Core` orchestrator owns directly (RL-011+): which nodes are exempt from the
//! VM pay-lock (`free_nodes`) and the cost knobs used for `vm.cost.negotiate`
//! and program pricing. Everything here is owned by `Core`; the chain module and
//! the public action layer read it through the `ICore` accessors.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aseman_config::AsemanConfig;

/// Cost knobs applied to VM execution and program pricing.
#[derive(Default, Clone)]
pub(crate) struct CostConfig {
    pub(crate) execution_cost_per_second: i64,
    pub(crate) vm_ram_cost_per_mb_minute: i64,
    pub(crate) vm_cpu_core_cost_per_minute: i64,
    pub(crate) vm_disk_cost_per_gb_per_minute: i64,
}

impl CostConfig {
    /// Load the cost knobs from the typed configuration.
    pub(crate) fn from_config(config: &Option<&AsemanConfig>) -> CostConfig {
        match config {
            Some(config) => CostConfig {
                execution_cost_per_second: config.core.execution_cost_per_second,
                vm_ram_cost_per_mb_minute: config.core.ram_cost_per_mb_minute,
                vm_cpu_core_cost_per_minute: config.core.cpu_core_cost_per_minute,
                vm_disk_cost_per_gb_per_minute: config.core.disk_cost_per_gb_minute,
            },
            None => CostConfig::default(),
        }
    }
}

/// Core-owned finance state: free-node exemptions and the cost model.
pub(crate) struct Finance {
    /// Nodes exempt from the VM pay-lock (seeded from the configured root node).
    free_nodes: Mutex<HashMap<String, bool>>,
    cost: Mutex<CostConfig>,
}

impl Finance {
    /// Build finance state from the typed configuration.
    pub(crate) fn new(config: &Option<Arc<AsemanConfig>>) -> Finance {
        let mut free_nodes = HashMap::new();
        if let Some(root) = config
            .as_ref()
            .and_then(|config| config.core.root_node.as_ref())
        {
            free_nodes.insert(root.clone(), true);
        }
        Finance {
            free_nodes: Mutex::new(free_nodes),
            cost: Mutex::new(CostConfig::from_config(&config.as_deref())),
        }
    }

    /// The set of free nodes.
    pub(crate) fn free_nodes(&self) -> HashMap<String, bool> {
        self.free_nodes.lock().unwrap().clone()
    }

    /// Add a free node.
    #[expect(
        dead_code,
        reason = "RL-003: legacy orchestration surface kept until its deletion gate"
    )]
    pub(crate) fn add_free_node(&self, node_id: &str) {
        if node_id.is_empty() {
            return;
        }
        self.free_nodes
            .lock()
            .unwrap()
            .insert(node_id.to_string(), true);
    }

    /// Whether a node is exempt from the VM pay-lock.
    pub(crate) fn is_free_node(&self, node_id: &str) -> bool {
        self.free_nodes.lock().unwrap().contains_key(node_id)
    }

    /// The execution cost per second (for `vm.cost.negotiate`).
    pub(crate) fn execution_cost_per_second(&self) -> i64 {
        self.cost.lock().unwrap().execution_cost_per_second
    }

    /// The VM RAM cost per MB per minute.
    pub(crate) fn vm_ram_cost_per_mb_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_ram_cost_per_mb_minute
    }

    /// The VM CPU-core cost per minute.
    pub(crate) fn vm_cpu_core_cost_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_cpu_core_cost_per_minute
    }

    /// The VM disk cost per GB per minute.
    pub(crate) fn vm_disk_cost_per_gb_per_minute(&self) -> i64 {
        self.cost.lock().unwrap().vm_disk_cost_per_gb_per_minute
    }
}

impl Clone for Finance {
    /// Snapshot for the weak `ICore` forwarding view used inside
    /// `modify_state` transaction closures.
    fn clone(&self) -> Finance {
        Finance {
            free_nodes: Mutex::new(self.free_nodes()),
            cost: Mutex::new(self.cost.lock().unwrap().clone()),
        }
    }
}
