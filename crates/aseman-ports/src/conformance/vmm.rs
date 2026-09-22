//! VMM service store suite, with the in-memory reference stores and the scripted
//! reference backend that the service tests and the A504 conformance kit use.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aseman_domain::vmm::{
    Artifact, ArtifactKind, Bootstrap, DesiredStatus, Endpoint, LogRecord, LogStream,
    NetworkPolicy, Observation, OperationKind, OperationRecord, ReconcileAction, Resources,
    RuntimeCapabilities, Usage, WorkloadEventRecord, WorkloadEventType, WorkloadLabels,
    WorkloadRecord, WorkloadSpec,
};
use aseman_domain::{
    DesiredWorkloadState, Generation, ObservedWorkloadState, OperationId, OperationState, Uuid,
    WorkloadId,
};

use crate::vmm::{
    BackendDescription, EventBatch, IdempotencyClaim, IdempotencyStore, OperationFilter, Page,
    ReplayableResponse, VmmBackend, VmmEventLog, VmmOperationStore, VmmWorkloadStore,
    WorkloadFilter,
};
use crate::{PortError, PortResult};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct IdempotencyEntry {
    digest: [u8; 32],
    claimed_at_millis: i64,
    response: Option<ReplayableResponse>,
}

#[derive(Default)]
struct MemoryState {
    workloads: BTreeMap<WorkloadId, WorkloadRecord>,
    operations: BTreeMap<OperationId, OperationRecord>,
    idempotency: BTreeMap<(String, String), IdempotencyEntry>,
    events: Vec<WorkloadEventRecord>,
    next_sequence: u64,
    truncated_through: u64,
}

/// The reference VMM stores, in memory.
#[derive(Default)]
pub struct MemoryVmmStores {
    state: Mutex<MemoryState>,
}

fn page<T: Clone>(
    items: impl Iterator<Item = (String, T)>,
    cursor: Option<&str>,
    limit: usize,
) -> Page<T> {
    let mut rest = items.skip_while(|(key, _)| cursor.is_some_and(|cursor| key.as_str() != cursor));
    if cursor.is_some() {
        rest.next();
    }
    let taken: Vec<(String, T)> = rest.take(limit.max(1) + 1).collect();
    let more = taken.len() > limit.max(1);
    let items: Vec<(String, T)> = taken.into_iter().take(limit.max(1)).collect();
    Page {
        next_cursor: if more {
            items.last().map(|(key, _)| key.clone())
        } else {
            None
        },
        items: items.into_iter().map(|(_, item)| item).collect(),
    }
}

impl VmmWorkloadStore for MemoryVmmStores {
    fn workload(&self, owner: &str, id: WorkloadId) -> PortResult<Option<WorkloadRecord>> {
        Ok(lock(&self.state)
            .workloads
            .get(&id)
            .filter(|record| record.owner == owner)
            .cloned())
    }

    fn workloads(
        &self,
        owner: &str,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let state = lock(&self.state);
        Ok(page(
            state
                .workloads
                .values()
                .filter(|record| record.owner == owner)
                .filter(|record| {
                    filter
                        .creature_id
                        .is_none_or(|creature| record.labels.creature_id == creature)
                })
                .filter(|record| {
                    filter.observed_state.is_none_or(|wanted| {
                        record.observed.as_ref().map(|observed| observed.state) == Some(wanted)
                    })
                })
                .map(|record| (record.id.to_string(), record.clone())),
            cursor,
            limit,
        ))
    }

    fn all_workloads(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<WorkloadRecord>> {
        let state = lock(&self.state);
        Ok(page(
            state
                .workloads
                .values()
                .map(|record| (record.id.to_string(), record.clone())),
            cursor,
            limit,
        ))
    }

    fn insert_workload(&self, record: &WorkloadRecord) -> PortResult<()> {
        let mut state = lock(&self.state);
        if state.workloads.contains_key(&record.id) {
            return Err(PortError::Conflict);
        }
        state.workloads.insert(record.id, record.clone());
        Ok(())
    }

    fn replace_workload(&self, record: &WorkloadRecord, expected: u64) -> PortResult<()> {
        let mut state = lock(&self.state);
        match state.workloads.get(&record.id) {
            Some(stored) if stored.owner == record.owner && stored.resource_version == expected => {
                state.workloads.insert(record.id, record.clone());
                Ok(())
            }
            Some(_) => Err(PortError::Conflict),
            None => Err(PortError::NotFound),
        }
    }
}

impl VmmOperationStore for MemoryVmmStores {
    fn operation(&self, owner: &str, id: OperationId) -> PortResult<Option<OperationRecord>> {
        Ok(lock(&self.state)
            .operations
            .get(&id)
            .filter(|record| record.owner == owner)
            .cloned())
    }

