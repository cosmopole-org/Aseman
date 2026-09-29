//! The node's prices and free nodes, from its configuration: what a VM costs per
//! minute and what execution costs per second (quoted on `vm.cost.negotiate`),
//! and which nodes are exempt from the VM pay-lock.

use std::collections::BTreeSet;

use aseman_config::AsemanConfig;

/// What running on this node costs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct VmCosts {
    pub(crate) execution_per_second: i64,
    pub(crate) ram_per_mb_minute: i64,
    pub(crate) cpu_core_per_minute: i64,
    pub(crate) disk_per_gb_minute: i64,
}

impl VmCosts {
    fn from_config(config: Option<&AsemanConfig>) -> Self {
        config.map_or_else(Self::default, |config| Self {
            execution_per_second: config.core.execution_cost_per_second,
            ram_per_mb_minute: config.core.ram_cost_per_mb_minute,
            cpu_core_per_minute: config.core.cpu_core_cost_per_minute,
            disk_per_gb_minute: config.core.disk_cost_per_gb_minute,
        })
    }

    /// Whether VMs are free here (every VM rate is zero).
    pub(crate) fn is_free(&self) -> bool {
        self.ram_per_mb_minute == 0 && self.cpu_core_per_minute == 0 && self.disk_per_gb_minute == 0
    }
}

/// The node's prices and the nodes exempt from paying them.
pub(crate) struct Finance {
    free_nodes: BTreeSet<String>,
    costs: VmCosts,
}

impl Finance {
    pub(crate) fn new(config: Option<&AsemanConfig>) -> Self {
        Self {
            free_nodes: config
                .and_then(|config| config.core.root_node.clone())
                .into_iter()
                .collect(),
            costs: VmCosts::from_config(config),
        }
    }

    /// Whether a node is exempt from the VM pay-lock.
    pub(crate) fn is_free_node(&self, node_id: &str) -> bool {
        self.free_nodes.contains(node_id)
    }

    pub(crate) fn costs(&self) -> VmCosts {
        self.costs
    }
}
