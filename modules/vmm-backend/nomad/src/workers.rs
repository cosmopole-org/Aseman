//! Worker lifecycle: enroll, cordon, drain, and removal (P6-03, A606).
//!
//! These are operator actions, not scheduling. They use an operator-scoped Nomad
//! token, never the backend's: a component that runs workloads must not be able to
//! cordon the cluster it runs them on (A602). The types are separate for the same
//! reason — holding a [`NomadBackend`](crate::backend::NomadBackend) gives you no way
//! to reach any of this.
//!
//! None of it touches Aseman identity. Adding a worker, draining one, or losing one
//! changes which machine a workload runs on and nothing else: the node ID, its
//! signing-key lineage, and the workload's own ID and generation are untouched
//! (ADR 0013).

use aseman_ports::{PortError, PortResult};
use serde_json::json;

use crate::client::Nomad;

/// A worker as the operator sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Worker {
    pub id: String,
    pub name: String,
    pub datacenter: String,
    /// `ready`, `down`, `initializing`, or `disconnected`.
    pub status: String,
    /// Whether the scheduler may place new work here.
    pub eligible: bool,
    /// Whether existing work is being moved off.
    pub draining: bool,
}

impl Worker {
    /// Whether this worker can take new workloads right now.
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.status == "ready" && self.eligible && !self.draining
    }
}

/// The operator's view of the worker pool.
///
/// `nomad` must carry an operator token with `node-write`; the backend's own token
/// deliberately does not have it.
pub struct Workers {
    nomad: Nomad,
}

impl Workers {
    #[must_use]
    pub fn new(nomad: Nomad) -> Self {
        Self { nomad }
    }

    /// Every worker in the cluster.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn list(&self) -> PortResult<Vec<Worker>> {
        Ok(self
            .nomad
            .nodes()?
            .into_iter()
            .map(|node| Worker {
                id: node.id,
                name: node.name,
                datacenter: node.datacenter,
                status: node.status,
                eligible: node.scheduling_eligibility != "ineligible",
                draining: node.drain,
            })
            .collect())
    }

    /// One worker by ID.
    ///
    /// # Errors
    ///
    /// [`PortError::NotFound`] when no worker has that ID.
    pub fn get(&self, id: &str) -> PortResult<Worker> {
        self.list()?
            .into_iter()
            .find(|worker| worker.id == id)
            .ok_or(PortError::NotFound)
    }

    /// Stop placing new work on a worker, leaving what runs there alone.
    ///
    /// This is the first half of taking a machine out of service: it stops the bleed
    /// while an operator decides whether to drain or to fix it in place.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or the token lacks `node-write`.
    pub fn cordon(&self, id: &str) -> PortResult<()> {
        self.eligibility(id, false)
    }

    /// Place work on a worker again.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or the token lacks `node-write`.
    pub fn uncordon(&self, id: &str) -> PortResult<()> {
        self.eligibility(id, true)
    }

    fn eligibility(&self, id: &str, eligible: bool) -> PortResult<()> {
        self.nomad
            .post(
                &format!("/v1/node/{id}/eligibility"),
                &json!({
                    "NodeID": id,
                    "Eligibility": if eligible { "eligible" } else { "ineligible" },
                }),
            )
            .map(|_| ())
    }

    /// Move every workload off a worker within `deadline_millis`, then stop what is
    /// left. A drain also cordons: nothing is placed back while it runs.
    ///
    /// A workload that moves keeps its identity and its generation — it is the same
    /// workload on another machine.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or the token lacks `node-write`.
    pub fn drain(&self, id: &str, deadline_millis: i64) -> PortResult<()> {
        if deadline_millis <= 0 {
            return Err(PortError::Denied("a drain deadline is positive"));
        }
        self.nomad
            .post(
                &format!("/v1/node/{id}/drain"),
                &json!({
                    "NodeID": id,
                    "DrainSpec": {
                        "Deadline": deadline_millis * 1_000_000,
                        "IgnoreSystemJobs": false,
                    },
                }),
            )
            .map(|_| ())
    }

    /// Stop a drain in progress. The worker stays cordoned: an operator who wants it
    /// serving again says so with [`Self::uncordon`], so a cancelled drain never
    /// silently puts a suspect machine back into rotation.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or the token lacks `node-write`.
    pub fn cancel_drain(&self, id: &str) -> PortResult<()> {
        self.nomad
            .post(
                &format!("/v1/node/{id}/drain"),
                &json!({"NodeID": id, "DrainSpec": null}),
            )
            .map(|_| ())
    }

    /// Whether every workload has left a draining worker.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn drained(&self, id: &str) -> PortResult<bool> {
        Ok(self
            .nomad
            .node_allocations(id)?
            .iter()
            .all(|allocation| allocation.client_status != "running"))
    }
}