    fn operations(
        &self,
        owner: &str,
        filter: &OperationFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<Page<OperationRecord>> {
        let state = lock(&self.state);
        let mut matching: Vec<&OperationRecord> = state
            .operations
            .values()
            .filter(|record| record.owner == owner)
            .filter(|record| {
                filter
                    .workload_id
                    .is_none_or(|workload| record.workload_id == Some(workload))
            })
            .filter(|record| filter.state.is_none_or(|wanted| record.state == wanted))
            .collect();
        matching.sort_by(|a, b| {
            (b.created_at_millis, b.id.to_string()).cmp(&(a.created_at_millis, a.id.to_string()))
        });
        Ok(page(
            matching
                .into_iter()
                .map(|record| (record.id.to_string(), record.clone())),
            cursor,
            limit,
        ))
    }

    fn insert_operation(&self, record: &OperationRecord) -> PortResult<()> {
        let mut state = lock(&self.state);
        if state.operations.contains_key(&record.id) {
            return Err(PortError::Conflict);
        }
        state.operations.insert(record.id, record.clone());
        Ok(())
    }

    fn replace_operation(
        &self,
        record: &OperationRecord,
        expected: OperationState,
    ) -> PortResult<()> {
        let mut state = lock(&self.state);
        match state.operations.get(&record.id) {
            Some(stored) if stored.owner == record.owner && stored.state == expected => {
                state.operations.insert(record.id, record.clone());
                Ok(())
            }
            Some(_) => Err(PortError::Conflict),
            None => Err(PortError::NotFound),
        }
    }

    fn unfinished_operations(&self, limit: usize) -> PortResult<Vec<OperationRecord>> {
        let state = lock(&self.state);
        let mut unfinished: Vec<&OperationRecord> = state
            .operations
            .values()
            .filter(|record| {
                matches!(
                    record.state,
                    OperationState::Pending | OperationState::Running
                )
            })
            .collect();
        unfinished.sort_by(|a, b| {
            (a.created_at_millis, a.id.to_string()).cmp(&(b.created_at_millis, b.id.to_string()))
        });
        Ok(unfinished.into_iter().take(limit.max(1)).cloned().collect())
    }
}

impl IdempotencyStore for MemoryVmmStores {
    fn claim(
        &self,
        owner: &str,
        key: &str,
        digest: [u8; 32],
        now_millis: i64,
        claim_ttl_millis: i64,
    ) -> PortResult<IdempotencyClaim> {
        let mut state = lock(&self.state);
        let slot = (owner.to_owned(), key.to_owned());
        match state.idempotency.get_mut(&slot) {
            Some(entry) if entry.digest != digest => Ok(IdempotencyClaim::Mismatch),
            Some(entry) => match &entry.response {
                Some(response) => Ok(IdempotencyClaim::Completed(response.clone())),
                None if now_millis - entry.claimed_at_millis >= claim_ttl_millis => {
                    entry.claimed_at_millis = now_millis;
                    Ok(IdempotencyClaim::Claimed)
                }
                None => Ok(IdempotencyClaim::InProgress),
            },
            None => {
                state.idempotency.insert(
                    slot,
                    IdempotencyEntry {
                        digest,
                        claimed_at_millis: now_millis,
                        response: None,
                    },
                );
                Ok(IdempotencyClaim::Claimed)
            }
        }
    }

    fn complete(&self, owner: &str, key: &str, response: &ReplayableResponse) -> PortResult<()> {
        let mut state = lock(&self.state);
        let entry = state
            .idempotency
            .get_mut(&(owner.to_owned(), key.to_owned()))
            .ok_or(PortError::NotFound)?;
        entry.response = Some(response.clone());
        Ok(())
    }

