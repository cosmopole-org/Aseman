//! The node's prices and free nodes, from its configuration: what a VM costs per
//! minute and what execution costs per second (quoted on `vm.cost.negotiate`),
//! and which nodes are exempt from the VM pay-lock.

use std::collections::BTreeSet;

use aseman_config::AsemanConfig;

/// What running on this node costs (shared with the action plugins, ADR 0040).
pub use aseman_action_sdk::context::VmCosts;

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
            costs: config.map_or_else(VmCosts::default, |config| VmCosts {
                execution_per_second: config.core.execution_cost_per_second,
                ram_per_mb_minute: config.core.ram_cost_per_mb_minute,
                cpu_core_per_minute: config.core.cpu_core_cost_per_minute,
                disk_per_gb_minute: config.core.disk_cost_per_gb_minute,
            }),
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