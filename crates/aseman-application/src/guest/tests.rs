use super::*;
use aseman_domain::authority::{
    ActionClass, ActionRegistry, PolicyDecision, PolicyRequest, RegisteredAction, evaluate,
};
use aseman_domain::capability::Grant;
use aseman_domain::guest::LegacyKvNamespace;
use aseman_domain::identity::{
    CredentialWindow, FreshnessPolicy, IdentityKey, KeyDescription, KeyEpoch, KeyPurpose,
    RotationPolicy, SignatureContext,
};
use aseman_domain::{
    CreatureDatabaseBinding, CreatureId, DesiredWorkload, Generation, ProgramId, Uuid,
};
use aseman_ports::{PortError, PortResult};
use std::collections::BTreeMap;
use std::sync::Mutex;

const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct World {
    workloads: BTreeMap<WorkloadId, DesiredWorkload>,
    bindings: BTreeMap<CreatureId, CreatureDatabaseBinding>,
    /// (creature of the binding used, namespace, key) -> value.
    data: Mutex<BTreeMap<(CreatureId, &'static str, String), String>>,
    keys: BTreeMap<String, IdentityKey>,
    nonces: Mutex<Vec<Vec<u8>>>,
}

impl WorkloadRepository for World {
    fn get_desired(&self, id: WorkloadId) -> PortResult<Option<DesiredWorkload>> {
        Ok(self.workloads.get(&id).cloned())
    }
    fn put_desired(&self, _: &DesiredWorkload, _: Generation) -> PortResult<()> {
        unreachable!()
    }
}

impl CreatureDatabaseBindings for World {
    fn binding_for(&self, creature: CreatureId) -> PortResult<Option<CreatureDatabaseBinding>> {
        Ok(self.bindings.get(&creature).cloned())
    }
    fn record_binding(&self, _: &CreatureDatabaseBinding) -> PortResult<()> {
        unreachable!()
    }
}

impl GuestKv for World {
    fn execute(
        &self,
        binding: &CreatureDatabaseBinding,
        operation: &GuestKvOperation,
    ) -> PortResult<GuestKvOutcome> {
        let mut data = self.data.lock().unwrap();
        let slot = |namespace: LegacyKvNamespace, key: &str| {
            (binding.creature_id, namespace.as_str(), key.to_owned())
        };
        Ok(match operation {
            GuestKvOperation::Get { namespace, key } => GuestKvOutcome::Value {
                value: data.get(&slot(*namespace, key)).cloned(),
            },
            GuestKvOperation::Put {
                namespace,
                key,
                value,
            } => {
                data.insert(slot(*namespace, key), value.clone());
                GuestKvOutcome::Written
            }
            GuestKvOperation::Delete { namespace, key } => GuestKvOutcome::Deleted {
                existed: data.remove(&slot(*namespace, key)).is_some(),
            },
            GuestKvOperation::List { .. } => GuestKvOutcome::Listed { pairs: Vec::new() },
            _ => GuestKvOutcome::Keys { keys: Vec::new() },
        })
    }
}

impl GrantStore for World {
    fn grant(&self, _: Uuid) -> PortResult<Option<Grant>> {
        Ok(None)
    }
    fn grants_of(&self, _: &Subject) -> PortResult<Vec<Grant>> {
        Ok(Vec::new())
    }
    fn children(&self, _: Uuid) -> PortResult<Vec<Grant>> {
        Ok(Vec::new())
    }
    fn put(&self, _: &Grant) -> PortResult<()> {
        unreachable!()
    }
    fn revoke(&self, _: Uuid, _: i64) -> PortResult<()> {
        unreachable!()
    }
}

impl ClockPort for World {
    fn unix_millis(&self) -> i64 {
        NOW
    }
}

impl KeyDirectory for World {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
        Ok(self.keys.get(key_id).cloned())
    }
    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>> {
        Ok(self
            .keys
            .values()
            .filter(|key| key.epoch.subject == *subject && key.epoch.purpose == purpose)
            .cloned()
            .collect())
    }
    fn register(&self, _: &IdentityKey) -> PortResult<()> {
        unreachable!()
    }
    fn retire(&self, _: &str, _: i64) -> PortResult<()> {
        unreachable!()
    }
    fn revoke(&self, _: &str, _: i64) -> PortResult<()> {
        unreachable!()
    }
}

impl ReplayGuard for World {
    fn record_nonce(&self, _: &str, nonce: &[u8], _: i64, _: i64) -> PortResult<bool> {
        let mut nonces = self.nonces.lock().unwrap();
        if nonces.iter().any(|used| used == nonce) {
            return Ok(false);
        }
        nonces.push(nonce.to_vec());
        Ok(true)
    }
}