    fn release(&self, owner: &str, key: &str) -> PortResult<()> {
        let mut state = lock(&self.state);
        let slot = (owner.to_owned(), key.to_owned());
        if state
            .idempotency
            .get(&slot)
            .is_some_and(|entry| entry.response.is_none())
        {
            state.idempotency.remove(&slot);
        }
        Ok(())
    }

    fn purge_before(&self, cutoff_millis: i64) -> PortResult<u64> {
        let mut state = lock(&self.state);
        let before = state.idempotency.len();
        state
            .idempotency
            .retain(|_, entry| entry.claimed_at_millis >= cutoff_millis);
        Ok((before - state.idempotency.len()) as u64)
    }
}

impl VmmEventLog for MemoryVmmStores {
    fn append(&self, event: &WorkloadEventRecord) -> PortResult<u64> {
        let mut state = lock(&self.state);
        state.next_sequence += 1;
        let mut event = event.clone();
        event.sequence = state.next_sequence;
        state.events.push(event);
        Ok(state.next_sequence)
    }

    fn events_after(
        &self,
        owner: &str,
        after: u64,
        workload: Option<WorkloadId>,
        limit: usize,
    ) -> PortResult<EventBatch> {
        let state = lock(&self.state);
        if after < state.truncated_through {
            return Ok(EventBatch {
                events: Vec::new(),
                resync: true,
            });
        }
        Ok(EventBatch {
            events: state
                .events
                .iter()
                .filter(|event| event.sequence > after && event.owner == owner)
                .filter(|event| workload.is_none_or(|id| event.workload_id == id))
                .take(limit.max(1))
                .cloned()
                .collect(),
            resync: false,
        })
    }

