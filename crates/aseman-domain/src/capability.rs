//! Capability grants (A403, ADR 0008): issuance, delegation by intersection only, expiry,
//! explanation through the parent chain, and descendant revocation. Pure: callers load
//! the chains; nothing here performs I/O.
//!
//! A chain is a grant followed by its ancestors up to its root. It authorizes only while
//! every link is valid: in its time window, unrevoked, correctly linked, and attenuating
//! its parent. Revoking any ancestor therefore voids every descendant at once.

use crate::authority::ActionRegistry;
use crate::identity::Subject;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;
use uuid::Uuid;

/// The resources a grant covers.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum ResourceSelector {
    /// One resource.
    Exact { kind: String, id: String },
    /// Every resource of a type. Only an administrator issues such a root.
    AnyOfKind { kind: String },
}

impl ResourceSelector {
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::Exact { kind, .. } | Self::AnyOfKind { kind } => kind,
        }
    }

    /// Whether this selector covers the resource `kind`/`id`.
    #[must_use]
    pub fn covers(&self, kind: &str, id: &str) -> bool {
        match self {
            Self::Exact {
                kind: own_kind,
                id: own_id,
            } => own_kind == kind && own_id == id,
            Self::AnyOfKind { kind: own_kind } => own_kind == kind,
        }
    }

    /// The largest selector inside both, if any.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::AnyOfKind { kind: left }, Self::AnyOfKind { kind: right }) => {
                (left == right).then(|| self.clone())
            }
            (Self::AnyOfKind { kind }, exact @ Self::Exact { .. })
            | (exact @ Self::Exact { .. }, Self::AnyOfKind { kind }) => {
                (exact.kind() == kind).then(|| exact.clone())
            }
            (Self::Exact { .. }, Self::Exact { .. }) => (self == other).then(|| self.clone()),
        }
    }

    /// Whether every resource this selector covers is covered by `outer`.
    #[must_use]
    pub fn within(&self, outer: &Self) -> bool {
        self.intersect(outer).as_ref() == Some(self)
    }
}

/// One capability grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub id: Uuid,
    pub subject: Subject,
    pub issuer: Subject,
    pub actions: BTreeSet<String>,
    pub resource: ResourceSelector,
    /// The actions the subject may delegate onward; a subset of `actions`.
    pub delegable_actions: BTreeSet<String>,
    /// How many further delegation levels may follow this grant.
    pub max_depth: u32,
    pub parent: Option<Uuid>,
    pub not_before_millis: i64,
    pub expires_at_millis: Option<i64>,
    pub revoked_at_millis: Option<i64>,
    pub policy_version: String,
}

impl Grant {
    fn live_at(&self, now_millis: i64) -> bool {
        self.not_before_millis <= now_millis
            && self
                .expires_at_millis
                .is_none_or(|expires| now_millis < expires)
            && self
                .revoked_at_millis
                .is_none_or(|revoked| now_millis < revoked)
    }
}

/// Why a grant chain does not authorize, or why a delegation was refused.
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityError {
    #[error("the grant chain is empty or broken")]
    BrokenChain,
    #[error("a grant in the chain is not live")]
    NotLive,
    #[error("a grant in the chain widens its parent")]
    Amplifies,
    #[error("the delegator does not hold the parent grant")]
    NotHolder,
    #[error("nothing requested can be delegated")]
    NothingDelegable,
    #[error("the requested resources are outside the parent grant")]
    ResourceOutside,
    #[error("the parent grant allows no further delegation")]
    DepthExhausted,
    #[error("the subject class may not hold the action")]
    SubjectNotAllowed,
    #[error("an action is not registered")]
    UnknownAction,
}

/// Whether `child` stays inside `parent`: same holder link, fewer or equal actions,
/// a narrower or equal resource, a later or equal start, an earlier or equal expiry,
/// and a smaller depth budget.
fn attenuates(child: &Grant, parent: &Grant) -> bool {
    child.parent == Some(parent.id)
        && child.issuer == parent.subject
        && child.actions.is_subset(&parent.delegable_actions)
        && child.delegable_actions.is_subset(&child.actions)
        && child.resource.within(&parent.resource)
        && child.not_before_millis >= parent.not_before_millis
        && match (child.expires_at_millis, parent.expires_at_millis) {
            (_, None) => true,
            (Some(child_expiry), Some(parent_expiry)) => child_expiry <= parent_expiry,
            (None, Some(_)) => false,
        }
        && parent.max_depth >= 1
        && child.max_depth < parent.max_depth
}

