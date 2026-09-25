use std::collections::BTreeMap;
use std::sync::Mutex;

use aseman_domain::identity::{
    AuthenticationError, CredentialWindow, FreshnessPolicy, IdentityKey, Introduction,
    KeyDescription, RotationPolicy, SignatureContext,
};
use aseman_domain::vmm::{
    Artifact, ArtifactKind, Bootstrap, NetworkPolicy, OperationKind, OperationRecord, Resources,
    WorkloadRecord,
};
use aseman_domain::{CreatureId, Generation, OperationId, OperationState, ProgramId, Uuid};
use aseman_ports::PortResult;
use aseman_ports::vmm::{BackendDescription, EventBatch, LifecycleCommand, Page, WorkloadFilter};

use super::*;

const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct World {
    workloads: Mutex<BTreeMap<WorkloadId, DesiredWorkload>>,
    keys: Mutex<BTreeMap<String, IdentityKey>>,
    nonces: Mutex<Vec<Vec<u8>>>,
    served: Mutex<Vec<(GuestCaller, String, String)>>,
    created: Mutex<Vec<(NewWorkload, String)>>,
}

impl WorkloadRepository for World {
    fn create_desired(&self, workload: &DesiredWorkload) -> PortResult<()> {
        let mut workloads = self.workloads.lock().unwrap();
        if workloads.contains_key(&workload.id) {
            return Err(PortError::Conflict);
        }
        workloads.insert(workload.id, workload.clone());
        Ok(())
    }
    fn get_desired(&self, id: WorkloadId) -> PortResult<Option<DesiredWorkload>> {
        Ok(self.workloads.lock().unwrap().get(&id).cloned())
    }
    fn put_desired(&self, workload: &DesiredWorkload, _: Generation) -> PortResult<()> {
        self.workloads
            .lock()
            .unwrap()
            .insert(workload.id, workload.clone());
        Ok(())
    }
}

