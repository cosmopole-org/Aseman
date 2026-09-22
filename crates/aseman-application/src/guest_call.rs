//! Guest host calls over the network (A405 "Host calls", P5-04), and provisioning of
//! the workloads that make them.
//!
//! A remote runtime acts for a workload with that workload's own key. The node
//! authenticates the A401 proof, requires the signed action to be the one the call is
//! registered as, resolves the workload's creature and program from the trusted
//! records, and only then serves the call. Nothing in the request selects an identity.

use aseman_domain::identity::{KeyPurpose, Proof, Subject, SubjectKind};
use aseman_domain::vmm::{
    DesiredStatus, OperationRecord, WorkloadLabels, WorkloadSpec, WriteOnlyCredential,
};
use aseman_domain::{DesiredWorkload, DesiredWorkloadState, WorkloadId};
use aseman_ports::guest::{GuestCaller, GuestHostCalls, LegacyWorkloadRefs};
use aseman_ports::vmm::{NewWorkload, VmmClient};
use aseman_ports::{
    ClockPort, IdentityVerifier, KeyDirectory, PortError, ReplayGuard, WorkloadRepository,
};

use crate::identity::{AuthenticateProof, IdentityFailure, NewKey, RotateKey, VerifierPolicy};

/// What a guest asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestRequest<'r> {
    /// A host call with its JSON input.
    Call { op: &'r str, input: &'r str },
    /// An artifact of the workload's own program.
    Artifact { digest: &'r str },
}

impl GuestRequest<'_> {
    fn resource(&self) -> &str {
        match self {
            Self::Call { op, .. } => op,
            Self::Artifact { digest } => digest,
        }
    }
}

pub struct ServeGuestCall<'a> {
    pub keys: &'a dyn KeyDirectory,
    pub replay: &'a dyn ReplayGuard,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
    pub workloads: &'a dyn WorkloadRepository,
    pub refs: &'a dyn LegacyWorkloadRefs,
    pub calls: &'a dyn GuestHostCalls,
}

impl ServeGuestCall<'_> {
    /// Serve `request`, whose exact body is `body`. `action` is the action the request
    /// is registered as (A402); the proof must have signed exactly that.
    ///
    /// # Errors
    ///
    /// The A401 code of a failing proof; `Refused` when the credential, workload, or
    /// request does not qualify; or `Unavailable`.
    pub fn execute(
        &self,
        proof: &Proof,
        body: &[u8],
        request: GuestRequest<'_>,
        action: &str,
        policy: &VerifierPolicy,
    ) -> Result<Vec<u8>, IdentityFailure> {
        let subject = AuthenticateProof {
            keys: self.keys,
            replay: self.replay,
            verifier: self.verifier,
            clock: self.clock,
        }
        .execute(proof, body, policy)?;
        if subject.kind != SubjectKind::Workload {
            return Err(IdentityFailure::Refused("only workloads use the guest API"));
        }
        if proof.action != action || proof.resource != request.resource() {
            return Err(IdentityFailure::Refused(
                "the credential does not cover this operation",
            ));
        }
        let workload = self
            .workloads
            .get_desired(WorkloadId::from_uuid(subject.id))?
            .ok_or(IdentityFailure::Refused("unknown workload"))?;
        if workload.state == DesiredWorkloadState::Deleted {
            return Err(IdentityFailure::Refused("the workload is deleted"));
        }
        let (creature_ref, program_ref) = self.refs.legacy_refs(&workload)?;
        let caller = GuestCaller {
            workload,
            creature_ref,
            program_ref,
        };
        Ok(match request {
            GuestRequest::Call { op, input } => self.calls.call(&caller, op, input)?.into_bytes(),
            GuestRequest::Artifact { digest } => self.calls.artifact(&caller, digest)?,
        })
    }
}

/// Record a workload, register its signing key, and ask the VMM to create it.
pub struct ProvisionWorkload<'a> {
    pub workloads: &'a dyn WorkloadRepository,
    pub keys: &'a dyn KeyDirectory,
    pub verifier: &'a dyn IdentityVerifier,
    pub clock: &'a dyn ClockPort,
    pub vmm: &'a dyn VmmClient,
}

/// The workload's key: its public encoding, and the credential for the epoch it is
/// registered at.
pub struct WorkloadKey<'k> {
    pub public_key: Vec<u8>,
    pub credential_for_epoch: &'k dyn Fn(u32) -> Result<WriteOnlyCredential, PortError>,
}

impl ProvisionWorkload<'_> {
    /// Provisioning is safe to repeat: an existing record is kept, a fresh key epoch
    /// replaces the previous one, and the VMM deduplicates the create by the workload.
    ///
    /// # Errors
    ///
    /// `Refused` for an invalid key, or `Unavailable`.
    pub fn execute(
        &self,
        workload: &DesiredWorkload,
        labels: WorkloadLabels,
        mut spec: WorkloadSpec,
        key: &WorkloadKey<'_>,
    ) -> Result<OperationRecord, IdentityFailure> {
        match self.workloads.create_desired(workload) {
            Ok(()) | Err(PortError::Conflict) => {}
            Err(error) => return Err(error.into()),
        }
        let subject = Subject {
            kind: SubjectKind::Workload,
            id: *workload.id.as_uuid(),
        };
        let registered = RotateKey {
            keys: self.keys,
            verifier: self.verifier,
            clock: self.clock,
        }
        .execute(
            subject,
            KeyPurpose::Authentication,
            &NewKey {
                public_key: key.public_key.clone(),
                expires_at_millis: None,
            },
        )?;
        spec.bootstrap.credential = Some((key.credential_for_epoch)(registered.epoch.epoch)?);
        Ok(self.vmm.create(
            &NewWorkload {
                id: workload.id,
                labels,
                spec,
                desired: DesiredStatus {
                    state: workload.state,
                    generation: workload.generation,
                },
            },
            &format!("create-{}", workload.id),
        )?)
    }
}

#[cfg(test)]
mod tests;
