//! Driver-independent Aseman application use cases.
#![forbid(unsafe_code)]

use aseman_domain::authority::{Condition, ResourceRef};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_domain::{DesiredWorkloadState, WorkloadId};
use aseman_ports::vmm::{LifecycleCommand, VmmClient};
use aseman_ports::{
    ClockPort, GrantStore, PeerDirectoryPort, PolicyDecisionPort, PortError, ServerIdentityPort,
    WorkloadRepository,
};
use std::collections::BTreeSet;
use thiserror::Error;

pub mod capability;
pub mod creature;
pub mod guest;
pub mod guest_call;
pub mod identity;
pub mod program;
pub mod storage_migration;
pub mod store;
pub mod vmm;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ApplicationError {
    #[error("workload not found")]
    WorkloadNotFound,
    #[error("operation denied: {0}")]
    Denied(String),
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    Domain(#[from] aseman_domain::DomainError),
}

pub struct SetDesiredWorkloadState<'a> {
    pub workloads: &'a dyn WorkloadRepository,
    pub policy: &'a dyn PolicyDecisionPort,
    pub grants: &'a dyn GrantStore,
    pub vmm: &'a dyn VmmClient,
    pub clock: &'a dyn ClockPort,
}

/// Transport-neutral diagnostics use cases backing the compatibility `/api/*` family.
pub struct Diagnostics<'a> {
    pub clock: &'a dyn ClockPort,
    pub advertised_port: &'a str,
}

pub struct GetServerPublicKey<'a> {
    pub identity: &'a dyn ServerIdentityPort,
}

pub struct GetServerPeers<'a> {
    pub peers: &'a dyn PeerDirectoryPort,
}

/// Transport-neutral classification for the legacy session shortcuts. All other paths
/// enter the ordinary authorized action dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRoute {
    Authenticate,
    Logout,
    Action,
}

#[must_use]
pub fn classify_session_route(path: &str) -> SessionRoute {
    match path {
        "authenticate" | "/creatures/authenticate" => SessionRoute::Authenticate,
        "logout" => SessionRoute::Logout,
        _ => SessionRoute::Action,
    }
}

impl Diagnostics<'_> {
    #[must_use]
    pub fn hello(&self, name: &str) -> String {
        format!("hello {name} !")
    }

    #[must_use]
    pub fn time_millis(&self) -> i64 {
        self.clock.unix_millis()
    }

    #[must_use]
    pub fn ping(&self) -> &str {
        self.advertised_port
    }
}

impl GetServerPublicKey<'_> {
    pub fn execute(&self) -> Result<String, ApplicationError> {
        self.identity.server_public_key().map_err(Into::into)
    }
}

impl GetServerPeers<'_> {
    pub fn execute(&self) -> Result<Vec<String>, ApplicationError> {
        self.peers.peer_servers().map_err(Into::into)
    }
}

