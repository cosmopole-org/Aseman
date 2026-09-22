//! Capability use cases (A403): authorize with the subject's grant chains, issue root
//! grants, delegate by attenuation, and revoke with every descendant. Who may issue,
//! delegate, or revoke is itself decided by policy (`capability.*` actions).

use crate::ApplicationError;
use aseman_domain::Uuid;
use aseman_domain::authority::{
    ActionRegistry, Condition, PolicyDecision, PolicyRequest, ResourceRef,
};
use aseman_domain::capability::{
    DelegationRequest, Grant, ResourceSelector, check_chain, delegate,
};
use aseman_domain::identity::Subject;
use aseman_ports::{ClockPort, GrantStore, PolicyDecisionPort, PortError};
use std::collections::{BTreeSet, VecDeque};

/// The longest chain loaded; a longer one cannot authorize.
pub const MAX_CHAIN_LENGTH: usize = 16;
/// The most descendants one revocation reports for re-evaluation.
pub const MAX_REVOCATION_CLOSURE: usize = 10_000;

/// The chain of `grant`: it, then its ancestors to the root. `None` when a link is
/// missing or the chain is too long.
fn chain_of(store: &dyn GrantStore, grant: Grant) -> Result<Option<Vec<Grant>>, PortError> {
    let mut chain = vec![grant];
    while let Some(parent) = chain.last().and_then(|grant| grant.parent) {
        if chain.len() >= MAX_CHAIN_LENGTH {
            return Ok(None);
        }
        match store.grant(parent)? {
            Some(grant) => chain.push(grant),
            None => return Ok(None),
        }
    }
    Ok(Some(chain))
}

/// The subject's candidate chains for `action`.
///
/// # Errors
///
/// A failing grant store.
pub fn load_chains(
    store: &dyn GrantStore,
    subject: &Subject,
    action: &str,
) -> Result<Vec<Vec<Grant>>, PortError> {
    let mut chains = Vec::new();
    for grant in store.grants_of(subject)? {
        if grant.actions.contains(action)
            && let Some(chain) = chain_of(store, grant)?
        {
            chains.push(chain);
        }
    }
    Ok(chains)
}

/// Decide with the subject's grants loaded (ADR 0008: grant loading and evaluation are
/// separate ports).
pub struct Authorize<'a> {
    pub policy: &'a dyn PolicyDecisionPort,
    pub grants: &'a dyn GrantStore,
    pub clock: &'a dyn ClockPort,
}

impl Authorize<'_> {
    /// # Errors
    ///
    /// A failing grant store or policy provider; callers deny.
    pub fn decide(
        &self,
        subject: Option<Subject>,
        action: &str,
        resource: ResourceRef,
        facts: BTreeSet<Condition>,
    ) -> Result<PolicyDecision, PortError> {
        let grants = match &subject {
            Some(subject) => load_chains(self.grants, subject, action)?,
            None => Vec::new(),
        };
        self.policy.decide(&PolicyRequest {
            subject,
            action: action.to_owned(),
            resource,
            facts,
            grants,
            at_millis: self.clock.unix_millis(),
        })
    }
}

fn refused(message: &str) -> ApplicationError {
    ApplicationError::Denied(message.to_owned())
}

/// Issue a root grant. The issuer's authority (`capability.issue`) is decided by policy
/// before this runs; this checks the grant against the registry.
pub struct IssueGrant<'a> {
    pub grants: &'a dyn GrantStore,
    pub registry: &'a ActionRegistry,
    pub clock: &'a dyn ClockPort,
}

