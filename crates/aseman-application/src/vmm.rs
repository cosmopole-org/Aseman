//! The VMM service use cases (A501, A503): idempotent lifecycle commands under the
//! node's desired generations, operation tracking, observation, and reconciliation.
//!
//! Transport, authentication, and idempotency keys are the HTTP edge's; the backend
//! does the infrastructure work (A504). Every read and write is scoped by `owner`,
//! the calling node.

use aseman_domain::vmm::{
    CommandFreshness, DesiredStatus, Endpoint, LifecycleError, LogRecord, Observation,
    OperationFailure, OperationKind, OperationRecord, ReconcileAction, RuntimeCapabilities, Usage,
    VmmFailure, WorkloadEventRecord, WorkloadEventType, WorkloadOperation, WorkloadRecord,
    WorkloadSpec, accept_observation, command_freshness, desired_transition, reconcile,
};
use aseman_domain::{
    DesiredWorkloadState, Generation, ObservedWorkloadState, OperationId, OperationState,
    WorkloadId,
};
use aseman_ports::vmm::{
    LifecycleCommand, NewWorkload, OperationFilter, Page, VmmBackend, VmmEventLog,
    VmmOperationStore, VmmWorkloadStore, WorkloadFilter,
};
use aseman_ports::{ClockPort, PortError};
use thiserror::Error;

/// How many times a compare-and-set on a workload record is retried after losing a
/// race with the observer.
const CAS_ATTEMPTS: usize = 5;
const PAGE: usize = 200;

/// A refused or failed VMM request, as an A501 problem code.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{}: {detail}", failure.as_str())]
pub struct VmmError {
    pub failure: VmmFailure,
    pub detail: String,
    /// With `stale_generation`: the generation the VMM holds.
    pub current_generation: Option<Generation>,
}

impl VmmError {
    #[must_use]
    pub fn new(failure: VmmFailure, detail: impl Into<String>) -> Self {
        Self {
            failure,
            detail: detail.into(),
            current_generation: None,
        }
    }
}

impl From<LifecycleError> for VmmError {
    fn from(error: LifecycleError) -> Self {
        Self::new(error.into(), error.to_string())
    }
}

impl From<PortError> for VmmError {
    fn from(error: PortError) -> Self {
        let failure = match &error {
            PortError::NotFound => VmmFailure::NotFound,
            PortError::Conflict => VmmFailure::ResourceVersionMismatch,
            PortError::Denied(_) => VmmFailure::Forbidden,
            PortError::Unavailable(_) => VmmFailure::Unavailable,
            PortError::Deadline => VmmFailure::DeadlineExceeded,
            PortError::Unsupported(_) => VmmFailure::UnsupportedOperation,
            PortError::Failed(_) => VmmFailure::BackendFailure,
        };
        Self::new(failure, error.to_string())
    }
}

type VmmResult<T> = Result<T, VmmError>;

/// What a data-plane operation targets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationTarget {
    Workload(WorkloadId),
    /// Builds and verifications name a runtime, not a workload.
    Runtime(String),
}

/// What one observation pass did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ObservationReport {
    pub recorded: usize,
    pub lost: usize,
    /// Instances the backend runs that no workload desires. They are left alone for
    /// adoption (ADR 0022), never stopped or deleted here.
    pub undesired: Vec<WorkloadId>,
}

pub struct VmmService<'a> {
    pub workloads: &'a dyn VmmWorkloadStore,
    pub operations: &'a dyn VmmOperationStore,
    pub events: &'a dyn VmmEventLog,
    pub backend: &'a dyn VmmBackend,
    pub clock: &'a dyn ClockPort,
}