    fn truncate_through(&self, sequence: u64) -> PortResult<u64> {
        let mut state = lock(&self.state);
        let before = state.events.len();
        state.events.retain(|event| event.sequence > sequence);
        state.truncated_through = state.truncated_through.max(sequence);
        Ok((before - state.events.len()) as u64)
    }
}

/// A workload record for tests.
#[must_use]
pub fn sample_workload(owner: &str, id: WorkloadId, runtime: &str) -> WorkloadRecord {
    WorkloadRecord {
        owner: owner.to_owned(),
        id,
        labels: WorkloadLabels {
            creature_id: Uuid::from_bytes([7; 16]),
            program_id: Uuid::from_bytes([8; 16]),
            entity_id: "main".to_owned(),
            legacy_machine_id: None,
            legacy_vm_id: None,
        },
        spec: WorkloadSpec {
            runtime: runtime.to_owned(),
            artifact: Artifact {
                kind: ArtifactKind::Blob,
                reference: "programs/p/main".to_owned(),
                digest: format!("sha256:{}", "0".repeat(64)),
            },
            entry: "main".to_owned(),
            resources: Resources {
                vcpu_millis: 100,
                memory_mib: 64,
                disk_mib: None,
                invocation_timeout_millis: None,
            },
            network: NetworkPolicy::default(),
            environment: BTreeMap::new(),
            bootstrap: Bootstrap {
                guest_api_url: "https://node.internal/guest".to_owned(),
                credential: None,
            },
        },
        desired: DesiredStatus {
            state: DesiredWorkloadState::Running,
            generation: Generation::INITIAL,
        },
        applied_generation: None,
        observed: None,
        resource_version: 1,
        created_at_millis: 1,
        updated_at_millis: 1,
    }
}

fn sample_operation(owner: &str, workload: WorkloadId, created: i64) -> OperationRecord {
    OperationRecord {
        owner: owner.to_owned(),
        id: OperationId::new(),
        workload_id: Some(workload),
        kind: OperationKind::Start,
        state: OperationState::Pending,
        generation: Some(Generation::INITIAL),
        request: None,
        created_at_millis: created,
        updated_at_millis: created,
        deadline_millis: None,
        result: None,
        error: None,
    }
}

/// Exercises the four VMM service stores, which must start empty.
///
/// # Panics
///
/// Panics when an adapter deviates from the port contract.
pub fn vmm_stores(
    workloads: &dyn VmmWorkloadStore,
    operations: &dyn VmmOperationStore,
    idempotency: &dyn IdempotencyStore,
    events: &dyn VmmEventLog,
) {
    // Workloads: owner scoping, compare-and-set, ID-ordered pagination.
    let mut ids: Vec<WorkloadId> = (0..5).map(|_| WorkloadId::new()).collect();
    ids.sort_by_key(ToString::to_string);
    for id in &ids {
        workloads
            .insert_workload(&sample_workload("node-a", *id, "wasm"))
            .unwrap();
    }
    let foreign = WorkloadId::new();
    workloads
        .insert_workload(&sample_workload("node-b", foreign, "wasm"))
        .unwrap();
    assert_eq!(
        workloads.insert_workload(&sample_workload("node-b", ids[0], "wasm")),
        Err(PortError::Conflict)
    );
    assert_eq!(workloads.workload("node-b", ids[0]), Ok(None));
    let mut record = workloads.workload("node-a", ids[0]).unwrap().unwrap();
    assert_eq!(record, sample_workload("node-a", ids[0], "wasm"));
    record.resource_version = 2;
    record.observed = Some(Observation {
        state: ObservedWorkloadState::Running,
        generation: Generation::INITIAL,
        sequence: 3,
        reason: Some("up".to_owned()),
        observed_at_millis: 9,
    });
    record.applied_generation = Some(Generation::INITIAL);
    assert_eq!(
        workloads.replace_workload(&record, 5),
        Err(PortError::Conflict)
    );
    workloads.replace_workload(&record, 1).unwrap();
    assert_eq!(
        workloads.replace_workload(&record, 1),
        Err(PortError::Conflict)
    );
    assert_eq!(
        workloads.workload("node-a", ids[0]).unwrap(),
        Some(record.clone())
    );
    let first = workloads
        .workloads("node-a", &WorkloadFilter::default(), None, 2)
        .unwrap();
    assert_eq!(
        first.items.iter().map(|w| w.id).collect::<Vec<_>>(),
        ids[..2]
    );
    let mut seen = first.items.len();
    let mut cursor = first.next_cursor;
    while let Some(next) = cursor {
        let page = workloads
            .workloads("node-a", &WorkloadFilter::default(), Some(&next), 2)
            .unwrap();
        seen += page.items.len();
        cursor = page.next_cursor;
    }
    assert_eq!(seen, 5);
    let running = workloads
        .workloads(
            "node-a",
            &WorkloadFilter {
                creature_id: None,
                observed_state: Some(ObservedWorkloadState::Running),
            },
            None,
            10,
        )
        .unwrap();
    assert_eq!(running.items, vec![record]);
    assert_eq!(workloads.all_workloads(None, 100).unwrap().items.len(), 6);

    // Operations: newest first, compare-and-set on state, unfinished oldest first.
    let older = sample_operation("node-a", ids[0], 10);
    let newer = sample_operation("node-a", ids[0], 20);
    let other = sample_operation("node-a", ids[1], 15);
    for operation in [&older, &newer, &other] {
        operations.insert_operation(operation).unwrap();
    }
    assert_eq!(
        operations.insert_operation(&older),
        Err(PortError::Conflict)
    );
    assert_eq!(operations.operation("node-b", older.id), Ok(None));
    let listed = operations
        .operations(
            "node-a",
            &OperationFilter {
                workload_id: Some(ids[0]),
                state: None,
            },
            None,
            10,
        )
        .unwrap();
    assert_eq!(listed.items, vec![newer.clone(), older.clone()]);
    let mut running = older.clone();
    running.state = OperationState::Running;
    assert_eq!(
        operations.replace_operation(&running, OperationState::Running),
        Err(PortError::Conflict)
    );
    operations
        .replace_operation(&running, OperationState::Pending)
        .unwrap();
    let mut done = newer.clone();
    done.state = OperationState::Succeeded;
    done.result = Some("{\"ok\":true}".to_owned());
    operations
        .replace_operation(&done, OperationState::Pending)
        .unwrap();
    assert_eq!(
        operations.operation("node-a", newer.id).unwrap(),
        Some(done)
    );
    assert_eq!(
        operations.unfinished_operations(10).unwrap(),
        vec![running, other]
    );

    // Idempotency: claim, in progress, mismatch, replay, release, takeover, purge.
    let digest = [1; 32];
    assert_eq!(
        idempotency.claim("node-a", "key-1", digest, 100, 1_000),
        Ok(IdempotencyClaim::Claimed)
    );
    assert_eq!(
        idempotency.claim("node-a", "key-1", digest, 200, 1_000),
        Ok(IdempotencyClaim::InProgress)
    );
    assert_eq!(
        idempotency.claim("node-a", "key-1", [2; 32], 200, 1_000),
        Ok(IdempotencyClaim::Mismatch)
    );
    assert_eq!(
        idempotency.claim("node-b", "key-1", [2; 32], 200, 1_000),
        Ok(IdempotencyClaim::Claimed),
        "keys are scoped by owner"
    );
    let response = ReplayableResponse {
        status: 202,
        body: b"{}".to_vec(),
        content_type: "application/json".to_owned(),
        location: Some("/v1/operations/x".to_owned()),
    };
    idempotency.complete("node-a", "key-1", &response).unwrap();
    assert_eq!(
        idempotency.claim("node-a", "key-1", digest, 300, 1_000),
        Ok(IdempotencyClaim::Completed(response.clone()))
    );
    idempotency.release("node-a", "key-1").unwrap();
    assert_eq!(
        idempotency.claim("node-a", "key-1", digest, 300, 1_000),
        Ok(IdempotencyClaim::Completed(response)),
        "a completed key is never released"
    );
    assert_eq!(
        idempotency.claim("node-a", "key-2", digest, 100, 1_000),
        Ok(IdempotencyClaim::Claimed)
    );
    idempotency.release("node-a", "key-2").unwrap();
    assert_eq!(
        idempotency.claim("node-a", "key-2", [3; 32], 150, 1_000),
        Ok(IdempotencyClaim::Claimed)
    );
    assert_eq!(
        idempotency.claim("node-a", "key-2", [3; 32], 1_150, 1_000),
        Ok(IdempotencyClaim::Claimed),
        "an abandoned claim is taken over"
    );
    assert_eq!(idempotency.purge_before(250), Ok(2));
    assert_eq!(
        idempotency.claim("node-a", "key-1", [9; 32], 400, 1_000),
        Ok(IdempotencyClaim::Claimed)
    );

    // Events: increasing sequences, owner and workload scoping, resync.
    let event = |owner: &str, workload| WorkloadEventRecord {
        owner: owner.to_owned(),
        sequence: 0,
        workload_id: workload,
        at_millis: 5,
        event_type: WorkloadEventType::Operation,
        observation: None,
        operation: Some(OperationId::new()),
    };
    let one = events.append(&event("node-a", ids[0])).unwrap();
    let two = events.append(&event("node-b", foreign)).unwrap();
    let three = events.append(&event("node-a", ids[1])).unwrap();
    assert!(one < two && two < three);
    let mine = events.events_after("node-a", 0, None, 10).unwrap();
    assert!(!mine.resync);
    assert_eq!(
        mine.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![one, three]
    );
    let only = events.events_after("node-a", 0, Some(ids[1]), 10).unwrap();
    assert_eq!(only.events.len(), 1);
    assert_eq!(only.events[0].workload_id, ids[1]);
    assert_eq!(
        events
            .events_after("node-a", one, None, 10)
            .unwrap()
            .events
            .len(),
        1
    );
    assert_eq!(events.truncate_through(two), Ok(2));
    assert!(events.events_after("node-a", 0, None, 10).unwrap().resync);
    assert_eq!(
        events
            .events_after("node-a", two, None, 10)
            .unwrap()
            .events
            .len(),
        1
    );
}

#[derive(Default)]
struct BackendState {
    instances: BTreeMap<WorkloadId, Observation>,
    steps: Vec<(WorkloadId, ReconcileAction)>,
    sequence: u64,
    fail_next: Option<PortError>,
    files: BTreeMap<(WorkloadId, String), Vec<u8>>,
}

/// The reference backend: it keeps instances in memory and does what it is told.
pub struct ScriptedBackend {
    pub runtimes: Vec<RuntimeCapabilities>,
    state: Mutex<BackendState>,
}

impl ScriptedBackend {
    #[must_use]
    pub fn new(runtimes: Vec<RuntimeCapabilities>) -> Self {
        Self {
            runtimes,
            state: Mutex::new(BackendState::default()),
        }
    }

