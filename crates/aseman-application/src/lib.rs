//! Driver-independent Aseman application use cases.
#![forbid(unsafe_code)]

use aseman_domain::{DesiredWorkloadState, WorkloadId};
use aseman_ports::{
    ClockPort, PeerDirectoryPort, PolicyDecisionPort, PortError, ServerIdentityPort, VmmPort,
    WorkloadRepository,
};
use thiserror::Error;

pub mod storage_migration;

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
    pub vmm: &'a dyn VmmPort,
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
    pub fn execute(
        &self,
        actor: &str,
        workload_id: WorkloadId,
        state: DesiredWorkloadState,
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
        let decision = self
            .policy
            .authorize(actor, action, &workload_id.to_string())?;
        if !decision.allowed {
            return Err(ApplicationError::Denied(decision.reason));
        }
        let expected = workload.generation;
        workload.generation = expected.next()?;
        workload.state = state;
        self.workloads.put_desired(&workload, expected)?;
        self.vmm.apply_desired(&workload)?;
        Ok(workload.generation.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::{CreatureId, DesiredWorkload, Generation, ProgramId};
    use aseman_ports::{PolicyDecision, PortResult};
    use std::sync::Mutex;

    struct Harness {
        workload: Mutex<Option<DesiredWorkload>>,
        allow: bool,
        applied: Mutex<Vec<DesiredWorkload>>,
    }

    impl WorkloadRepository for Harness {
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
        fn authorize(
            &self,
            _subject: &str,
            _action: &str,
            _resource: &str,
        ) -> PortResult<PolicyDecision> {
            Ok(PolicyDecision {
                allowed: self.allow,
                policy_version: "test-1".into(),
                reason: "test-policy".into(),
            })
        }
    }

    impl VmmPort for Harness {
        fn apply_desired(&self, workload: &DesiredWorkload) -> PortResult<()> {
            self.applied
                .lock()
                .expect("test mutex")
                .push(workload.clone());
            Ok(())
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
                    generation: Generation::INITIAL,
                    state: DesiredWorkloadState::Stopped,
                })),
                allow,
                applied: Mutex::new(Vec::new()),
            },
            id,
        )
    }

    #[test]
    fn denied_transition_has_no_repository_or_vmm_effect() {
        let (harness, id) = harness(false);
        let use_case = SetDesiredWorkloadState {
            workloads: &harness,
            policy: &harness,
            vmm: &harness,
        };
        assert!(matches!(
            use_case.execute("user", id, DesiredWorkloadState::Running),
            Err(ApplicationError::Denied(_))
        ));
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
    fn allowed_transition_increments_generation_before_vmm_apply() {
        let (harness, id) = harness(true);
        let use_case = SetDesiredWorkloadState {
            workloads: &harness,
            policy: &harness,
            vmm: &harness,
        };
        assert_eq!(
            use_case
                .execute("user", id, DesiredWorkloadState::Running)
                .unwrap(),
            2
        );
        let applied = harness.applied.lock().unwrap();
        assert_eq!(applied[0].generation.get(), 2);
        assert_eq!(applied[0].state, DesiredWorkloadState::Running);
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
