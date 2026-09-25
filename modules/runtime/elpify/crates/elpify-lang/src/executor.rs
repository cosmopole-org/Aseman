use std::fmt::{Display, Formatter};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use miden_vm::{
    AdviceInputs, Assembler, DefaultHost, ExecutionProof, Program, ProgramInfo, ProvingOptions,
    StackInputs, StackOutputs, VerificationError, prove, verify,
};

#[derive(Debug, Clone)]
pub struct ExecutionArtifacts {
    pub stack_outputs: Vec<u64>,
    pub proof_bytes: Vec<u8>,
    pub program_info: ProgramInfo,
    pub stack_inputs: StackInputs,
}

#[derive(Debug)]
pub enum ExecutorError {
    Assembly(String),
    Input(String),
    Execution(String),
    ProofDeserialization(String),
    Verification(String),
}

impl Display for ExecutorError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Assembly(msg) => write!(f, "assembly error: {msg}"),
            Self::Input(msg) => write!(f, "input error: {msg}"),
            Self::Execution(msg) => write!(f, "execution/proving error: {msg}"),
            Self::ProofDeserialization(msg) => write!(f, "proof deserialization error: {msg}"),
            Self::Verification(msg) => write!(f, "verification error: {msg}"),
        }
    }
}

impl std::error::Error for ExecutorError {}

pub fn assemble_program(masm: &str) -> Result<Program, ExecutorError> {
    Assembler::default()
        .assemble_program(masm)
        .map_err(|e| ExecutorError::Assembly(e.to_string()))
}

pub fn execute_with_proof(masm: &str, inputs: &[u64]) -> Result<ExecutionArtifacts, ExecutorError> {
    let program = assemble_program(masm)?;
    let stack_inputs = StackInputs::try_from_ints(inputs.iter().copied())
        .map_err(|e| ExecutorError::Input(e.to_string()))?;

    let mut host = DefaultHost::default();
    let (stack_outputs, proof) = prove(
        &program,
        stack_inputs.clone(),
        AdviceInputs::default(),
        &mut host,
        ProvingOptions::default(),
    )
    .map_err(|e| ExecutorError::Execution(e.to_string()))?;

    Ok(ExecutionArtifacts {
        stack_outputs: stack_outputs.as_int_vec(),
        proof_bytes: proof.to_bytes(),
        program_info: program.clone().into(),
        stack_inputs,
    })
}

pub fn execute_masm_file_with_proof(
    masm_path: &str,
    inputs: &[u64],
) -> Result<ExecutionArtifacts, ExecutorError> {
    let masm = fs::read_to_string(masm_path)
        .map_err(|e| ExecutorError::Input(format!("unable to read MASM file {masm_path}: {e}")))?;
    execute_with_proof(&masm, inputs)
}

pub fn verify_execution(
    program_info: ProgramInfo,
    stack_inputs: StackInputs,
    stack_outputs: StackOutputs,
    proof_bytes: &[u8],
) -> Result<u32, ExecutorError> {
    let proof = ExecutionProof::from_bytes(proof_bytes)
        .map_err(|e| ExecutorError::ProofDeserialization(e.to_string()))?;

    verify(program_info, stack_inputs, stack_outputs, proof).map_err(map_verification_error)
}

fn map_verification_error(err: VerificationError) -> ExecutorError {
    ExecutorError::Verification(err.to_string())
}

pub fn stack_outputs_from_ints(outputs: &[u64]) -> Result<StackOutputs, ExecutorError> {
    StackOutputs::try_from_ints(outputs.iter().copied())
        .map_err(|e| ExecutorError::Input(e.to_string()))
}