impl VmmService<'_> {
    fn runtime(&self, key: &str) -> VmmResult<RuntimeCapabilities> {
        self.backend
            .describe()?
            .runtimes
            .into_iter()
            .find(|runtime| runtime.runtime == key)
            .ok_or_else(|| {
                VmmError::new(
                    VmmFailure::UnsupportedOperation,
                    format!("runtime {key} is not offered"),
                )
            })
    }

    fn load(&self, owner: &str, id: WorkloadId) -> VmmResult<WorkloadRecord> {
        self.workloads
            .workload(owner, id)?
            .ok_or_else(|| VmmError::new(VmmFailure::NotFound, "no such workload"))
    }

    fn event(
        &self,
        owner: &str,
        workload_id: WorkloadId,
        event_type: WorkloadEventType,
        observation: Option<Observation>,
        operation: Option<OperationId>,
    ) -> VmmResult<()> {
        self.events.append(&WorkloadEventRecord {
            owner: owner.to_owned(),
            sequence: 0,
            workload_id,
            at_millis: self.clock.unix_millis(),
            event_type,
            observation,
            operation,
        })?;
        Ok(())
    }

    fn new_operation(
        &self,
        owner: &str,
        workload_id: Option<WorkloadId>,
        kind: OperationKind,
        generation: Option<Generation>,
        request: Option<String>,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        let now = self.clock.unix_millis();
        let operation = OperationRecord {
            owner: owner.to_owned(),
            id: OperationId::new(),
            workload_id,
            kind,
            state: OperationState::Pending,
            generation,
            request,
            created_at_millis: now,
            updated_at_millis: now,
            deadline_millis,
            result: None,
            error: None,
        };
        self.operations.insert_operation(&operation)?;
        if let Some(workload_id) = workload_id {
            self.event(
                owner,
                workload_id,
                WorkloadEventType::Operation,
                None,
                Some(operation.id),
            )?;
        }
        Ok(operation)
    }

    /// The operation already recorded for this workload and generation (a replay).
    fn operation_for(
        &self,
        owner: &str,
        workload_id: WorkloadId,
        generation: Generation,
    ) -> VmmResult<OperationRecord> {
        let mut cursor = None;
        loop {
            let page = self.operations.operations(
                owner,
                &OperationFilter {
                    workload_id: Some(workload_id),
                    state: None,
                },
                cursor.as_deref(),
                PAGE,
            )?;
            if let Some(found) = page
                .items
                .into_iter()
                .find(|operation| operation.generation == Some(generation))
            {
                return Ok(found);
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => {
                    return Err(VmmError::new(
                        VmmFailure::NotFound,
                        "no operation recorded for this generation",
                    ));
                }
            }
        }
    }

    fn check_version(record: &WorkloadRecord, if_match: Option<u64>) -> VmmResult<()> {
        match if_match {
            Some(expected) if expected != record.resource_version => Err(VmmError::new(
                VmmFailure::ResourceVersionMismatch,
                "the resource version changed",
            )),
            _ => Ok(()),
        }
    }

    /// Classify the command's generation (A503). `Ok(None)` means apply.
    fn freshness(
        &self,
        owner: &str,
        record: &WorkloadRecord,
        generation: Generation,
    ) -> VmmResult<Option<OperationRecord>> {
        match command_freshness(Some(record.desired.generation), generation) {
            CommandFreshness::Apply => Ok(None),
            CommandFreshness::Replay => self.operation_for(owner, record.id, generation).map(Some),
            CommandFreshness::Stale => Err(VmmError {
                current_generation: Some(record.desired.generation),
                ..VmmError::new(
                    VmmFailure::StaleGeneration,
                    "a newer generation was applied",
                )
            }),
        }
    }

    /// Create a workload with its initial desired state (`stopped` or `running`).
    ///
    /// # Errors
    ///
    /// `already_exists`, `invalid_transition`, `unsupported_operation`, or a store
    /// failure.
    pub fn create(
        &self,
        owner: &str,
        workload: &NewWorkload,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        if !matches!(
            workload.desired.state,
            DesiredWorkloadState::Stopped | DesiredWorkloadState::Running
        ) {
            return Err(VmmError::new(
                VmmFailure::InvalidTransition,
                "a workload starts stopped or running",
            ));
        }
        let runtime = self.runtime(&workload.spec.runtime)?;
        if !workload.spec.network.ingress.is_empty() && !runtime.http_ingress {
            // Declared ports would silently be unreachable.
            return Err(VmmError::new(
                VmmFailure::UnsupportedOperation,
                "the runtime has no ingress",
            ));
        }
        let now = self.clock.unix_millis();
        let record = WorkloadRecord {
            owner: owner.to_owned(),
            id: workload.id,
            labels: workload.labels.clone(),
            spec: workload.spec.clone(),
            desired: workload.desired,
            applied_generation: None,
            observed: None,
            resource_version: 1,
            created_at_millis: now,
            updated_at_millis: now,
        };
        match self.workloads.insert_workload(&record) {
            Err(PortError::Conflict) => {
                return Err(VmmError::new(
                    VmmFailure::AlreadyExists,
                    "the workload already exists",
                ));
            }
            other => other?,
        }
        self.new_operation(
            owner,
            Some(record.id),
            OperationKind::Create,
            Some(record.desired.generation),
            None,
            deadline_millis,
        )
    }

    /// Apply a lifecycle command at the node's desired generation.
    ///
    /// # Errors
    ///
    /// `not_found`, `stale_generation`, `resource_version_mismatch`,
    /// `invalid_transition`, `workload_deleted`, `unsupported_operation`.
    pub fn command(
        &self,
        owner: &str,
        id: WorkloadId,
        command: LifecycleCommand,
        generation: Generation,
        if_match: Option<u64>,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        let mut record = self.load(owner, id)?;
        if let Some(replay) = self.freshness(owner, &record, generation)? {
            return Ok(replay);
        }
        Self::check_version(&record, if_match)?;
        let target = match command {
            LifecycleCommand::Start | LifecycleCommand::Resume => DesiredWorkloadState::Running,
            LifecycleCommand::Stop => DesiredWorkloadState::Stopped,
            LifecycleCommand::Pause => DesiredWorkloadState::Paused,
            LifecycleCommand::Delete => DesiredWorkloadState::Deleted,
        };
        if command == LifecycleCommand::Resume
            && record.desired.state != DesiredWorkloadState::Paused
        {
            return Err(VmmError::new(
                VmmFailure::InvalidTransition,
                "only a paused workload resumes",
            ));
        }
        desired_transition(record.desired.state, target)?;
        let operation = match command {
            LifecycleCommand::Pause | LifecycleCommand::Resume => WorkloadOperation::Pause,
            LifecycleCommand::Start => WorkloadOperation::Start,
            LifecycleCommand::Stop => WorkloadOperation::Stop,
            LifecycleCommand::Delete => WorkloadOperation::Delete,
        };
        self.runtime(&record.spec.runtime)?.check(operation)?;
        let expected = record.resource_version;
        record.desired = DesiredStatus {
            state: target,
            generation,
        };
        self.save(&mut record, expected)?;
        self.new_operation(
            owner,
            Some(id),
            command.kind(),
            Some(generation),
            None,
            deadline_millis,
        )
    }

    /// Replace the spec at a new desired generation; a running workload is
    /// redeployed by reconciliation.
    ///
    /// # Errors
    ///
    /// As [`Self::command`].
    pub fn update_spec(
        &self,
        owner: &str,
        id: WorkloadId,
        spec: &WorkloadSpec,
        generation: Generation,
        if_match: Option<u64>,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        let mut record = self.load(owner, id)?;
        if let Some(replay) = self.freshness(owner, &record, generation)? {
            return Ok(replay);
        }
        Self::check_version(&record, if_match)?;
        if record.desired.state == DesiredWorkloadState::Deleted {
            return Err(LifecycleError::Deleted.into());
        }
        self.runtime(&spec.runtime)?;
        let expected = record.resource_version;
        record.spec = spec.clone();
        record.desired.generation = generation;
        self.save(&mut record, expected)?;
        self.new_operation(
            owner,
            Some(id),
            OperationKind::UpdateSpec,
            Some(generation),
            None,
            deadline_millis,
        )
    }

    /// Restore a workload from a snapshot at a new desired generation; it runs
    /// afterwards.
    ///
    /// # Errors
    ///
    /// As [`Self::command`].
    pub fn restore(
        &self,
        owner: &str,
        id: WorkloadId,
        generation: Generation,
        request: String,
        if_match: Option<u64>,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        let mut record = self.load(owner, id)?;
        if let Some(replay) = self.freshness(owner, &record, generation)? {
            return Ok(replay);
        }
        Self::check_version(&record, if_match)?;
        desired_transition(record.desired.state, DesiredWorkloadState::Running)?;
        self.runtime(&record.spec.runtime)?
            .check(WorkloadOperation::Snapshot)?;
        let expected = record.resource_version;
        record.desired = DesiredStatus {
            state: DesiredWorkloadState::Running,
            generation,
        };
        self.save(&mut record, expected)?;
        self.new_operation(
            owner,
            Some(id),
            OperationKind::Restore,
            Some(generation),
            Some(request),
            deadline_millis,
        )
    }

    /// Record a data-plane operation (invocation, exec, build, snapshot) for the
    /// executor.
    ///
    /// # Errors
    ///
    /// `not_found`, `unsupported_operation`, `invalid_transition` (the workload is not
    /// desired running), `workload_deleted`.
    pub fn submit(
        &self,
        owner: &str,
        target: &OperationTarget,
        operation: WorkloadOperation,
        request: String,
        deadline_millis: Option<i64>,
    ) -> VmmResult<OperationRecord> {
        let kind = match operation {
            WorkloadOperation::Invoke | WorkloadOperation::InvokeChain => OperationKind::Invoke,
            WorkloadOperation::Exec => OperationKind::Exec,
            WorkloadOperation::Build => OperationKind::Build,
            WorkloadOperation::Snapshot => OperationKind::Snapshot,
            _ => {
                return Err(VmmError::new(
                    VmmFailure::InvalidRequest,
                    "not a data-plane operation",
                ));
            }
        };
        let workload_id = match target {
            OperationTarget::Runtime(runtime) => {
                self.runtime(runtime)?.check(operation)?;
                None
            }
            OperationTarget::Workload(id) => {
                let record = self.load(owner, *id)?;
                self.runtime(&record.spec.runtime)?.check(operation)?;
                match record.desired.state {
                    DesiredWorkloadState::Running => {}
                    // A snapshot of a paused or stopped instance is fine.
                    DesiredWorkloadState::Paused | DesiredWorkloadState::Stopped
                        if operation == WorkloadOperation::Snapshot => {}
                    DesiredWorkloadState::Deleted => return Err(LifecycleError::Deleted.into()),
                    _ => {
                        return Err(VmmError::new(
                            VmmFailure::InvalidTransition,
                            "the workload is not desired running",
                        ));
                    }
                }
                Some(*id)
            }
        };
        self.new_operation(
            owner,
            workload_id,
            kind,
            None,
            Some(request),
            deadline_millis,
        )
    }

    /// Persist `record` under compare-and-set, advancing its resource version.
    fn save(&self, record: &mut WorkloadRecord, expected: u64) -> VmmResult<()> {
        record.resource_version = expected + 1;
        record.updated_at_millis = self.clock.unix_millis();
        match self.workloads.replace_workload(record, expected) {
            Err(PortError::Conflict) => Err(VmmError::new(
                VmmFailure::ResourceVersionMismatch,
                "the workload changed concurrently",
            )),
            other => other.map_err(Into::into),
        }
    }

    pub fn workload(&self, owner: &str, id: WorkloadId) -> VmmResult<WorkloadRecord> {
        self.load(owner, id)
    }

    pub fn workloads(
        &self,
        owner: &str,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> VmmResult<Page<WorkloadRecord>> {
        Ok(self
            .workloads
            .workloads(owner, filter, cursor, limit.max(1))?)
    }

    pub fn operation(&self, owner: &str, id: OperationId) -> VmmResult<OperationRecord> {
        self.operations
            .operation(owner, id)?
            .ok_or_else(|| VmmError::new(VmmFailure::NotFound, "no such operation"))
    }

    pub fn operations(
        &self,
        owner: &str,
        filter: &OperationFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> VmmResult<Page<OperationRecord>> {
        Ok(self
            .operations
            .operations(owner, filter, cursor, limit.max(1))?)
    }

    /// Cancel an unfinished operation. A running backend call is not interrupted;
    /// its result is discarded.
    ///
    /// # Errors
    ///
    /// `not_found`, `operation_finished`.
    pub fn cancel(&self, owner: &str, id: OperationId) -> VmmResult<OperationRecord> {
        for _ in 0..CAS_ATTEMPTS {
            let mut operation = self.operation(owner, id)?;
            let expected = operation.state;
            operation.transition(OperationState::Cancelled, self.clock.unix_millis())?;
            match self.operations.replace_operation(&operation, expected) {
                Ok(()) => {
                    if let Some(workload_id) = operation.workload_id {
                        self.event(
                            owner,
                            workload_id,
                            WorkloadEventType::Operation,
                            None,
                            Some(id),
                        )?;
                    }
                    return Ok(operation);
                }
                Err(PortError::Conflict) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(VmmError::new(
            VmmFailure::IdempotencyInProgress,
            "the operation keeps changing",
        ))
    }

    fn live(
        &self,
        owner: &str,
        id: WorkloadId,
        operation: WorkloadOperation,
    ) -> VmmResult<WorkloadRecord> {
        let record = self.load(owner, id)?;
        if record.desired.state == DesiredWorkloadState::Deleted {
            return Err(LifecycleError::Deleted.into());
        }
        self.runtime(&record.spec.runtime)?.check(operation)?;
        Ok(record)
    }

    pub fn forward_http(&self, owner: &str, id: WorkloadId, request: &str) -> VmmResult<String> {
        let record = self.live(owner, id, WorkloadOperation::HttpForward)?;
        Ok(self.backend.forward_http(&record, request)?)
    }

    pub fn put_file(&self, owner: &str, id: WorkloadId, path: &str, bytes: &[u8]) -> VmmResult<()> {
        check_path(path)?;
        let record = self.live(owner, id, WorkloadOperation::CopyFiles)?;
        Ok(self.backend.put_file(&record, path, bytes)?)
    }

    pub fn get_file(&self, owner: &str, id: WorkloadId, path: &str) -> VmmResult<Vec<u8>> {
        check_path(path)?;
        let record = self.live(owner, id, WorkloadOperation::CopyFiles)?;
        Ok(self.backend.get_file(&record, path)?)
    }

    pub fn endpoints(&self, owner: &str, id: WorkloadId) -> VmmResult<Vec<Endpoint>> {
        Ok(self.backend.endpoints(&self.load(owner, id)?)?)
    }

    pub fn usage(&self, owner: &str, id: WorkloadId) -> VmmResult<Usage> {
        Ok(self.backend.usage(&self.load(owner, id)?)?)
    }

    pub fn logs(
        &self,
        owner: &str,
        id: WorkloadId,
        after: u64,
        limit: usize,
    ) -> VmmResult<Vec<LogRecord>> {
        Ok(self
            .backend
            .logs(&self.load(owner, id)?, after, limit.max(1))?)
    }

    pub fn verify(&self, runtime: &str, request: &str) -> VmmResult<String> {
        self.runtime(runtime)?
            .check(WorkloadOperation::VerifyExecution)?;
        Ok(self.backend.verify(runtime, request)?)
    }

    fn finish(
        &self,
        mut operation: OperationRecord,
        outcome: Result<Option<String>, VmmError>,
    ) -> VmmResult<OperationRecord> {
        let now = self.clock.unix_millis();
        match outcome {
            Ok(result) => {
                operation.transition(OperationState::Succeeded, now)?;
                operation.result = result;
            }
            Err(error) => {
                operation.transition(OperationState::Failed, now)?;
                operation.error = Some(OperationFailure {
                    code: error.failure,
                    detail: error.detail,
                });
            }
        }
        match self
            .operations
            .replace_operation(&operation, OperationState::Running)
        {
            // Cancelled meanwhile: the result is discarded.
            Err(PortError::Conflict) => {
                return self.operation(&operation.owner, operation.id);
            }
            other => other?,
        }
        if let Some(workload_id) = operation.workload_id {
            self.event(
                &operation.owner,
                workload_id,
                WorkloadEventType::Operation,
                None,
                Some(operation.id),
            )?;
        }
        Ok(operation)
    }

    /// Record `observation` on the workload (A503 ordering), retrying lost races.
    fn record_observation(
        &self,
        owner: &str,
        id: WorkloadId,
        observation: &Observation,
        applied: Option<Generation>,
    ) -> VmmResult<WorkloadRecord> {
        for _ in 0..CAS_ATTEMPTS {
            let mut record = self.load(owner, id)?;
            accept_observation(
                record.desired.generation,
                record.observed.as_ref(),
                observation,
            )?;
            let expected = record.resource_version;
            record.observed = Some(observation.clone());
            if applied.is_some() {
                record.applied_generation = applied;
            }
            match self.save(&mut record, expected) {
                Ok(()) => {
                    self.event(
                        owner,
                        id,
                        WorkloadEventType::Observed,
                        Some(observation.clone()),
                        None,
                    )?;
                    return Ok(record);
                }
                Err(error) if error.failure == VmmFailure::ResourceVersionMismatch => {}
                Err(error) => return Err(error),
            }
        }
        Err(VmmError::new(
            VmmFailure::Unavailable,
            "the workload keeps changing",
        ))
    }

    /// Take one reconciliation step for a lifecycle operation. `Ok(true)` when the
    /// workload reached its desired generation.
    fn converge(&self, operation: &OperationRecord, record: WorkloadRecord) -> VmmResult<bool> {
        if operation
            .generation
            .is_some_and(|generation| generation.get() < record.desired.generation.get())
        {
            return Err(VmmError {
                current_generation: Some(record.desired.generation),
                ..VmmError::new(
                    VmmFailure::StaleGeneration,
                    "superseded by a newer generation",
                )
            });
        }
        let desired = Some((record.desired.state, record.desired.generation));
        let action = reconcile(desired, record.observed.as_ref());
        if action == ReconcileAction::None {
            return Ok(true);
        }
        let observation = self.backend.step(&record, action)?;
        let record = self.record_observation(
            &operation.owner,
            record.id,
            &observation,
            Some(record.desired.generation),
        )?;
        let converged = reconcile(desired, record.observed.as_ref()) == ReconcileAction::None;
        if converged && record.desired.state == DesiredWorkloadState::Deleted {
            self.event(
                &operation.owner,
                record.id,
                WorkloadEventType::Deleted,
                None,
                Some(operation.id),
            )?;
        }
        Ok(converged)
    }

    /// Make one attempt at an operation. A lifecycle operation stays `running` until
    /// the workload converges; a data-plane operation finishes in one attempt.
    ///
    /// # Errors
    ///
    /// Store failures; backend failures are recorded on the operation instead.
    pub fn execute(&self, id: OperationId, owner: &str) -> VmmResult<OperationRecord> {
        let mut operation = self.operation(owner, id)?;
        if operation.state == OperationState::Pending {
            operation.transition(OperationState::Running, self.clock.unix_millis())?;
            match self
                .operations
                .replace_operation(&operation, OperationState::Pending)
            {
                Err(PortError::Conflict) => return self.operation(owner, id),
                other => other?,
            }
        }
        if operation.state != OperationState::Running {
            return Ok(operation);
        }
        if operation
            .deadline_millis
            .is_some_and(|deadline| self.clock.unix_millis() >= deadline)
        {
            return self.finish(
                operation,
                Err(VmmError::new(
                    VmmFailure::DeadlineExceeded,
                    "the deadline passed",
                )),
            );
        }
        let record = match operation.workload_id {
            Some(workload_id) => Some(self.load(owner, workload_id)?),
            None => None,
        };
        match operation.kind {
            OperationKind::Invoke
            | OperationKind::Exec
            | OperationKind::Build
            | OperationKind::Snapshot => {
                let outcome = self
                    .backend
                    .run(record.as_ref(), &operation)
                    .map(Some)
                    .map_err(VmmError::from);
                self.finish(operation, outcome)
            }
            OperationKind::CopyFiles => self.finish(operation, Ok(None)),
            _ => {
                let Some(mut record) = record else {
                    return self.finish(
                        operation,
                        Err(VmmError::new(VmmFailure::InvalidRequest, "no workload")),
                    );
                };
                if operation.kind == OperationKind::Restore
                    && record.applied_generation != operation.generation
                {
                    if let Err(error) = self.backend.run(Some(&record), &operation) {
                        return self.finish(operation, Err(error.into()));
                    }
                    // The restored instance is the new generation's.
                    record = self.load(owner, record.id)?;
                }
                match self.converge(&operation, record) {
                    Ok(true) => self.finish(operation, Ok(None)),
                    Ok(false) => Ok(operation),
                    Err(error) if error.failure.retryable() => Ok(operation),
                    Err(error) => self.finish(operation, Err(error)),
                }
            }
        }
    }

    /// Attempt every unfinished operation once; returns how many finished.
    ///
    /// # Errors
    ///
    /// Store failures.
    pub fn execute_pending(&self, limit: usize) -> VmmResult<usize> {
        let mut finished = 0;
        for operation in self.operations.unfinished_operations(limit.max(1))? {
            let after = self.execute(operation.id, &operation.owner)?;
            if !matches!(
                after.state,
                OperationState::Pending | OperationState::Running
            ) {
                finished += 1;
            }
        }
        Ok(finished)
    }

    fn each_workload(
        &self,
        mut visit: impl FnMut(WorkloadRecord) -> VmmResult<()>,
    ) -> VmmResult<()> {
        let mut cursor = None;
        loop {
            let page = self.workloads.all_workloads(cursor.as_deref(), PAGE)?;
            for record in page.items {
                visit(record)?;
            }
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return Ok(()),
            }
        }
    }

    /// Record what the backend reports. An instance the backend stopped reporting
    /// is `lost`; an instance nobody desires is reported, never touched.
    ///
    /// # Errors
    ///
    /// Backend or store failures.
    pub fn observe(&self) -> VmmResult<ObservationReport> {
        let mut reported: std::collections::BTreeMap<WorkloadId, Observation> =
            self.backend.observe_all()?.into_iter().collect();
        let mut report = ObservationReport::default();
        self.each_workload(|record| {
            let observation = match reported.remove(&record.id) {
                Some(observation) => observation,
                None => match &record.observed {
                    Some(previous)
                        if matches!(
                            previous.state,
                            ObservedWorkloadState::Pending
                                | ObservedWorkloadState::Running
                                | ObservedWorkloadState::Paused
                        ) && record.desired.state != DesiredWorkloadState::Deleted =>
                    {
                        report.lost += 1;
                        Observation {
                            state: ObservedWorkloadState::Lost,
                            generation: previous.generation,
                            sequence: previous.sequence + 1,
                            reason: Some("the backend no longer reports the instance".to_owned()),
                            observed_at_millis: self.clock.unix_millis(),
                        }
                    }
                    _ => return Ok(()),
                },
            };
            match self.record_observation(&record.owner, record.id, &observation, None) {
                Ok(_) => report.recorded += 1,
                // Old news: a newer observation is already recorded.
                Err(error) if error.failure == VmmFailure::StaleObservation => {}
                Err(error) => return Err(error),
            }
            Ok(())
        })?;
        report.undesired = reported.into_keys().collect();
        Ok(report)
    }

    /// Open an operation for every workload that drifted from its desired state and
    /// has none unfinished; returns how many were opened.
    ///
    /// # Errors
    ///
    /// Store failures.
    pub fn reconcile(&self) -> VmmResult<usize> {
        let mut opened = 0;
        self.each_workload(|record| {
            let action = reconcile(
                Some((record.desired.state, record.desired.generation)),
                record.observed.as_ref(),
            );
            let kind = match action {
                ReconcileAction::None | ReconcileAction::Adopt => return Ok(()),
                ReconcileAction::Start => OperationKind::Start,
                ReconcileAction::Stop => OperationKind::Stop,
                ReconcileAction::Pause => OperationKind::Pause,
                ReconcileAction::Resume => OperationKind::Resume,
                ReconcileAction::Delete => OperationKind::Delete,
                ReconcileAction::Restart => OperationKind::Restart,
            };
            let open = self
                .operations
                .operations(
                    &record.owner,
                    &OperationFilter {
                        workload_id: Some(record.id),
                        state: None,
                    },
                    None,
                    PAGE,
                )?
                .items
                .into_iter()
                .any(|operation| {
                    matches!(
                        operation.state,
                        OperationState::Pending | OperationState::Running
                    ) && operation.generation.is_some()
                });
            if !open {
                self.new_operation(
                    &record.owner,
                    Some(record.id),
                    kind,
                    Some(record.desired.generation),
                    None,
                    None,
                )?;
                opened += 1;
            }
            Ok(())
        })?;
        Ok(opened)
    }
}

/// A file path inside a workload: relative, without `..`, `.`, or empty segments.
fn check_path(path: &str) -> VmmResult<()> {
    let valid = !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if valid {
        Ok(())
    } else {
        Err(VmmError::new(
            VmmFailure::InvalidRequest,
            "invalid file path",
        ))
    }
}

#[cfg(test)]
mod tests;