/// Check a chain (the grant, then its ancestors to the root) at `now`.
///
/// # Errors
///
/// `BrokenChain`, `NotLive`, or `Amplifies`.
pub fn check_chain(chain: &[Grant], now_millis: i64) -> Result<(), CapabilityError> {
    let (root, _) = chain.split_last().ok_or(CapabilityError::BrokenChain)?;
    if root.parent.is_some() || !root.delegable_actions.is_subset(&root.actions) {
        return Err(CapabilityError::BrokenChain);
    }
    for grant in chain {
        if !grant.live_at(now_millis) {
            return Err(CapabilityError::NotLive);
        }
    }
    for pair in chain.windows(2) {
        if pair[0].parent != Some(pair[1].id) {
            return Err(CapabilityError::BrokenChain);
        }
        if !attenuates(&pair[0], &pair[1]) {
            return Err(CapabilityError::Amplifies);
        }
    }
    Ok(())
}

/// Whether a chain authorizes `subject` to perform `action` on `kind`/`id` at `now`.
#[must_use]
pub fn chain_authorizes(
    chain: &[Grant],
    subject: &Subject,
    action: &str,
    kind: &str,
    id: &str,
    now_millis: i64,
) -> bool {
    chain.first().is_some_and(|grant| {
        grant.subject == *subject
            && grant.actions.contains(action)
            && grant.resource.covers(kind, id)
    }) && check_chain(chain, now_millis).is_ok()
}

/// A delegation request by the holder of a parent grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DelegationRequest {
    pub id: Uuid,
    pub subject: Subject,
    pub actions: BTreeSet<String>,
    pub resource: ResourceSelector,
    pub delegable_actions: BTreeSet<String>,
    pub max_depth: u32,
    pub not_before_millis: i64,
    pub expires_at_millis: Option<i64>,
}

/// Delegate from the chain whose head the `delegator` holds (plan section "Child
/// workloads"): the child receives the intersection of what was requested, what the
/// parent may delegate, and what the registry lets the child's class hold and delegate.
/// Time, depth, and resources can only narrow.
///
/// # Errors
///
/// As [`check_chain`] for the parent, `NotHolder`, `DepthExhausted`, `ResourceOutside`,
/// `NothingDelegable`, `UnknownAction`, or `SubjectNotAllowed`.
pub fn delegate(
    registry: &ActionRegistry,
    parent_chain: &[Grant],
    delegator: &Subject,
    request: &DelegationRequest,
    policy_version: &str,
    now_millis: i64,
) -> Result<Grant, CapabilityError> {
    check_chain(parent_chain, now_millis)?;
    let parent = &parent_chain[0];
    if parent.subject != *delegator {
        return Err(CapabilityError::NotHolder);
    }
    if parent.max_depth == 0 {
        return Err(CapabilityError::DepthExhausted);
    }
    let resource = request
        .resource
        .intersect(&parent.resource)
        .ok_or(CapabilityError::ResourceOutside)?;
    let mut actions = BTreeSet::new();
    for action in request.actions.intersection(&parent.delegable_actions) {
        let registered = registry
            .actions
            .get(action)
            .ok_or(CapabilityError::UnknownAction)?;
        if !registered.delegable || registered.resource != resource.kind() {
            continue;
        }
        if !registered.subjects.contains(&request.subject.kind) {
            return Err(CapabilityError::SubjectNotAllowed);
        }
        actions.insert(action.clone());
    }
    if actions.is_empty() {
        return Err(CapabilityError::NothingDelegable);
    }
    let delegable_actions = request
        .delegable_actions
        .intersection(&actions)
        .cloned()
        .collect();
    let expires_at_millis = match (request.expires_at_millis, parent.expires_at_millis) {
        (Some(requested), Some(limit)) => Some(requested.min(limit)),
        (requested, None) => requested,
        (None, limit) => limit,
    };
    Ok(Grant {
        id: request.id,
        subject: request.subject,
        issuer: *delegator,
        actions,
        resource,
        delegable_actions,
        max_depth: request.max_depth.min(parent.max_depth - 1),
        parent: Some(parent.id),
        not_before_millis: request.not_before_millis.max(parent.not_before_millis),
        expires_at_millis,
        revoked_at_millis: None,
        policy_version: policy_version.to_owned(),
    })
}

/// The grants that a revocation of `revoked` voids: itself and every descendant among
/// `grants`, for re-evaluation and suspension of affected workloads.
#[must_use]
pub fn revocation_closure(grants: &[Grant], revoked: Uuid) -> BTreeSet<Uuid> {
    let mut closure = BTreeSet::from([revoked]);
    loop {
        let before = closure.len();
        for grant in grants {
            if grant.parent.is_some_and(|parent| closure.contains(&parent)) {
                closure.insert(grant.id);
            }
        }
        if closure.len() == before {
            return closure;
        }
    }
}

#[cfg(test)]
mod tests;