// ── Batch loop wrapping ──────────────────────────────────────────────────────
//
// The elpify scheduler collects a program's pending transactions into a 500ms
// window and runs them as a *single* batch. Instead of proving N independent
// executions (N proofs), the batch is compiled into ONE Miden program whose
// body is the original program unrolled once per transaction. This amortizes a
// single STARK proof over the whole batch — the entire point of batching on a
// zkVM.
//
// Per round (transaction) `i` the wrapper:
//   1. pushes that transaction's payload onto the operand stack as MASM
//      literals, reproducing the operand-stack layout the program would have
//      received as standalone `StackInputs` (slice order → last element on top),
//   2. inlines the original program body (`exec.main` for transpiled code),
//   3. captures the round's result (top of stack) into reserved absolute memory
//      at `BATCH_RESULT_BASE + i`,
//   4. drops the round's operand-stack remnants so the next round starts from a
//      clean 16-element zero frame (Miden never lets the stack shrink below 16,
//      so an over-count of `drop`s is a harmless no-op).
//
// After all rounds the per-transaction results are reloaded from memory using a
// descending-load accumulator. Miden requires the final operand stack to hold
// at most 16 elements, so each load is paired with a `movup.k drop` that
// consumes one base-zero as the result is placed — keeping the depth pinned at
// 16. The final stack is `[r_0, r_1, ..., r_{k-1}, 0, ...]` so
// `stack_outputs[i]` is transaction `i`'s result.
//
// Because the public operand stack exposes at most 16 elements, at most
// `BATCH_MAX_EXPOSED_OUTPUTS` per-transaction results are surfaced on the proof's
// public outputs; the proof itself still attests to every round in the batch.

/// Reserved absolute-memory base for per-round results. Procedure locals
/// (`loc_*`) live in a high frame region (≥ 2^30) managed by the assembler, so
/// this low region does not collide with transpiled programs (which only use
/// `loc_*`, never absolute `mem_*`).
const BATCH_RESULT_BASE: u32 = 1_000_000_000;

/// Max per-transaction results exposable on the public operand stack. Bounded by
/// Miden's 16-element output limit and the `movup` immediate range (2..=15).
pub const BATCH_MAX_EXPOSED_OUTPUTS: usize = 15;

/// Artifacts produced by proving a single batched (loop-wrapped) execution.
#[derive(Debug, Clone)]
pub struct BatchArtifacts {
    /// Number of transactions (rounds) in the batch.
    pub batch_size: usize,
    /// Full public operand stack at the end of execution (top-first).
    pub stack_outputs: Vec<u64>,
    /// Per-transaction results: `per_tx_outputs[i]` is transaction `i`'s result
    /// (top-of-stack after its round). Capped at [`BATCH_MAX_EXPOSED_OUTPUTS`].
    pub per_tx_outputs: Vec<u64>,
    /// STARK proof bytes covering the whole batch.
    pub proof_bytes: Vec<u8>,
    pub program_info: ProgramInfo,
    pub stack_inputs: StackInputs,
    /// The loop-wrapped MASM that was compiled and proven for this batch.
    pub wrapped_masm: String,
}

