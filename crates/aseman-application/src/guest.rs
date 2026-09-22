//! The guest gateway (A405, P4-04). A workload reaches its own creature's guest database
//! and nothing else:
//!
//! 1. The workload is authenticated: by an A401 proof ([`ServeSignedGuestRequest`]), or,
//!    for an in-process runtime, by the VM handle the node registered when it started
//!    the workload. It is never identified by anything the guest sends.
//! 2. The operation must be the action the workload signed.
//! 3. Workload, then program, then creature, then binding are resolved server-side from
//!    the trusted records. The binding must be active.
//! 4. Policy decides `guest_data.access` on the resolved creature's guest data, with
//!    `same_creature` established by that resolution.
//! 5. The operation runs on that binding.

use crate::capability::Authorize;
use crate::identity::{AuthenticateProof, IdentityFailure, VerifierPolicy};
use aseman_domain::authority::{Condition, ResourceRef};
use aseman_domain::guest::{GUEST_DATA_ACTION, GuestKvOperation, GuestKvOutcome};
use aseman_domain::identity::{AuthenticationError, Proof, Subject, SubjectKind};
use aseman_domain::{BindingStatus, DesiredWorkloadState, WorkloadId};
use aseman_ports::{
    ClockPort, CreatureDatabaseBindings, GrantStore, GuestKv, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, ReplayGuard, WorkloadRepository,
};
use std::collections::BTreeSet;

/// Serves the guest operations of an authenticated workload.
pub struct GuestGateway<'a> {
    pub workloads: &'a dyn WorkloadRepository,
    pub bindings: &'a dyn CreatureDatabaseBindings,
    pub policy: &'a dyn PolicyDecisionPort,
    pub grants: &'a dyn GrantStore,
    pub clock: &'a dyn ClockPort,
    pub kv: &'a dyn GuestKv,
}

impl GuestGateway<'_> {
    /// Run `operation` for `workload`, which the caller has authenticated. `signed_action`
    /// is the action the workload's credential covers.
    ///
    /// # Errors
    ///
    /// `Refused` when the workload, its creature's binding, or the operation does not
    /// qualify; `Rejected` with the policy reason code; or `Unavailable`.
    pub fn execute(
        &self,
        workload: Subject,
        signed_action: &str,
        operation: &GuestKvOperation,
    ) -> Result<GuestKvOutcome, IdentityFailure> {
        if workload.kind != SubjectKind::Workload {
            return Err(IdentityFailure::Refused("only workloads use the guest API"));
        }
        if signed_action != GUEST_DATA_ACTION {
            return Err(IdentityFailure::Refused(
                "the credential does not cover this operation",
            ));
        }
        if !operation.is_valid() {
            return Err(IdentityFailure::Refused(
                "the operation exceeds the guest limits",
            ));
        }
        let placement = self
            .workloads
            .get_desired(WorkloadId::from_uuid(workload.id))?
            .ok_or(IdentityFailure::Refused("unknown workload"))?;
        if placement.state == DesiredWorkloadState::Deleted {
            return Err(IdentityFailure::Refused("the workload is deleted"));
        }
        let binding = self
            .bindings
            .binding_for(placement.creature_id)?
            .filter(|binding| binding.status == BindingStatus::Active)
            .ok_or(IdentityFailure::Refused(
                "the creature's guest database is not active",
            ))?;
        let decision = Authorize {
            policy: self.policy,
            grants: self.grants,
            clock: self.clock,
        }
        .decide(
            Some(workload),
            GUEST_DATA_ACTION,
            ResourceRef {
                kind: "guest_data".to_owned(),
                id: placement.creature_id.to_string(),
            },
            BTreeSet::from([Condition::SameCreature]),
        )?;
        if !decision.allowed {
            return Err(IdentityFailure::Refused(
                "the policy denies guest data access",
            ));
        }
        Ok(self.kv.execute(&binding, operation)?)
    }
}

/// A signed guest request: an A401 proof over the operation's body, then the gateway.
pub struct ServeSignedGuestRequest<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub replay: &'a dyn ReplayGuard,
    pub verifier: &'a dyn IdentityVerifier,
    pub gateway: GuestGateway<'a>,
}

impl ServeSignedGuestRequest<'_> {
    /// `body` is the exact request body the proof signs; `operation` is its parse.
    ///
    /// # Errors
    ///
    /// The A401 code of a failing proof, or as [`GuestGateway::execute`].
    pub fn execute(
        &self,
        proof: &Proof,
        body: &[u8],
        operation: &GuestKvOperation,
        policy: &VerifierPolicy,
    ) -> Result<GuestKvOutcome, IdentityFailure> {
        let workload = AuthenticateProof {
            keys: self.keys,
            replay: self.replay,
            verifier: self.verifier,
            clock: self.gateway.clock,
        }
        .execute(proof, body, policy)?;
        if workload.kind != SubjectKind::Workload {
            return Err(AuthenticationError::KeySubjectMismatch.into());
        }
        self.gateway.execute(workload, &proof.action, operation)
    }
}

#[cfg(test)]
mod tests;