impl IdentityVerifier for World {
    fn verify(&self, proof: &Proof, _: &IdentityKey) -> Result<(), AuthenticationError> {
        (proof.signature == b"good")
            .then_some(())
            .ok_or(AuthenticationError::BadSignature)
    }
    fn body_digest(&self, _: &[u8]) -> [u8; 32] {
        [0; 32]
    }
    fn describe_key(&self, _: &[u8]) -> Result<KeyDescription, AuthenticationError> {
        unreachable!()
    }
    fn introduction_bytes(&self, _: &aseman_domain::identity::Introduction) -> Vec<u8> {
        unreachable!()
    }
}

struct Policy(ActionRegistry);

impl PolicyDecisionPort for Policy {
    fn decide(&self, request: &PolicyRequest) -> PortResult<PolicyDecision> {
        Ok(evaluate(&self.0, request, "p1"))
    }
}

fn policy() -> Policy {
    let action = RegisteredAction {
        id: GUEST_DATA_ACTION.to_owned(),
        resource: "guest_data".to_owned(),
        class: ActionClass::Write,
        subjects: [SubjectKind::Workload].into(),
        rule: vec![Condition::SameCreature],
        delegable: true,
    };
    Policy(ActionRegistry {
        version: "r1".to_owned(),
        actions: BTreeMap::from([(action.id.clone(), action)]),
    })
}

fn workload_subject(n: u128) -> Subject {
    Subject {
        kind: SubjectKind::Workload,
        id: Uuid::from_u128(n),
    }
}

/// Workloads 1 and 2 belong to creatures A and B, both with active bindings.
fn world() -> (World, [CreatureId; 2]) {
    let mut world = World::default();
    let creatures = [CreatureId::new(), CreatureId::new()];
    for (n, creature) in [(1, creatures[0]), (2, creatures[1])] {
        let id = WorkloadId::from_uuid(Uuid::from_u128(n));
        world.workloads.insert(
            id,
            DesiredWorkload {
                id,
                creature_id: creature,
                program_id: ProgramId::new(),
                generation: Generation::INITIAL,
                state: DesiredWorkloadState::Running,
            },
        );
        let mut binding = CreatureDatabaseBinding::new(
            creature,
            "postgres-guest-v1".to_owned(),
            format!("db{n}"),
            format!("role{n}"),
        )
        .unwrap();
        binding.status = BindingStatus::Active;
        world.bindings.insert(creature, binding);
    }
    (world, creatures)
}