/// Strip `#`-to-end-of-line MASM comments so whitespace tokenization is safe.
fn strip_masm_comments(masm: &str) -> String {
    masm.lines()
        .map(|line| match line.find('#') {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Split a MASM program into its procedure/header section and the body of its
/// single `begin … end` entry block. MASM permits exactly one `begin` (the
/// executable entry — procedures use `proc`), which makes this split
/// unambiguous.
fn split_program(masm: &str) -> Result<(String, String), ExecutorError> {
    let stripped = strip_masm_comments(masm);
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    let begin_pos = tokens
        .iter()
        .position(|t| *t == "begin")
        .ok_or_else(|| ExecutorError::Assembly("program has no `begin` block".to_string()))?;
    let end_pos = tokens
        .iter()
        .rposition(|t| *t == "end")
        .ok_or_else(|| ExecutorError::Assembly("program has no closing `end`".to_string()))?;
    if end_pos <= begin_pos {
        return Err(ExecutorError::Assembly(
            "malformed program: closing `end` precedes `begin`".to_string(),
        ));
    }
    let header = tokens[..begin_pos].join(" ");
    let body = tokens[begin_pos + 1..end_pos].join(" ");
    Ok((header, body))
}

/// Build the per-batch loop-wrapped MASM program. `batch_inputs[i]` is
/// transaction `i`'s payload (its operand-stack inputs). The returned program is
/// recompiled fresh for every batch (the loop count and the literal payloads are
/// baked in at this point).
pub fn wrap_masm_in_batch_loop(
    masm: &str,
    batch_inputs: &[Vec<u64>],
) -> Result<String, ExecutorError> {
    let n = batch_inputs.len();
    if n == 0 {
        return Err(ExecutorError::Input(
            "cannot build a batch loop for an empty batch".to_string(),
        ));
    }
    let (header, body) = split_program(masm)?;
    let body = body.trim();

    let mut out = String::new();
    let header = header.trim();
    if !header.is_empty() {
        out.push_str(header);
        out.push('\n');
    }
    out.push_str("begin\n");

    for (i, payload) in batch_inputs.iter().enumerate() {
        out.push_str(&format!("    # ── round {i}: transaction {i} ──\n"));
        // 1. Payload onto the operand stack. Pushing in slice order matches
        //    StackInputs::try_from_ints (last element ends on top).
        for v in payload {
            out.push_str(&format!("    push.{v}\n"));
        }
        // 2. The original program body for this round.
        if !body.is_empty() {
            out.push_str("    ");
            out.push_str(body);
            out.push('\n');
        }
        // 3. Capture the round result (top of stack) into reserved memory.
        out.push_str(&format!("    mem_store.{}\n", BATCH_RESULT_BASE + i as u32));
        // 4. Reset the operand stack to a clean 16-zero frame. Dropping past the
        //    16-element floor is a no-op, so `payload.len() + 16` always clears
        //    any round remnants regardless of the body's net stack effect.
        let resets = payload.len() + 16;
        for _ in 0..resets {
            out.push_str("    drop\n");
        }
    }

    // Reload per-transaction results so the final stack is [r_0, ..., r_{k-1}].
    // Each load is paired with a `movup.(e+1) drop` (or `swap.1` for index 1)
    // that consumes one base-zero, holding the operand stack at depth 16.
    let exposed = n.min(BATCH_MAX_EXPOSED_OUTPUTS);
    out.push_str(&format!(
        "    # ── expose {exposed} of {n} per-transaction result(s) ──\n"
    ));
    for e in 0..exposed {
        let addr = BATCH_RESULT_BASE + (exposed - 1 - e) as u32;
        let idx = e + 1;
        let consume = if idx == 1 {
            "swap.1".to_string()
        } else {
            format!("movup.{idx}")
        };
        out.push_str(&format!("    mem_load.{addr}\n    {consume}\n    drop\n"));
    }

    out.push_str("end\n");
    Ok(out)
}

/// Compile and prove a single loop-wrapped batch, returning one STARK proof
/// covering every transaction's round plus the per-transaction results.
pub fn execute_batch_with_proof(
    masm: &str,
    batch_inputs: &[Vec<u64>],
) -> Result<BatchArtifacts, ExecutorError> {
    let wrapped = wrap_masm_in_batch_loop(masm, batch_inputs)?;
    let program = assemble_program(&wrapped)?;

    // Payloads are baked in as literals, so the public operand stack is empty.
    let stack_inputs = StackInputs::default();
    let mut host = DefaultHost::default();
    let (stack_outputs, proof) = prove(
        &program,
        stack_inputs.clone(),
        AdviceInputs::default(),
        &mut host,
        ProvingOptions::default(),
    )
    .map_err(|e| ExecutorError::Execution(e.to_string()))?;

    let stack_outputs = stack_outputs.as_int_vec();
    let exposed = batch_inputs.len().min(BATCH_MAX_EXPOSED_OUTPUTS);
    let per_tx_outputs = stack_outputs.iter().copied().take(exposed).collect();

    Ok(BatchArtifacts {
        batch_size: batch_inputs.len(),
        stack_outputs,
        per_tx_outputs,
        proof_bytes: proof.to_bytes(),
        program_info: program.clone().into(),
        stack_inputs,
        wrapped_masm: wrapped,
    })
}

/// Read a MASM file and prove a loop-wrapped batch of `batch_inputs`.
pub fn execute_masm_file_batch_with_proof(
    masm_path: &str,
    batch_inputs: &[Vec<u64>],
) -> Result<BatchArtifacts, ExecutorError> {
    let masm = fs::read_to_string(masm_path)
        .map_err(|e| ExecutorError::Input(format!("unable to read MASM file {masm_path}: {e}")))?;
    execute_batch_with_proof(&masm, batch_inputs)
}

pub type ProgramId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Set,
    Delete,
    Get,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    Set { key: u64, value: u64 },
    Delete { key: u64 },
    Get { key: u64, value: Option<u64> },
}

#[derive(Debug, Clone)]
pub struct TaskInput {
    pub inputs: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct TaskResult {
    pub runs: Vec<ExecutionArtifacts>,
    pub events: Vec<EngineEvent>,
    pub final_state: std::collections::HashMap<u64, u64>,
}

type EventHandler =
    Arc<dyn Fn(&EngineEvent, &mut std::collections::HashMap<u64, u64>) + Send + Sync + 'static>;
type EventDecoder = Arc<dyn Fn(&ExecutionArtifacts) -> Vec<EngineEvent> + Send + Sync + 'static>;

#[derive(Debug)]
pub enum EngineError {
    ProgramNotFound(ProgramId),
    QueueClosed,
    Executor(ExecutorError),
}

impl Display for EngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProgramNotFound(id) => write!(f, "program with id {id} was not found"),
            Self::QueueClosed => write!(f, "program queue is closed"),
            Self::Executor(err) => write!(f, "{err}"),
        }
    }
}
impl std::error::Error for EngineError {}
impl From<ExecutorError> for EngineError {
    fn from(value: ExecutorError) -> Self {
        Self::Executor(value)
    }
}