impl IssueGrant<'_> {
    /// # Errors
    ///
    /// `Denied` for a grant the registry does not allow, or a failing store.
    pub fn execute(&self, grant: &Grant) -> Result<(), ApplicationError> {
        if grant.parent.is_some() {
            return Err(refused("a root grant has no parent; delegate instead"));
        }
        if grant.actions.is_empty() || !grant.delegable_actions.is_subset(&grant.actions) {
            return Err(refused("delegable actions must be granted actions"));
        }
        for action in &grant.actions {
            let registered = self
                .registry
                .actions
                .get(action)
                .ok_or_else(|| refused("unknown action"))?;
            if registered.resource != grant.resource.kind() {
                return Err(refused("the action does not apply to the resource"));
            }
            if !registered.subjects.contains(&grant.subject.kind) {
                return Err(refused("the subject class may not hold the action"));
            }
            if grant.delegable_actions.contains(action) && !registered.delegable {
                return Err(refused("the action is not delegable"));
            }
        }
        if grant
            .expires_at_millis
            .is_some_and(|expires| expires <= self.clock.unix_millis())
        {
            return Err(refused("the grant has already expired"));
        }
        match self.grants.put(grant) {
            Err(PortError::Conflict) => Err(refused("the grant already exists")),
            other => Ok(other?),
        }
    }
}

/// Delegate an attenuated grant from one the delegator holds (plan section "Child
/// workloads").
pub struct DelegateGrant<'a> {
    pub grants: &'a dyn GrantStore,
    pub registry: &'a ActionRegistry,
    pub clock: &'a dyn ClockPort,
}

impl DelegateGrant<'_> {
    /// # Errors
    ///
    /// `Denied` naming the attenuation rule refused, or a failing store.
    pub fn execute(
        &self,
        delegator: &Subject,
        parent: Uuid,
        request: &DelegationRequest,
        policy_version: &str,
    ) -> Result<Grant, ApplicationError> {
        let parent = self
            .grants
            .grant(parent)?
            .ok_or_else(|| refused("unknown parent grant"))?;
        let chain =
            chain_of(self.grants, parent)?.ok_or_else(|| refused("the parent chain is broken"))?;
        let child = delegate(
            self.registry,
            &chain,
            delegator,
            request,
            policy_version,
            self.clock.unix_millis(),
        )
        .map_err(|error| ApplicationError::Denied(error.to_string()))?;
        match self.grants.put(&child) {
            Err(PortError::Conflict) => Err(refused("the grant already exists")),
            other => other.map(|()| child).map_err(Into::into),
        }
    }
}

/// Revoke a grant. Its descendants stop authorizing at once, because every chain is
/// checked to its root; the returned closure (the grant and its descendants) is what
/// must be re-evaluated, for example to suspend workloads.
pub struct RevokeGrant<'a> {
    pub grants: &'a dyn GrantStore,
    pub clock: &'a dyn ClockPort,
}

impl RevokeGrant<'_> {
    /// # Errors
    ///
    /// `Denied` for an unknown grant, or a failing store.
    pub fn execute(&self, id: Uuid) -> Result<Vec<Uuid>, ApplicationError> {
        match self.grants.revoke(id, self.clock.unix_millis()) {
            Err(PortError::NotFound) => return Err(refused("unknown grant")),
            other => other?,
        }
        let mut closure = vec![id];
        let mut pending = VecDeque::from([id]);
        while let Some(next) = pending.pop_front() {
            for child in self.grants.children(next)? {
                if closure.len() >= MAX_REVOCATION_CLOSURE {
                    return Ok(closure);
                }
                if !closure.contains(&child.id) {
                    closure.push(child.id);
                    pending.push_back(child.id);
                }
            }
        }
        Ok(closure)
    }
}

/// Whether a stored grant still authorizes anything at `now` (its whole chain).
///
/// # Errors
///
/// A failing store.
pub fn grant_is_live(store: &dyn GrantStore, id: Uuid, now_millis: i64) -> Result<bool, PortError> {
    let Some(grant) = store.grant(id)? else {
        return Ok(false);
    };
    Ok(chain_of(store, grant)?.is_some_and(|chain| check_chain(&chain, now_millis).is_ok()))
}

/// A root grant over one resource, for callers that build grants.
#[must_use]
pub fn exact(kind: &str, id: &str) -> ResourceSelector {
    ResourceSelector::Exact {
        kind: kind.to_owned(),
        id: id.to_owned(),
    }
}

#[cfg(test)]
mod tests;
