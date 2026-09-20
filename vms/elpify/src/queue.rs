//! Per-program-entity batching queues for elpify transactions.
//!
//! Each distinct MASM program on a machine gets a worker that collects
//! transactions over a 500ms window and executes each sealed window as ONE
//! loop-wrapped, single-proof batch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use elpify_lang::execute_batch_with_proof;
use serde_json::{json, Value as JsonValue};

use caspar_vm_sdk::host::{host, log, set_log_vm_context};
use caspar_vm_sdk::util::{emit_vm_error, panic_message};
use caspar_vm_sdk::VmResourceLimits;

fn global_elpify_vms() -> &'static Arc<Mutex<HashMap<String, Arc<ElpifyManagedVm>>>> {
    static CELL: OnceLock<Arc<Mutex<HashMap<String, Arc<ElpifyManagedVm>>>>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

pub(crate) struct ElpifyTask {
    pub(crate) masm_path: String,
    pub(crate) input_raw: String,
    pub(crate) vm_id: String,
    pub(crate) limits: VmResourceLimits,
}

pub(crate) struct ElpifyManagedVm {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) sender: Sender<ElpifyTask>,
}

/// Length of the collection window each elpify program queue uses to gather
/// transactions before sealing and executing them as one batch.
pub(crate) const ELPIFY_BATCH_WINDOW: Duration = Duration::from_millis(500);