enum QueueMode {
    Single(TaskInput),
    Batch(Vec<TaskInput>),
}

struct QueuedTask {
    mode: QueueMode,
    responder: mpsc::Sender<Result<TaskResult, EngineError>>,
}

struct ProgramWorker {
    sender: mpsc::Sender<QueuedTask>,
}

pub struct ExecutionEngine {
    next_program_id: AtomicU64,
    workers: Arc<Mutex<std::collections::HashMap<ProgramId, ProgramWorker>>>,
    state: Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    handlers: Arc<Mutex<std::collections::HashMap<EventKind, Vec<EventHandler>>>>,
    decoder: Arc<Mutex<EventDecoder>>,
}

impl Default for ExecutionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionEngine {
    pub fn new() -> Self {
        Self {
            next_program_id: AtomicU64::new(1),
            workers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            handlers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            decoder: Arc::new(Mutex::new(Arc::new(default_event_decoder))),
        }
    }

    pub fn register_event_handler<F>(&self, kind: EventKind, handler: F)
    where
        F: Fn(&EngineEvent, &mut std::collections::HashMap<u64, u64>) + Send + Sync + 'static,
    {
        let mut handlers = self.handlers.lock().expect("handlers lock poisoned");
        handlers.entry(kind).or_default().push(Arc::new(handler));
    }

    pub fn register_event_decoder<F>(&self, decoder: F)
    where
        F: Fn(&ExecutionArtifacts) -> Vec<EngineEvent> + Send + Sync + 'static,
    {
        let mut lock = self.decoder.lock().expect("decoder lock poisoned");
        *lock = Arc::new(decoder);
    }

    pub fn deploy_program(&self, masm: &str) -> Result<ProgramId, EngineError> {
        assemble_program(masm)?;

        let program_id = self.next_program_id.fetch_add(1, Ordering::SeqCst);
        let masm = masm.to_string();
        let (tx, rx) = mpsc::channel::<QueuedTask>();
        let shared_state = Arc::clone(&self.state);
        let handlers = Arc::clone(&self.handlers);
        let decoder = Arc::clone(&self.decoder);

        thread::spawn(move || {
            while let Ok(task) = rx.recv() {
                let result = run_queued_task(
                    &masm,
                    task.mode,
                    &shared_state,
                    &handlers,
                    decoder.lock().expect("decoder lock poisoned").clone(),
                );
                let _ = task.responder.send(result);
            }
        });

        let worker = ProgramWorker { sender: tx };
        self.workers
            .lock()
            .expect("workers lock poisoned")
            .insert(program_id, worker);
        Ok(program_id)
    }