impl SetDesiredWorkloadState<'_> {
    /// `established` holds facts the enforcement layer resolved for `actor` on this
    /// workload (for example `owner` for the user owning its program); the workload's
    /// own creature is always its owner.
    pub fn execute(
        &self,
        actor: Subject,
        workload_id: WorkloadId,
        state: DesiredWorkloadState,
        established: &BTreeSet<Condition>,
    ) -> Result<u64, ApplicationError> {
        let mut workload = self
            .workloads
            .get_desired(workload_id)?
            .ok_or(ApplicationError::WorkloadNotFound)?;
        let action = match state {
            DesiredWorkloadState::Running => "workload.start",
            DesiredWorkloadState::Stopped => "workload.stop",
            DesiredWorkloadState::Paused => "workload.pause",
            DesiredWorkloadState::Deleted => "workload.delete",
        };
        // The workload's own creature owns it; other relations are resolved by the
        // enforcement layer (P4-05).
        let owner = Subject {
            kind: SubjectKind::Creature,
            id: *workload.creature_id.as_uuid(),
        };
        let mut facts = established.clone();
        if actor == owner {
            facts.insert(Condition::Owner);
        }
        let decision = capability::Authorize {
            policy: self.policy,
            grants: self.grants,
            clock: self.clock,
        }
        .decide(
            Some(actor),
            action,
            ResourceRef {
                kind: "workload".to_owned(),
                id: workload_id.to_string(),
            },
            facts,
        )?;
        if !decision.allowed {
            return Err(ApplicationError::Denied(decision.reason.code().to_owned()));
        }
        let expected = workload.generation;
        let command = match (workload.state, state) {
            (DesiredWorkloadState::Paused, DesiredWorkloadState::Running) => {
                LifecycleCommand::Resume
            }
            (_, DesiredWorkloadState::Running) => LifecycleCommand::Start,
            (_, DesiredWorkloadState::Stopped) => LifecycleCommand::Stop,
            (_, DesiredWorkloadState::Paused) => LifecycleCommand::Pause,
            (_, DesiredWorkloadState::Deleted) => LifecycleCommand::Delete,
        };
        workload.generation = expected.next()?;
        workload.state = state;
        self.workloads.put_desired(&workload, expected)?;
        // Desired state is recorded first; the key names the generation, so a retry
        // after a crash between the two is deduplicated by the VMM (A503).
        self.vmm.command(
            workload_id,
            command,
            workload.generation,
            &format!("desired-{workload_id}-{}", workload.generation.get()),
        )?;
        Ok(workload.generation.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::authority::{DecisionReason, PolicyDecision, PolicyRequest};
    use aseman_domain::capability::Grant;
    use aseman_domain::{CreatureId, DesiredWorkload, Generation, ProgramId};
    use aseman_ports::PortResult;
    use std::sync::Mutex;

    struct Harness {
        workload: Mutex<Option<DesiredWorkload>>,
        allow: bool,
        applied: Mutex<Vec<(WorkloadId, LifecycleCommand, Generation, String)>>,
        asked: Mutex<Vec<PolicyRequest>>,
    }

    impl WorkloadRepository for Harness {
        fn create_desired(&self, _: &DesiredWorkload) -> PortResult<()> {
            unreachable!()
        }
        fn get_desired(&self, _id: WorkloadId) -> PortResult<Option<DesiredWorkload>> {
            Ok(self.workload.lock().expect("test mutex").clone())
        }
        fn put_desired(&self, workload: &DesiredWorkload, expected: Generation) -> PortResult<()> {
            let mut stored = self.workload.lock().expect("test mutex");
            if stored.as_ref().map(|item| item.generation) != Some(expected) {
                return Err(PortError::Conflict);
            }
            *stored = Some(workload.clone());
            Ok(())
        }
    }

    impl PolicyDecisionPort for Harness {
        /// Allows owners when `allow` is set; records what was asked.
        fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision> {
            self.asked.lock().expect("test mutex").push(request.clone());
            let allowed = self.allow && request.facts.contains(&Condition::Owner);
            Ok(PolicyDecision {
                allowed,
                reason: if allowed {
                    DecisionReason::Allowed
                } else {
                    DecisionReason::ConditionNotMet
                },
                matched: allowed.then_some(Condition::Owner),
                considered: vec![Condition::Owner],
                grant_chain: Vec::new(),
                registry_version: "test".into(),
                policy_version: "test-1".into(),
            })
        }
    }

    impl GrantStore for Harness {
        fn grant(&self, _: aseman_domain::Uuid) -> PortResult<Option<Grant>> {
            Ok(None)
        }
        fn grants_of(&self, _: &Subject) -> PortResult<Vec<Grant>> {
            Ok(Vec::new())
        }
        fn children(&self, _: aseman_domain::Uuid) -> PortResult<Vec<Grant>> {
            Ok(Vec::new())
        }
        fn put(&self, _: &Grant) -> PortResult<()> {
            unreachable!()
        }
        fn revoke(&self, _: aseman_domain::Uuid, _: i64) -> PortResult<()> {
            unreachable!()
        }
    }

    impl aseman_ports::vmm::VmmClient for Harness {
        fn capabilities(&self) -> PortResult<aseman_ports::vmm::BackendDescription> {
            unreachable!()
        }
        fn create(
            &self,
            _: &aseman_ports::vmm::NewWorkload,
            _: &str,
        ) -> PortResult<aseman_domain::vmm::OperationRecord> {
            unreachable!()
        }
        fn workload(
            &self,
            _: WorkloadId,
        ) -> PortResult<Option<aseman_domain::vmm::WorkloadRecord>> {
            unreachable!()
        }
        fn workloads(
            &self,
            _: &aseman_ports::vmm::WorkloadFilter,
            _: Option<&str>,
            _: usize,
        ) -> PortResult<aseman_ports::vmm::Page<aseman_domain::vmm::WorkloadRecord>> {
            unreachable!()
        }
        fn command(
            &self,
            id: WorkloadId,
            command: LifecycleCommand,
            generation: Generation,
            key: &str,
        ) -> PortResult<aseman_domain::vmm::OperationRecord> {
            self.applied.lock().expect("test mutex").push((
                id,
                command,
                generation,
                key.to_owned(),
            ));
            Err(PortError::Unavailable("not needed"))
        }
        fn update_spec(
            &self,
            _: WorkloadId,
            _: &aseman_domain::vmm::WorkloadSpec,
            _: Generation,
            _: &str,
        ) -> PortResult<aseman_domain::vmm::OperationRecord> {
            unreachable!()
        }
        fn invoke(
            &self,
            _: WorkloadId,
            _: &str,
            _: &str,
        ) -> PortResult<aseman_domain::vmm::OperationRecord> {
            unreachable!()
        }
        fn forward_http(&self, _: WorkloadId, _: &str, _: &str) -> PortResult<String> {
            unreachable!()
        }
        fn operation(
            &self,
            _: aseman_domain::OperationId,
        ) -> PortResult<Option<aseman_domain::vmm::OperationRecord>> {
            unreachable!()
        }
        fn events_after(&self, _: u64, _: usize) -> PortResult<aseman_ports::vmm::EventBatch> {
            unreachable!()
        }
        fn exec(
            &self,
            _: WorkloadId,
            _: &str,
            _: &str,
        ) -> PortResult<aseman_domain::vmm::OperationRecord> {
            unreachable!()
        }
        fn build(&self, _: &str, _: &str) -> PortResult<aseman_domain::vmm::OperationRecord> {
            unreachable!()
        }
        fn put_file(&self, _: WorkloadId, _: &str, _: &[u8], _: &str) -> PortResult<()> {
            unreachable!()
        }
        fn get_file(&self, _: WorkloadId, _: &str) -> PortResult<Vec<u8>> {
            unreachable!()
        }
        fn endpoints(&self, _: WorkloadId) -> PortResult<Vec<aseman_domain::vmm::Endpoint>> {
            unreachable!()
        }
        fn verify(&self, _: &str, _: &str, _: &str) -> PortResult<String> {
            unreachable!()
        }
        fn logs(&self, _: WorkloadId, _: u64) -> PortResult<Vec<aseman_domain::vmm::LogRecord>> {
            unreachable!()
        }
    }

    impl ClockPort for Harness {
        fn unix_millis(&self) -> i64 {
            1_700_000_000_123
        }
    }

    impl ServerIdentityPort for Harness {
        fn server_public_key(&self) -> PortResult<String> {
            Ok("public-key".to_owned())
        }
    }

    impl PeerDirectoryPort for Harness {
        fn peer_servers(&self) -> PortResult<Vec<String>> {
            Ok(vec!["node-a:1337".to_owned(), "node-b:1337".to_owned()])
        }
    }

    fn harness(allow: bool) -> (Harness, WorkloadId) {
        let id = WorkloadId::new();
        (
            Harness {
                workload: Mutex::new(Some(DesiredWorkload {
                    id,
                    creature_id: CreatureId::new(),
                    program_id: ProgramId::new(),
                    name: "main/vm".to_owned(),
                    runtime: "wasm".to_owned(),
                    generation: Generation::INITIAL,
                    state: DesiredWorkloadState::Stopped,
                })),
                allow,
                applied: Mutex::new(Vec::new()),
                asked: Mutex::new(Vec::new()),
            },
            id,
        )
    }

    fn owner(harness: &Harness) -> Subject {
        Subject {
            kind: SubjectKind::Creature,
            id: *harness
                .workload
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .creature_id
                .as_uuid(),
        }
    }

    #[test]
    fn denied_transition_has_no_repository_or_vmm_effect() {
        let (harness, id) = harness(false);
        let use_case = SetDesiredWorkloadState {
            workloads: &harness,
            policy: &harness,
            grants: &harness,
            vmm: &harness,
            clock: &harness,
        };
        let stranger = Subject {
            kind: SubjectKind::User,
            id: *CreatureId::new().as_uuid(),
        };
        assert_eq!(
            use_case.execute(
                stranger,
                id,
                DesiredWorkloadState::Running,
                &BTreeSet::new()
            ),
            Err(ApplicationError::Denied("condition_not_met".to_owned()))
        );
        assert_eq!(
            harness
                .workload
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .generation,
            Generation::INITIAL
        );
        assert!(harness.applied.lock().unwrap().is_empty());
    }

    #[test]
    fn allowed_transition_records_desired_before_commanding_the_vmm() {
        let (harness, id) = harness(true);
        let use_case = SetDesiredWorkloadState {
            workloads: &harness,
            policy: &harness,
            grants: &harness,
            vmm: &harness,
            clock: &harness,
        };
        // The VMM is unreachable here: desired state is recorded anyway, and the retry
        // reuses the same key.
        assert_eq!(
            use_case.execute(
                owner(&harness),
                id,
                DesiredWorkloadState::Running,
                &BTreeSet::new()
            ),
            Err(ApplicationError::Port(PortError::Unavailable("not needed")))
        );
        assert_eq!(
            harness
                .workload
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .generation
                .get(),
            2
        );
        let asked = harness.asked.lock().unwrap();
        assert_eq!(
            (asked[0].action.as_str(), asked[0].resource.kind.as_str()),
            ("workload.start", "workload")
        );
        assert_eq!(asked[0].at_millis, 1_700_000_000_123);
        let applied = harness.applied.lock().unwrap();
        assert_eq!(
            applied[0],
            (
                id,
                LifecycleCommand::Start,
                Generation::from_stored(2).unwrap(),
                format!("desired-{id}-2")
            )
        );
    }

    #[test]
    fn diagnostics_are_transport_neutral_and_clock_injected() {
        let (harness, _) = harness(true);
        let diagnostics = Diagnostics {
            clock: &harness,
            advertised_port: "4000",
        };
        assert_eq!(diagnostics.hello("Aseman"), "hello Aseman !");
        assert_eq!(diagnostics.time_millis(), 1_700_000_000_123);
        assert_eq!(diagnostics.ping(), "4000");
    }

    #[test]
    fn session_shortcuts_are_identical_for_every_transport() {
        assert_eq!(
            classify_session_route("authenticate"),
            SessionRoute::Authenticate
        );
        assert_eq!(
            classify_session_route("/creatures/authenticate"),
            SessionRoute::Authenticate
        );
        assert_eq!(classify_session_route("logout"), SessionRoute::Logout);
        assert_eq!(
            classify_session_route("/programs/list"),
            SessionRoute::Action
        );
    }

    #[test]
    fn auth_bootstrap_use_cases_depend_only_on_narrow_ports() {
        let (harness, _) = harness(true);
        assert_eq!(
            GetServerPublicKey { identity: &harness }.execute().unwrap(),
            "public-key"
        );
        assert_eq!(
            GetServerPeers { peers: &harness }.execute().unwrap(),
            vec!["node-a:1337", "node-b:1337"]
        );
    }
}