impl KeyDirectory for World {
    fn key(&self, key_id: &str) -> PortResult<Option<IdentityKey>> {
        Ok(self.keys.lock().unwrap().get(key_id).cloned())
    }
    fn epochs(&self, subject: &Subject, purpose: KeyPurpose) -> PortResult<Vec<IdentityKey>> {
        let mut epochs: Vec<IdentityKey> = self
            .keys
            .lock()
            .unwrap()
            .values()
            .filter(|key| key.epoch.subject == *subject && key.epoch.purpose == purpose)
            .cloned()
            .collect();
        epochs.sort_by_key(|key| key.epoch.epoch);
        Ok(epochs)
    }
    fn register(&self, key: &IdentityKey) -> PortResult<()> {
        let mut keys = self.keys.lock().unwrap();
        if keys.contains_key(&key.key_id) {
            return Err(PortError::Conflict);
        }
        keys.insert(key.key_id.clone(), key.clone());
        Ok(())
    }
    fn retire(&self, key_id: &str, at_millis: i64) -> PortResult<()> {
        let mut keys = self.keys.lock().unwrap();
        let key = keys.get_mut(key_id).ok_or(PortError::NotFound)?;
        key.epoch.retired_at_millis.get_or_insert(at_millis);
        Ok(())
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
    fn describe_key(&self, public_key: &[u8]) -> Result<KeyDescription, AuthenticationError> {
        Ok(KeyDescription {
            key_id: format!(
                "zQm{}",
                public_key
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            legacy: false,
        })
    }
    fn introduction_bytes(&self, _: &Introduction) -> Vec<u8> {
        unreachable!()
    }
}

impl ClockPort for World {
    fn unix_millis(&self) -> i64 {
        NOW
    }
}

impl LegacyWorkloadRefs for World {
    fn legacy_refs(&self, _: &DesiredWorkload) -> PortResult<(String, String)> {
        Ok(("7@global".to_owned(), "9@global".to_owned()))
    }
}

impl GuestHostCalls for World {
    fn call(&self, caller: &GuestCaller, op: &str, input: &str) -> PortResult<String> {
        self.served
            .lock()
            .unwrap()
            .push((caller.clone(), op.to_owned(), input.to_owned()));
        Ok("{\"ok\":true}".to_owned())
    }
    fn artifact(&self, _: &GuestCaller, digest: &str) -> PortResult<Vec<u8>> {
        Ok(digest.as_bytes().to_vec())
    }
}

impl VmmClient for World {
    fn capabilities(&self) -> PortResult<BackendDescription> {
        unreachable!()
    }
    fn create(&self, workload: &NewWorkload, key: &str) -> PortResult<OperationRecord> {
        self.created
            .lock()
            .unwrap()
            .push((workload.clone(), key.to_owned()));
        Ok(OperationRecord {
            owner: "node".to_owned(),
            id: OperationId::new(),
            workload_id: Some(workload.id),
            kind: OperationKind::Create,
            state: OperationState::Pending,
            generation: Some(workload.desired.generation),
            request: None,
            created_at_millis: NOW,
            updated_at_millis: NOW,
            deadline_millis: None,
            result: None,
            error: None,
        })
    }
    fn workload(&self, _: WorkloadId) -> PortResult<Option<WorkloadRecord>> {
        unreachable!()
    }
    fn workloads(
        &self,
        _: &WorkloadFilter,
        _: Option<&str>,
        _: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        unreachable!()
    }
    fn command(
        &self,
        _: WorkloadId,
        _: LifecycleCommand,
        _: Generation,
        _: &str,
    ) -> PortResult<OperationRecord> {
        unreachable!()
    }
    fn update_spec(
        &self,
        _: WorkloadId,
        _: &WorkloadSpec,
        _: Generation,
        _: &str,
    ) -> PortResult<OperationRecord> {
        unreachable!()
    }
    fn invoke(&self, _: WorkloadId, _: &str, _: &str) -> PortResult<OperationRecord> {
        unreachable!()
    }
    fn forward_http(&self, _: WorkloadId, _: &str, _: &str) -> PortResult<String> {
        unreachable!()
    }
    fn usage(&self, _: WorkloadId) -> PortResult<aseman_domain::vmm::Usage> {
        unreachable!()
    }
    fn operation(&self, _: OperationId) -> PortResult<Option<OperationRecord>> {
        unreachable!()
    }
    fn events_after(&self, _: u64, _: usize) -> PortResult<EventBatch> {
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

fn workload() -> DesiredWorkload {
    DesiredWorkload {
        id: WorkloadId::from_uuid(Uuid::from_u128(1)),
        creature_id: CreatureId::from_uuid(Uuid::from_u128(2)),
        program_id: ProgramId::from_uuid(Uuid::from_u128(3)),
        name: "main/vm-1".to_owned(),
        runtime: "wasm".to_owned(),
        generation: Generation::INITIAL,
        state: DesiredWorkloadState::Running,
    }
}

fn spec() -> WorkloadSpec {
    WorkloadSpec {
        runtime: "wasm".to_owned(),
        artifact: Artifact {
            kind: ArtifactKind::Blob,
            reference: "programs/9/main/module.wasm".to_owned(),
            digest: format!("sha256:{}", "a".repeat(64)),
        },
        entry: "module.wasm".to_owned(),
        resources: Resources {
            vcpu_millis: 100,
            memory_mib: 64,
            disk_mib: None,
            invocation_timeout_millis: None,
        },
        network: NetworkPolicy::default(),
        environment: BTreeMap::new(),
        bootstrap: Bootstrap {
            guest_api_url: "https://node".to_owned(),
            credential: None,
        },
    }
}

fn labels() -> WorkloadLabels {
    WorkloadLabels {
        creature_id: Uuid::from_u128(2),
        program_id: Uuid::from_u128(3),
        entity_id: "main".to_owned(),
        legacy_machine_id: Some("9@global".to_owned()),
        legacy_vm_id: Some("vm-1".to_owned()),
    }
}

fn provision(world: &World, public_key: Vec<u8>) -> OperationRecord {
    let credential = |epoch: u32| Ok(WriteOnlyCredential::new(format!("credential-at-{epoch}")));
    ProvisionWorkload {
        workloads: world,
        keys: world,
        verifier: world,
        clock: world,
        vmm: world,
    }
    .execute(
        &workload(),
        labels(),
        spec(),
        &WorkloadKey {
            public_key,
            credential_for_epoch: &credential,
        },
    )
    .unwrap()
}

#[test]
fn provisioning_records_registers_and_creates_and_is_safe_to_repeat() {
    let world = World::default();
    let operation = provision(&world, vec![1, 2, 3]);
    assert_eq!(operation.workload_id, Some(workload().id));
    assert_eq!(world.get_desired(workload().id).unwrap(), Some(workload()));
    let subject = Subject {
        kind: SubjectKind::Workload,
        id: *workload().id.as_uuid(),
    };
    let epochs = world.epochs(&subject, KeyPurpose::Authentication).unwrap();
    assert_eq!(epochs.len(), 1);
    assert_eq!(epochs[0].epoch.epoch, 1);
    let (created, key) = world.created.lock().unwrap()[0].clone();
    assert_eq!(key, format!("create-{}", workload().id));
    assert_eq!(
        created.spec.bootstrap.credential,
        Some(WriteOnlyCredential::new("credential-at-1".to_owned()))
    );
    // Again, as after a crash: the record stays, a new epoch replaces the key, and the
    // VMM sees the same idempotency key.
    provision(&world, vec![4, 5, 6]);
    let epochs = world.epochs(&subject, KeyPurpose::Authentication).unwrap();
    assert_eq!(epochs.len(), 2);
    assert!(epochs[0].epoch.retired_at_millis.is_some());
    assert_eq!(epochs[1].epoch.epoch, 2);
    let (created, key) = world.created.lock().unwrap()[1].clone();
    assert_eq!(key, format!("create-{}", workload().id));
    assert_eq!(
        created.spec.bootstrap.credential,
        Some(WriteOnlyCredential::new("credential-at-2".to_owned()))
    );
}

fn proof(world: &World, nonce: u8, action: &str, resource: &str) -> Proof {
    let key = world
        .epochs(
            &Subject {
                kind: SubjectKind::Workload,
                id: *workload().id.as_uuid(),
            },
            KeyPurpose::Authentication,
        )
        .unwrap()
        .pop()
        .unwrap();
    Proof {
        context: SignatureContext::Request,
        algorithm: "ed25519".to_owned(),
        key_id: key.key_id,
        key_epoch: key.epoch.epoch,
        subject: key.epoch.subject,
        audience: "node:a/guest/v1".to_owned(),
        window: CredentialWindow {
            issued_at_millis: NOW,
            not_before_millis: NOW,
            expires_at_millis: NOW + 60_000,
        },
        nonce: vec![nonce; 16],
        request_id: "r".to_owned(),
        action: action.to_owned(),
        resource: resource.to_owned(),
        body_digest: [0; 32],
        signature: b"good".to_vec(),
    }
}

#[test]
fn guest_calls_run_as_the_resolved_workload_only() {
    let world = World::default();
    provision(&world, vec![1]);
    let policy = VerifierPolicy {
        audience: "node:a/guest/v1".to_owned(),
        freshness: FreshnessPolicy::GUEST,
        rotation: RotationPolicy::DEFAULT,
    };
    let serve = ServeGuestCall {
        keys: &world,
        replay: &world,
        verifier: &world,
        clock: &world,
        workloads: &world,
        refs: &world,
        calls: &world,
    };
    let call = GuestRequest::Call {
        op: "genId",
        input: "{}",
    };
    assert_eq!(
        serve.execute(
            &proof(&world, 1, "node.id.generate", "genId"),
            b"{}",
            call,
            "node.id.generate",
            &policy
        ),
        Ok(b"{\"ok\":true}".to_vec())
    );
    let (caller, op, _) = world.served.lock().unwrap()[0].clone();
    assert_eq!(op, "genId");
    assert_eq!(
        (caller.creature_ref.as_str(), caller.program_ref.as_str()),
        ("7@global", "9@global")
    );
    assert_eq!(caller.entity_and_instance(), ("main", "vm-1"));
    let refused = Err(IdentityFailure::Refused(
        "the credential does not cover this operation",
    ));
    // Another action, or the same action for another call.
    assert_eq!(
        serve.execute(
            &proof(&world, 2, "store.signal", "genId"),
            b"{}",
            call,
            "node.id.generate",
            &policy
        ),
        refused
    );
    assert_eq!(
        serve.execute(
            &proof(&world, 3, "node.id.generate", "signal"),
            b"{}",
            call,
            "node.id.generate",
            &policy
        ),
        refused
    );
    // A replayed proof.
    assert_eq!(
        serve.execute(
            &proof(&world, 1, "node.id.generate", "genId"),
            b"{}",
            call,
            "node.id.generate",
            &policy
        ),
        Err(AuthenticationError::Replayed.into())
    );
    // A deleted workload.
    let mut deleted = workload();
    deleted.state = DesiredWorkloadState::Deleted;
    world.put_desired(&deleted, Generation::INITIAL).unwrap();
    assert_eq!(
        serve.execute(
            &proof(&world, 4, "node.id.generate", "genId"),
            b"{}",
            call,
            "node.id.generate",
            &policy
        ),
        Err(IdentityFailure::Refused("the workload is deleted"))
    );
    assert_eq!(world.served.lock().unwrap().len(), 1);
}