    pub fn deploy_program_from_path(&self, masm_path: &str) -> Result<ProgramId, EngineError> {
        let masm = fs::read_to_string(masm_path)
            .map_err(|e| EngineError::Executor(ExecutorError::Input(e.to_string())))?;
        self.deploy_program(&masm)
    }

    pub fn submit_task(
        &self,
        program_id: ProgramId,
        input: TaskInput,
    ) -> Result<TaskResult, EngineError> {
        self.submit(program_id, QueueMode::Single(input))
    }

    pub fn submit_batch(
        &self,
        program_id: ProgramId,
        batch: Vec<TaskInput>,
    ) -> Result<TaskResult, EngineError> {
        self.submit(program_id, QueueMode::Batch(batch))
    }

    fn submit(&self, program_id: ProgramId, mode: QueueMode) -> Result<TaskResult, EngineError> {
        let workers = self.workers.lock().expect("workers lock poisoned");
        let worker = workers
            .get(&program_id)
            .ok_or(EngineError::ProgramNotFound(program_id))?;

        let (tx, rx) = mpsc::channel();
        worker
            .sender
            .send(QueuedTask {
                mode,
                responder: tx,
            })
            .map_err(|_| EngineError::QueueClosed)?;

        rx.recv().map_err(|_| EngineError::QueueClosed)?
    }
}

fn run_queued_task(
    masm: &str,
    mode: QueueMode,
    shared_state: &Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    handlers: &Arc<Mutex<std::collections::HashMap<EventKind, Vec<EventHandler>>>>,
    decoder: EventDecoder,
) -> Result<TaskResult, EngineError> {
    let mut runs = Vec::new();
    let mut events = Vec::new();
    let items = match mode {
        QueueMode::Single(t) => vec![t],
        QueueMode::Batch(v) => v,
    };

    for task in items {
        let artifacts = execute_with_proof(masm, &task.inputs)?;
        let decoded = decoder(&artifacts);
        apply_events(shared_state, handlers, &decoded);
        events.extend(decoded);
        runs.push(artifacts);
    }

    let final_state = shared_state.lock().expect("state lock poisoned").clone();
    Ok(TaskResult {
        runs,
        events,
        final_state,
    })
}

fn apply_events(
    state: &Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    handlers: &Arc<Mutex<std::collections::HashMap<EventKind, Vec<EventHandler>>>>,
    events: &[EngineEvent],
) {
    let handlers_guard = handlers.lock().expect("handlers lock poisoned");
    let mut state_guard = state.lock().expect("state lock poisoned");

    for event in events {
        match event {
            EngineEvent::Set { key, value } => {
                state_guard.insert(*key, *value);
                if let Some(hs) = handlers_guard.get(&EventKind::Set) {
                    for h in hs {
                        h(event, &mut state_guard);
                    }
                }
            }
            EngineEvent::Delete { key } => {
                state_guard.remove(key);
                if let Some(hs) = handlers_guard.get(&EventKind::Delete) {
                    for h in hs {
                        h(event, &mut state_guard);
                    }
                }
            }
            EngineEvent::Get { key, .. } => {
                if let Some(hs) = handlers_guard.get(&EventKind::Get) {
                    for h in hs {
                        h(event, &mut state_guard);
                    }
                }
                let _ = state_guard.get(key);
            }
        }
    }
}

fn default_event_decoder(artifacts: &ExecutionArtifacts) -> Vec<EngineEvent> {
    let values = &artifacts.stack_outputs;
    if values.len() < 4 {
        return vec![];
    }
    let count = values[0] as usize;
    let mut events = Vec::new();
    for i in 0..count {
        let base = 1 + i * 3;
        if base + 2 >= values.len() {
            break;
        }
        let op = values[base];
        let key = values[base + 1];
        let value = values[base + 2];
        let event = match op {
            1 => EngineEvent::Set { key, value },
            2 => EngineEvent::Delete { key },
            3 => EngineEvent::Get { key, value: None },
            _ => continue,
        };
        events.push(event);
    }
    events
}