impl ElpifyManagedVm {
    /// Spawn a per-program-entity worker that batches transactions on a 500ms
    /// window and executes each sealed batch sequentially.
    ///
    /// Scheduling contract (per program queue):
    ///   * A fresh window opens the moment the queue starts collecting.
    ///   * The window stays open for [`ELPIFY_BATCH_WINDOW`]; every transaction
    ///     that arrives during it joins the batch.
    ///   * When the window closes the batch is sealed and executed
    ///     synchronously on this single worker thread, so two batches of the
    ///     *same* program never overlap.
    ///   * The queue keeps collecting *during* execution; those transactions
    ///     form the next batch, whose window is anchored to the previous
    ///     window's close.
    pub(crate) fn new(machine_id: String) -> Self {
        let (tx, rx): (Sender<ElpifyTask>, Receiver<ElpifyTask>) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let machine_clone = machine_id.clone();

        thread::spawn(move || {
            // This queue serves one program entity (machine_id + masm_path), so
            // a batch shares one MASM source; cache it across batches anyway.
            let mut source_cache: HashMap<String, String> = HashMap::new();
            let mut pending: Vec<ElpifyTask> = Vec::new();
            // `Some(deadline)` while a collection window is open; `None` idle.
            let mut next_deadline: Option<Instant> = None;

            'scheduler: loop {
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }
                match next_deadline {
                    None => {
                        // Idle: block until the first transaction of a new window.
                        match rx.recv() {
                            Ok(task) => {
                                pending.push(task);
                                next_deadline = Some(Instant::now() + ELPIFY_BATCH_WINDOW);
                            }
                            Err(_) => break, // every sender dropped → tear down
                        }
                    }
                    Some(deadline) => {
                        let now = Instant::now();
                        if now < deadline {
                            match rx.recv_timeout(deadline - now) {
                                Ok(task) => {
                                    pending.push(task);
                                    continue 'scheduler; // keep collecting
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    // window elapsed → fall through to seal
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    if !pending.is_empty() {
                                        let batch = std::mem::take(&mut pending);
                                        run_batch_dispatch(
                                            &machine_clone,
                                            &mut source_cache,
                                            &batch,
                                        );
                                    }
                                    break;
                                }
                            }
                        }
                        if stop_clone.load(Ordering::Relaxed) {
                            break;
                        }
                        // Window closed → seal and execute this batch.
                        let batch = std::mem::take(&mut pending);
                        let anchor = deadline; // next window opens back-to-back here
                        if !batch.is_empty() {
                            run_batch_dispatch(&machine_clone, &mut source_cache, &batch);
                        }
                        // Pull in everything that arrived during execution.
                        while let Ok(task) = rx.try_recv() {
                            pending.push(task);
                        }
                        if pending.is_empty() {
                            next_deadline = None;
                        } else {
                            // Anchor the next window to the previous window's
                            // close; if execution overran it fires immediately.
                            next_deadline = Some(anchor + ELPIFY_BATCH_WINDOW);
                        }
                    }
                }
            }
        });

        ElpifyManagedVm { stop, sender: tx }
    }

    pub(crate) fn enqueue(&self, task: ElpifyTask) -> Result<(), String> {
        self.sender
            .send(task)
            .map_err(|e| format!("failed to enqueue elpify task: {}", e))
    }

    pub(crate) fn terminate(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Identity of an elpify program *entity*: a distinct MASM program on a
/// machine. Same key → shared queue (sequential batches); different keys →
/// independent queues (concurrent batches).
pub(crate) fn elpify_program_key(machine_id: &str, masm_path: &str) -> String {
    if masm_path.is_empty() {
        machine_id.to_string()
    } else {
        format!("{}::{}", machine_id, masm_path)
    }
}

/// Enqueue a transaction into the per-program elpify queue, creating the
/// queue (and its batch-scheduler worker) on first use.
pub(crate) fn enqueue_elpify_task(
    machine_id: &str,
    masm_path: String,
    input_raw: String,
    vm_id: String,
    limits: VmResourceLimits,
) -> Result<(), String> {
    let key = elpify_program_key(machine_id, &masm_path);
    let handle = {
        let mut map = global_elpify_vms().lock().unwrap();
        Arc::clone(
            map.entry(key)
                .or_insert_with(|| Arc::new(ElpifyManagedVm::new(machine_id.to_string()))),
        )
    };
    handle.enqueue(ElpifyTask {
        masm_path,
        input_raw,
        vm_id,
        limits,
    })
}

/// Stop and remove every elpify queue belonging to `machine_id`.
pub(crate) fn terminate_elpify_vms(machine_id: &str) {
    let prefix = format!("{}::", machine_id);
    let mut emap = global_elpify_vms().lock().unwrap();
    let keys: Vec<String> = emap
        .keys()
        .filter(|k| k.as_str() == machine_id || k.starts_with(&prefix))
        .cloned()
        .collect();
    for key in keys {
        if let Some(handle) = emap.remove(&key) {
            handle.terminate();
            log(format!(
                "terminate requested for running elpify vm: {}",
                key
            ));
        }
    }
}

/// Load (and cache) a batch's MASM source, then prove it, surfacing any
/// failure or panic as a `vmOutput` error packet.
fn run_batch_dispatch(
    machine_id: &str,
    source_cache: &mut HashMap<String, String>,
    batch: &[ElpifyTask],
) {
    if batch.is_empty() {
        return;
    }
    let masm_path = batch
        .iter()
        .map(|t| t.masm_path.clone())
        .find(|p| !p.is_empty())
        .unwrap_or_default();
    let vm_id_for_err = batch.last().map(|t| t.vm_id.clone()).unwrap_or_default();

    // Resolve the program source (cached across batches).
    let source = if let Some(s) = source_cache.get(&masm_path) {
        Ok(s.clone())
    } else {
        match std::fs::read_to_string(&masm_path) {
            Ok(s) => {
                source_cache.insert(masm_path.clone(), s.clone());
                Ok(s)
            }
            Err(e) => Err(format!("failed to read MASM file {}: {}", masm_path, e)),
        }
    };

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let source = source?;
        run_elpify_batch(machine_id, &source, &masm_path, batch)
    }));
    match res {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            log(format!(
                "elpify batch failed for machine {}: {}",
                machine_id, e
            ));
            emit_vm_error(machine_id, &vm_id_for_err, "elpify", &e);
        }
        Err(payload) => {
            let msg = panic_message(&payload);
            log(format!(
                "elpify batch worker panicked for machine {}: {}",
                machine_id, msg
            ));
            emit_vm_error(
                machine_id,
                &vm_id_for_err,
                "elpify",
                &format!("panic: {}", msg),
            );
            // The worker thread survives; the next batch is independent.
        }
    }
}