fn put(key: &str, value: &str) -> GuestKvOperation {
    GuestKvOperation::Put {
        namespace: LegacyKvNamespace::DbOp,
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn get(key: &str) -> GuestKvOperation {
    GuestKvOperation::Get {
        namespace: LegacyKvNamespace::DbOp,
        key: key.to_owned(),
    }
}

fn run(
    world: &World,
    who: Subject,
    action: &str,
    operation: &GuestKvOperation,
) -> Result<GuestKvOutcome, IdentityFailure> {
    let policy = policy();
    GuestGateway {
        workloads: world,
        bindings: world,
        policy: &policy,
        grants: world,
        clock: world,
        kv: world,
    }
    .execute(who, action, operation)
}

#[test]
fn each_workload_reaches_only_its_own_creature_database() {
    let (world, creatures) = world();
    let (a, b) = (workload_subject(1), workload_subject(2));
    run(&world, a, GUEST_DATA_ACTION, &put("profile", "alice")).unwrap();
    run(&world, b, GUEST_DATA_ACTION, &put("profile", "bob")).unwrap();
    assert_eq!(
        run(&world, a, GUEST_DATA_ACTION, &get("profile")),
        Ok(GuestKvOutcome::Value {
            value: Some("alice".to_owned())
        })
    );
    assert_eq!(
        run(&world, b, GUEST_DATA_ACTION, &get("profile")),
        Ok(GuestKvOutcome::Value {
            value: Some("bob".to_owned())
        })
    );
    // Every write went to the writer's own creature, whatever the key.
    let data = world.data.lock().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[&(creatures[0], "dbop", "profile".to_owned())], "alice");
    assert_eq!(data[&(creatures[1], "dbop", "profile".to_owned())], "bob");
}

#[test]
fn the_gateway_refuses_everything_it_cannot_resolve_or_authorize() {
    let (mut world, creatures) = world();
    let a = workload_subject(1);
    let refused = |result: Result<GuestKvOutcome, IdentityFailure>| match result {
        Err(IdentityFailure::Refused(reason)) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    };
    // Only workloads, only the signed action, only valid operations.
    let user = Subject {
        kind: SubjectKind::User,
        ..a
    };
    assert_eq!(
        refused(run(&world, user, GUEST_DATA_ACTION, &get("k"))),
        "only workloads use the guest API"
    );
    assert_eq!(
        refused(run(&world, a, "store.signal", &get("k"))),
        "the credential does not cover this operation"
    );
    assert_eq!(
        refused(run(&world, a, GUEST_DATA_ACTION, &get(""))),
        "the operation exceeds the guest limits"
    );
    // Unknown and deleted workloads.
    assert_eq!(
        refused(run(
            &world,
            workload_subject(9),
            GUEST_DATA_ACTION,
            &get("k")
        )),
        "unknown workload"
    );
    // A disabled binding serves nothing.
    world.bindings.get_mut(&creatures[0]).unwrap().status = BindingStatus::Disabled;
    assert_eq!(
        refused(run(&world, a, GUEST_DATA_ACTION, &get("k"))),
        "the creature's guest database is not active"
    );
    world.bindings.remove(&creatures[0]);
    assert_eq!(
        refused(run(&world, a, GUEST_DATA_ACTION, &get("k"))),
        "the creature's guest database is not active"
    );
    world
        .workloads
        .get_mut(&WorkloadId::from_uuid(Uuid::from_u128(1)))
        .unwrap()
        .state = DesiredWorkloadState::Deleted;
    assert_eq!(
        refused(run(&world, a, GUEST_DATA_ACTION, &get("k"))),
        "the workload is deleted"
    );
    assert!(world.data.lock().unwrap().is_empty());
}

#[test]
fn signed_requests_authenticate_the_workload_key_before_the_gateway() {
    let (mut world, creatures) = world();
    let a = workload_subject(1);
    world.keys.insert(
        "zQmWorkload".to_owned(),
        IdentityKey {
            key_id: "zQmWorkload".to_owned(),
            public_key: Vec::new(),
            epoch: KeyEpoch {
                subject: a,
                purpose: KeyPurpose::Authentication,
                epoch: 1,
                not_before_millis: NOW - 1_000,
                expires_at_millis: None,
                retired_at_millis: None,
                revoked_at_millis: None,
                legacy: false,
            },
        },
    );
    let proof = Proof {
        context: SignatureContext::Request,
        algorithm: "ed25519".to_owned(),
        key_id: "zQmWorkload".to_owned(),
        key_epoch: 1,
        subject: a,
        audience: "node:a/guest/v1".to_owned(),
        window: CredentialWindow {
            issued_at_millis: NOW,
            not_before_millis: NOW,
            expires_at_millis: NOW + 60_000,
        },
        nonce: vec![3; 16],
        request_id: "r1".to_owned(),
        action: GUEST_DATA_ACTION.to_owned(),
        resource: String::new(),
        body_digest: [0; 32],
        signature: b"good".to_vec(),
    };
    let verifier_policy = VerifierPolicy {
        audience: "node:a/guest/v1".to_owned(),
        freshness: FreshnessPolicy::GUEST,
        rotation: RotationPolicy::DEFAULT,
    };
    let policy = policy();
    let serve = ServeSignedGuestRequest {
        keys: &world,
        replay: &world,
        verifier: &world,
        gateway: GuestGateway {
            workloads: &world,
            bindings: &world,
            policy: &policy,
            grants: &world,
            clock: &world,
            kv: &world,
        },
    };
    assert_eq!(
        serve.execute(&proof, b"{}", &put("k", "v"), &verifier_policy),
        Ok(GuestKvOutcome::Written)
    );
    assert!(
        world
            .data
            .lock()
            .unwrap()
            .contains_key(&(creatures[0], "dbop", "k".to_owned()))
    );
    // A replayed proof, a forged signature, or another subject's key are refused.
    assert_eq!(
        serve.execute(&proof, b"{}", &put("k", "v2"), &verifier_policy),
        Err(AuthenticationError::Replayed.into())
    );
    let forged = Proof {
        nonce: vec![4; 16],
        signature: b"forged".to_vec(),
        ..proof.clone()
    };
    assert_eq!(
        serve.execute(&forged, b"{}", &put("k", "v2"), &verifier_policy),
        Err(AuthenticationError::BadSignature.into())
    );
    let impersonation = Proof {
        nonce: vec![5; 16],
        subject: workload_subject(2),
        ..proof
    };
    assert_eq!(
        serve.execute(&impersonation, b"{}", &put("k", "v2"), &verifier_policy),
        Err(AuthenticationError::KeySubjectMismatch.into())
    );
    assert_eq!(
        world.data.lock().unwrap()[&(creatures[0], "dbop", "k".to_owned())],
        "v"
    );
}

#[test]
fn a_failing_workload_directory_is_reported_as_unavailable() {
    struct Down;
    impl WorkloadRepository for Down {
        fn get_desired(&self, _: WorkloadId) -> PortResult<Option<DesiredWorkload>> {
            Err(PortError::Failed("down".to_owned()))
        }
        fn put_desired(&self, _: &DesiredWorkload, _: Generation) -> PortResult<()> {
            unreachable!()
        }
    }
    let (world, _) = world();
    let policy = policy();
    let result = GuestGateway {
        workloads: &Down,
        bindings: &world,
        policy: &policy,
        grants: &world,
        clock: &world,
        kv: &world,
    }
    .execute(workload_subject(1), GUEST_DATA_ACTION, &get("k"));
    assert!(matches!(result, Err(IdentityFailure::Unavailable(_))));
}