    /// Fail the next backend call with `error`.
    pub fn fail_next(&self, error: PortError) {
        lock(&self.state).fail_next = Some(error);
    }

    /// Lose an instance, as a crashed host would.
    pub fn lose(&self, id: WorkloadId) {
        lock(&self.state).instances.remove(&id);
    }

    /// Report an instance no workload desires.
    pub fn plant(&self, id: WorkloadId, observation: Observation) {
        lock(&self.state).instances.insert(id, observation);
    }

    /// The steps taken so far.
    #[must_use]
    pub fn steps(&self) -> Vec<(WorkloadId, ReconcileAction)> {
        lock(&self.state).steps.clone()
    }

    fn check(&self) -> PortResult<()> {
        match lock(&self.state).fail_next.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl VmmBackend for ScriptedBackend {
    fn describe(&self) -> PortResult<BackendDescription> {
        Ok(BackendDescription {
            name: "scripted".to_owned(),
            version: "1".to_owned(),
            contract: "1".to_owned(),
            runtimes: self.runtimes.clone(),
        })
    }

    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation> {
        self.check()?;
        let mut state = lock(&self.state);
        state.steps.push((workload.id, action));
        state.sequence += 1;
        let observed = match action {
            ReconcileAction::Start | ReconcileAction::Resume | ReconcileAction::Restart => {
                ObservedWorkloadState::Running
            }
            ReconcileAction::Pause => ObservedWorkloadState::Paused,
            ReconcileAction::Stop | ReconcileAction::Delete => ObservedWorkloadState::Stopped,
            ReconcileAction::None | ReconcileAction::Adopt => state
                .instances
                .get(&workload.id)
                .map_or(ObservedWorkloadState::Unknown, |observation| {
                    observation.state
                }),
        };
        let observation = Observation {
            state: observed,
            generation: workload.desired.generation,
            sequence: state.sequence,
            reason: None,
            observed_at_millis: 0,
        };
        if action == ReconcileAction::Delete {
            state.instances.remove(&workload.id);
        } else {
            state.instances.insert(workload.id, observation.clone());
        }
        Ok(observation)
    }

    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>> {
        self.check()?;
        let mut state = lock(&self.state);
        state.sequence += 1;
        let sequence = state.sequence;
        Ok(state
            .instances
            .iter_mut()
            .map(|(id, observation)| {
                observation.sequence = sequence;
                (*id, observation.clone())
            })
            .collect())
    }

    fn run(
        &self,
        _workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String> {
        self.check()?;
        Ok(format!(
            "{{\"output\":{}}}",
            operation.request.as_deref().unwrap_or("null")
        ))
    }

    fn forward_http(&self, _workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        self.check()?;
        Ok(format!(
            "{{\"status\":200,\"headers\":{{}},\"body\":\"{}\"}}",
            request.len()
        ))
    }

    fn put_file(&self, workload: &WorkloadRecord, path: &str, bytes: &[u8]) -> PortResult<()> {
        self.check()?;
        lock(&self.state)
            .files
            .insert((workload.id, path.to_owned()), bytes.to_vec());
        Ok(())
    }

    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>> {
        self.check()?;
        lock(&self.state)
            .files
            .get(&(workload.id, path.to_owned()))
            .cloned()
            .ok_or(PortError::NotFound)
    }

    fn endpoints(&self, _workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>> {
        self.check()?;
        Ok(Vec::new())
    }

    fn usage(&self, _workload: &WorkloadRecord) -> PortResult<Usage> {
        self.check()?;
        Ok(Usage {
            invocations: lock(&self.state).steps.len() as u64,
            ..Usage::default()
        })
    }

    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>> {
        self.check()?;
        let steps = lock(&self.state)
            .steps
            .iter()
            .filter(|(id, _)| *id == workload.id)
            .count() as u64;
        Ok((after + 1..=steps)
            .take(limit)
            .map(|sequence| LogRecord {
                sequence,
                at_millis: 0,
                stream: LogStream::System,
                line: format!("step {sequence}"),
            })
            .collect())
    }

    fn verify(&self, _runtime: &str, request: &str) -> PortResult<String> {
        self.check()?;
        Ok(format!("{{\"valid\":{}}}", request.contains("\"proof\"")))
    }
}