/// Compile a fresh loop-wrapped program for this batch (loop count == number
/// of transactions, each round fed its own payload) and prove it once. Emits
/// a single `vmOutput` packet carrying the batch proof and per-transaction
/// outputs.
fn run_elpify_batch(
    machine_id: &str,
    masm_source: &str,
    masm_path: &str,
    batch: &[ElpifyTask],
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    // Representative vm context for logging.
    let rep_vm = batch.last().map(|t| t.vm_id.clone()).unwrap_or_default();
    set_log_vm_context(&rep_vm);

    // Per-transaction payloads, in collection order.
    let payloads: Vec<Vec<u64>> = batch
        .iter()
        .map(|t| extract_elpify_inputs(&t.input_raw))
        .collect();

    // Honour the most-recently-supplied resource limits in the batch.
    let limits = batch
        .last()
        .map(|t| t.limits.clone())
        .unwrap_or_else(VmResourceLimits::with_defaults);
    let hard_limit_bytes = limits.ram_mb.saturating_mul(1024).saturating_mul(1024);
    if (masm_source.len() as u64) > hard_limit_bytes {
        return Err(format!(
            "elpify vm exceeded memory limit before execution: source={} bytes limit={} bytes",
            masm_source.len(),
            hard_limit_bytes
        ));
    }

    let started_at = Instant::now();
    let artifacts = execute_batch_with_proof(masm_source, &payloads)
        .map_err(|e| format!("elpify batch execution failed: {}", e))?;
    if started_at.elapsed() > Duration::from_secs(limits.max_exec_time_secs) {
        return Err(format!(
            "elpify vm exceeded max execution time: {} seconds",
            limits.max_exec_time_secs
        ));
    }

    // Encode the STARK proof as base64 (compact for wasm round-trips) and
    // emit a single batch result packet. The single proof attests to every
    // round; a consumer reads its transaction's result by index.
    let proof_b64 = base64::engine::general_purpose::STANDARD.encode(&artifacts.proof_bytes);
    let payloads_json: Vec<JsonValue> = payloads.iter().map(|p| json!(p)).collect();
    let summary = format!(
        "elpify batch executed: {} transaction(s)",
        artifacts.batch_size
    );
    let batch_packet = json!({
        "key": "vmOutput",
        "input": {
            "machineId": machine_id,
            "vmId": rep_vm,
            "runtime": "elpify",
            "logType": "output",
            "masmPath": masm_path,
            "batchSize": artifacts.batch_size,
            "exposedOutputs": artifacts.per_tx_outputs.len(),
            "inputs": payloads_json,
            "outputs": artifacts.per_tx_outputs,
            "stackOutputs": artifacts.stack_outputs,
            "proof": proof_b64,
            "text": summary,
            "data": summary,
        }
    });
    if let Some(h) = host() {
        let _ = h.dispatch(&batch_packet);
    }

    log(format!(
        "elpify batch executed machine={} masm={} txs={} outputs={:?}",
        machine_id, masm_path, artifacts.batch_size, artifacts.per_tx_outputs
    ));
    Ok(())
}

pub(crate) fn extract_elpify_inputs(input_raw: &str) -> Vec<u64> {
    let parsed: Result<JsonValue, _> = serde_json::from_str(input_raw);
    if parsed.is_err() {
        return vec![];
    }
    let parsed = parsed.unwrap();

    if let Some(arr) = parsed["inputs"].as_array() {
        return arr.iter().filter_map(|v| v.as_u64()).collect();
    }
    if let Some(arr) = parsed["data"]["inputs"].as_array() {
        return arr.iter().filter_map(|v| v.as_u64()).collect();
    }
    if let Some(data_raw) = parsed["data"].as_str() {
        if let Ok(data_json) = serde_json::from_str::<JsonValue>(data_raw) {
            if let Some(arr) = data_json["inputs"].as_array() {
                return arr.iter().filter_map(|v| v.as_u64()).collect();
            }
        }
    }
    vec![]
}
