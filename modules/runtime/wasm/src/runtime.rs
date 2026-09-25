//! The WasmEdge-backed managed VM runtime (`WasmMac`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value as JsonValue;
use std::time::SystemTime;

use wasmedge_sys::{
    config::Config, AsInstance, Compiler, Executor, FuncType, Function, ImportModule, Instance,
    Loader, Module, Store, Validator, WasiModule, WasmValue,
};
use wasmedge_types::ValType;

use caspar_vm_sdk::host::{host, log};

use crate::host_calls::host_call;
use crate::models::Trx;

pub(crate) fn global_managed_vms() -> &'static Arc<Mutex<HashMap<String, ManagedVmHandle>>> {
    static CELL: OnceLock<Arc<Mutex<HashMap<String, ManagedVmHandle>>>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

pub struct ManagedVmHandle {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) running: Arc<AtomicBool>,
}

impl ManagedVmHandle {
    pub fn terminate_vm_instance(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // WasmEdge's sync executor in this integration does not expose a hard
        // preemptive kill; cooperative stop is the termination mechanism.
    }
}

/// Signal a cooperative stop to the managed wasm VM of `machine_id`.
pub fn terminate_managed_vm(machine_id: &str) {
    let mut map = global_managed_vms().lock().unwrap();
    if let Some(handle) = map.remove(machine_id) {
        handle.terminate_vm_instance();
        if handle.running.load(Ordering::Relaxed) {
            log(format!(
                "terminate requested for running vm: {} (cooperative stop signaled)",
                machine_id
            ));
        }
    }
}

// ── AOT module cache ─────────────────────────────────────────────────────────
//
// A deployed module runs as native code instead of in the interpreter: we
// compile each module to a native shared library (`<src>.so`) exactly once,
// cache it on disk next to the source keyed by the source's mtime, and load
// that afterwards. The `.so` loads through the ordinary `Loader::from_file`
// path and exposes the identical exports, so nothing else in the build changes;
// it just runs as native code. Any failure (a WasmEdge built without LLVM, a
// read-only cache dir, a compiler panic) latches AOT off and falls back to the
// interpreter, so behaviour degrades to exactly what it was before.
//
// (The warm-VM pool means a module is loaded once per VM build and reused, so
// this AOT step now mainly buys native-speed execution rather than saving a
// per-signal load.)

/// Latched once an AOT compile fails, so a host that cannot compile never pays a
/// doomed compile again and quietly stays on the interpreter path.
fn aot_unavailable() -> &'static AtomicBool {
    static CELL: OnceLock<AtomicBool> = OnceLock::new();
    CELL.get_or_init(|| AtomicBool::new(false))
}

/// Per-module compile lock so concurrent signals to a cold module compile it
/// once instead of racing to write the same cache file.
fn aot_locks() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    static CELL: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Global gate serializing ALL AOT compilations across modules.
///
/// WasmEdge's AOT backend lowers each module to intermediate object files whose
/// functions are named generically (`f0`, `f1`, … — one set per module). When
/// two *different* modules compile concurrently, those object files collide at
/// the `ld.lld` link step (`error: duplicate symbol: f10`), the compile fails,
/// and AOT latches off for the rest of the process. The per-module `aot_locks`
/// only dedupe compiles of the *same* module, so cross-module compilation must
/// be serialized here. AOT is one-time per module (the result is cached to
/// disk), so this only serializes first-time warm-up and never touches the hot
/// path where a cached artifact already exists.
fn aot_compile_gate() -> &'static Mutex<()> {
    static CELL: OnceLock<Mutex<()>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(()))
}

/// AOT is on unless `CASPAR_WASM_AOT` is explicitly a falsey value.
fn aot_enabled() -> bool {
    aseman_config::runtime_config().wasm_aot
}

fn file_mtime(path: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Resolve the module file to actually load: a fresh cached AOT artifact when one
/// is available (`(path, true)`), otherwise the original `.wasm` (`(path, false)`,
/// which the caller still validates).
fn resolve_module_artifact(mod_path: &str) -> (String, bool) {
    if !aot_enabled() || aot_unavailable().load(Ordering::Relaxed) {
        return (mod_path.to_string(), false);
    }
    let src_mtime = match file_mtime(mod_path) {
        Some(t) => t,
        None => return (mod_path.to_string(), false),
    };
    // WasmEdge 0.17's loader dispatches on file extension and only treats
    // `.wasm`/`.wat`/`.so`/`.dylib`/`.dll` (or an extension-less path) as a
    // loadable module; a `.aot` suffix is rejected outright. The AOT compiler
    // emits a native shared library, so the cache lives at `<src>.so`.
    let cache_path = format!("{}.so", mod_path);
    // Fast path: a cache at least as new as the source already exists.
    if let Some(cache_mtime) = file_mtime(&cache_path) {
        if cache_mtime >= src_mtime {
            return (cache_path, true);
        }
    }
    // Serialize compilation of this specific module across concurrent signals.
    let lock = {
        let mut map = aot_locks().lock().unwrap();
        map.entry(mod_path.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().unwrap();
    // Re-check under the lock — another thread may have just built it, or another
    // module may have latched AOT off while we waited.
    if let Some(cache_mtime) = file_mtime(&cache_path) {
        if cache_mtime >= src_mtime {
            return (cache_path, true);
        }
    }
    if aot_unavailable().load(Ordering::Relaxed) {
        return (mod_path.to_string(), false);
    }
    match compile_aot(mod_path, &cache_path) {
        Ok(()) => {
            log(format!("wasm AOT compiled: {} -> {}", mod_path, cache_path));
            (cache_path, true)
        }
        Err(e) => {
            aot_unavailable().store(true, Ordering::Relaxed);
            log(format!(
                "wasm AOT unavailable ({}); using interpreter for {} and later modules",
                e, mod_path
            ));
            (mod_path.to_string(), false)
        }
    }
}

/// Compile `src` (`.wasm`) to a cached native shared library at `dst` (`.so`),
/// atomically. The WasmEdge FFI is wrapped in `catch_unwind` so a host without
/// an AOT-capable WasmEdge reports a normal error (→ interpreter fallback)
/// instead of aborting.
fn compile_aot(src: &str, dst: &str) -> Result<(), String> {
    // Compile to a temp file then rename, so a crash mid-compile never leaves a
    // half-written artifact a later signal would try to load.
    let tmp = format!("{}.tmp.{}", dst, std::process::id());
    let src_owned = src.to_string();
    let tmp_for_compile = tmp.clone();
    // Serialize the compile itself across all modules (see `aot_compile_gate`).
    // Held only for the duration of the LLVM/link work; recovered from a
    // poisoned lock so a single failed compile can never wedge AOT for the
    // whole process (the panic is contained by the `catch_unwind` below, so in
    // practice the guard is dropped cleanly).
    let _gate = aot_compile_gate()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
        // `Native` output format = a platform-native shared library the loader
        // dlopen's directly (WasmEdge 0.15+ dropped the universal-wasm output
        // that the previous `.aot` cache relied on). The artifact is host-local
        // and regenerated on any source/mtime change, so non-portability is fine.
        let mut config = Config::create().map_err(|e| format!("aot config: {}", e))?;
        config.set_aot_compiler_output_format(wasmedge_types::CompilerOutputFormat::Native);
        let compiler = Compiler::create(Some(&config)).map_err(|e| format!("aot create: {}", e))?;
        compiler
            .compile_from_file(&src_owned, &tmp_for_compile)
            .map_err(|e| format!("aot compile: {}", e))?;
        Ok(())
    }));
    let compiled = match res {
        Ok(inner) => inner,
        Err(_) => Err("aot compiler panicked".to_string()),
    };
    match compiled {
        Ok(()) => std::fs::rename(&tmp, dst).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("aot cache rename: {}", e)
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub struct WasmMac {
    pub callback: Box<dyn (Fn(JsonValue) -> String) + Send + Sync>,
    pub machine_id: String,
    pub vm_id: String,
    pub store_id: String,
    pub trx: Box<Trx>,
    /// Whether this VM opened a per-lifecycle JSON transaction on the host.
    /// Lazily set on the first putJson/getJson/getByPrefix/delKey host call
    /// and closed (committing) either on an explicit `commitTrx` host call or
    /// at VM teardown inside [`WasmMac::finalize`].
    pub vm_trx_open: bool,
    /// This execution's own key for the host JSON transaction. Never the bare
    /// `vm_id`: concurrent runs share that, and sharing a transaction loses writes.
    pub trx_key: String,
    pub mod_path: String,
    pub cost: u64,
    pub ram_limit_mb: u64,

    pub(crate) execution_result: String,
    pub(crate) has_output: bool,
    pub(crate) stop_: Arc<AtomicBool>,
    pub(crate) running_: Arc<AtomicBool>,
}

pub struct HostData {
    pub(crate) exec: *mut Executor,
    pub(crate) runtime: *mut WasmMac,
}

const WASM_PAGE: usize = 65536;

// ── Warm-VM cache ────────────────────────────────────────────────────────────
//
// Building an entire WasmEdge VM (Config/Store/Executor, the WASI + `env` host
// import modules, loading and validating the module, then instantiating it) and
// tearing it all down on *every* signal is the dominant source of the node's
// steady RSS growth: that per-signal allocate-then-free churn is never returned
// to the OS, so resident memory climbs roughly one linear-memory's worth per
// execution for the life of the process.
//
// Instead we build the VM once per (module payload, ram-limit), keep it warm in
// a small pool, and between runs only restore the guest's linear memory to the
// snapshot captured right after `_start`. A creature therefore always begins
// from exactly the state it saw on a cold start — nothing can bleed in from a
// previous invocation — while the expensive native machinery is reused.
//
// Safety / correctness invariants:
//   * A warm VM is removed from the pool for the whole duration of a run, so no
//     two threads ever touch the same WasmEdge Store/Executor/Instance at once.
//   * `HostData` and the `Executor` live behind `Box`es, so the raw pointers the
//     host function captured into them stay valid when the `VmStack` is moved
//     in and out of the pool (only the box pointers move; the heap stays put).
//   * `host_data.runtime` is re-pointed at the current `WasmMac` before each
//     run, so host calls always see the live signal's trx / callback / stop.
//   * Linear memory is rewound to its post-`_start` bytes before each run,
//     resetting the guest allocator and every byte of in-memory state.
//   * A warm VM is retired (dropped, not returned) once it has served
//     `MAX_REUSE` runs or its memory has grown past a cap, bounding any residual
//     per-instance drift.

/// Env-gated, default on. `CASPAR_WASM_VM_CACHE` = `0`/`false`/`off`/`no`
/// forces the legacy build-and-teardown-per-run path (used to isolate the
/// cache in testing / as an escape hatch).
fn vm_cache_enabled() -> bool {
    aseman_config::runtime_config().wasm_vm_cache
}

/// Retire a warm VM after this many runs (bounds any per-instance drift the
/// memory snapshot can't reach, e.g. WasmEdge-internal bookkeeping).
const MAX_REUSE: u32 = 1024;
/// Cap on warm VMs kept per pool key; extra concurrent builds run and are
/// dropped rather than pooled, so peak memory tracks peak concurrency.
const MAX_POOL_PER_KEY: usize = 16;
/// Retire a warm VM once its linear memory has grown this many pages past its
/// post-`_start` size, so a creature that expands memory cannot pin it forever.
const MEM_GROWTH_CAP_PAGES: u32 = 32;

/// One fully-instantiated, reusable WasmEdge VM for a specific module payload.
///
/// Field order is drop order: the instance and store are torn down before the
/// executor and host data they reference.
struct VmStack {
    instance: Instance,
    _module: Arc<Module>,
    _env: ImportModule<i32>,
    _wasi: WasiModule,
    _store: Store,
    _config: Config,
    /// Boxed so `host_data.exec` stays valid across `VmStack` moves.
    executor: Box<Executor>,
    /// Boxed so the host function's captured data pointer stays valid across
    /// `VmStack` moves. `runtime` is re-pointed per run.
    host_data: Box<HostData>,
    /// Linear-memory bytes captured right after `_start` (length =
    /// `initial_pages * WASM_PAGE`).
    mem_snapshot: Vec<u8>,
    initial_pages: u32,
    ram_limit_mb: u64,
    mtime: Option<SystemTime>,
    runs: u32,
}

// A `VmStack` is only ever used by one thread at a time (checked out
// exclusively from the pool). Its self-referential raw pointers target the
// boxed `executor` / `host_data`, whose heap allocations are stable across the
// moves the pool performs.
unsafe impl Send for VmStack {}

fn vm_pool() -> &'static Mutex<HashMap<String, Vec<VmStack>>> {
    static CELL: OnceLock<Mutex<HashMap<String, Vec<VmStack>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pool_key(mod_path: &str, ram_limit_mb: u64) -> String {
    format!("{}::{}", mod_path, ram_limit_mb)
}

/// Take a warm VM for this module+limit whose source file is unchanged, or
/// `None` if the pool has none (cold, or all entries were stale/mtime-changed).
fn checkout_vm(
    mod_path: &str,
    ram_limit_mb: u64,
    cur_mtime: Option<SystemTime>,
) -> Option<VmStack> {
    let key = pool_key(mod_path, ram_limit_mb);
    let mut pool = vm_pool().lock().unwrap();
    let bucket = pool.get_mut(&key)?;
    while let Some(vm) = bucket.pop() {
        // Drop any warm VM whose module file changed under it (redeploy); the
        // caller will rebuild from the fresh bytes.
        if vm.mtime == cur_mtime {
            return Some(vm);
        }
        // else: `vm` drops here, releasing the stale VM.
    }
    None
}

/// Return a warm VM to the pool, unless it should be retired (too many runs,
/// grown memory, or the pool is already full).
fn return_vm(mut vm: VmStack, mod_path: &str) {
    if vm.runs >= MAX_REUSE {
        return;
    }
    // Retire if the guest expanded its memory beyond the cap (can't shrink).
    if let Ok(mem) = vm.instance.get_memory_mut("memory") {
        if mem.size() > vm.initial_pages.saturating_add(MEM_GROWTH_CAP_PAGES) {
            return;
        }
    }
    let key = pool_key(mod_path, vm.ram_limit_mb);
    let mut pool = vm_pool().lock().unwrap();
    let bucket = pool.entry(key).or_default();
    if bucket.len() < MAX_POOL_PER_KEY {
        bucket.push(vm);
    }
    // else: full — `vm` drops.
}

impl WasmMac {
    pub fn new_vm(
        machine_id: String,
        vm_id: String,
        store_id: String,
        mod_path: String,
        ram_limit_mb: u64,
        cb: Box<dyn (Fn(JsonValue) -> String) + Send + Sync>,
    ) -> Self {
        let trx_key = caspar_vm_sdk::util::execution_trx_key(&vm_id);
        WasmMac {
            callback: cb,
            machine_id,
            vm_id,
            store_id,
            trx: Box::new(Trx::new()),
            vm_trx_open: false,
            trx_key,
            mod_path,
            execution_result: "".to_string(),
            has_output: false,
            stop_: Arc::new(AtomicBool::new(false)),
            running_: Arc::new(AtomicBool::new(false)),
            cost: 0,
            ram_limit_mb: ram_limit_mb.max(1),
        }
    }

    pub(crate) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop_)
    }

    pub(crate) fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running_)
    }

    pub fn finalize(&mut self) {
        // Auto-commit the per-VM JSON transaction if the VM did not call
        // commitTrx.
        if self.vm_trx_open {
            if let Some(h) = host() {
                h.end_vm_json_trx(&self.trx_key);
            }
            self.vm_trx_open = false;
        }
        self.trx.commit_as_offchain();
        // Surface the creature's final output (set by the `output` host op) as
        // a runtime vmOutput packet so the host platform can observe the JSON
        // response a wasm program produced for a given signal.
        if self.has_output {
            let payload = serde_json::json!({
                "key": "vmOutput",
                "input": {
                    "text": self.execution_result.clone(),
                    "data": self.execution_result.clone(),
                    "vmId": self.vm_id.clone(),
                    "machineId": self.machine_id.clone(),
                    "logType": "output",
                }
            });
            if let Some(h) = host() {
                let _ = h.dispatch(&payload);
            }
        }
    }

    /// Execute the deployed WASM module against `input`.
    ///
    /// Reuses a warm VM from the pool (see [`VmStack`]) when caching is enabled
    /// (the default), building one only on a cold miss; the guest's linear
    /// memory is rewound to its post-`_start` snapshot before each run so state
    /// never bleeds between invocations. With caching disabled a fresh VM is
    /// built and torn down for this one run — the original behaviour.
    ///
    /// Returns a structured error on any wasmedge failure instead of panicking.
    pub fn execute_on_update(&mut self, input: String) -> Result<(), String> {
        // Guard the FFI boundary: WasmEdge's `Loader::from_file` calls
        // `std::filesystem::absolute(path)` in C++, which THROWS on an empty
        // path ("cannot make absolute path: Invalid argument"). A C++ throw
        // cannot unwind across the FFI boundary, so it aborts the whole node
        // process (`std::terminate`) — uncatchable by the controller's
        // `catch_unwind`. We must therefore never hand WasmEdge an invalid
        // module path: validate it here and surface a normal Result::Err (which
        // the controller turns into a contained vmOutput error) instead.
        let mod_path = self.mod_path.trim().to_string();
        if mod_path.is_empty() {
            return Err(
                "wasm module path is empty (no astPath resolved for this entity); \
                 refusing to load"
                    .to_string(),
            );
        }
        if !std::path::Path::new(&mod_path).is_file() {
            return Err(format!("wasm module file not found: {}", mod_path));
        }

        self.running_.store(true, Ordering::Relaxed);
        struct RunningGuard(Arc<AtomicBool>);
        impl Drop for RunningGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Relaxed);
            }
        }
        let _running_guard = RunningGuard(Arc::clone(&self.running_));

        let cur_mtime = file_mtime(&mod_path);

        if vm_cache_enabled() {
            // Reuse a warm VM whose source is unchanged, else build one.
            let mut stack = match checkout_vm(&mod_path, self.ram_limit_mb, cur_mtime) {
                Some(s) => s,
                None => self.build_vm_stack(&mod_path, cur_mtime)?,
            };
            let res = self.run_on_stack(&mut stack, input);
            // Only a cleanly-finished run returns to the pool; a trap/error may
            // have left the instance in an undefined state, so drop it instead.
            if res.is_ok() {
                return_vm(stack, &mod_path);
            }
            res
        } else {
            // Legacy path: a fresh VM for this run, dropped at the end.
            let mut stack = self.build_vm_stack(&mod_path, cur_mtime)?;
            self.run_on_stack(&mut stack, input)
        }
    }

    /// Build a complete, ready-to-run WasmEdge VM for `mod_path`: config, store,
    /// executor, the WASI + `env` host imports, the loaded (and, for non-AOT,
    /// validated) module, an instantiated instance with `_start` already run,
    /// and a snapshot of its post-init linear memory. Everything the host
    /// function points into lives behind `Box`es so the pointers survive the
    /// `VmStack` being moved into and out of the pool.
    fn build_vm_stack(
        &self,
        mod_path: &str,
        cur_mtime: Option<SystemTime>,
    ) -> Result<VmStack, String> {
        // Boxed so its address is stable across moves; `runtime` points at the
        // building `WasmMac` so any host call issued during `_start` is served.
        let mut host_data = Box::new(HostData {
            exec: std::ptr::null_mut(),
            runtime: self as *const WasmMac as *mut WasmMac,
        });

        let mut config = Config::create().map_err(|e| format!("wasm config: {}", e))?;
        let bytes = self.ram_limit_mb.saturating_mul(1024).saturating_mul(1024);
        let pages = ((bytes + 65535) / 65536).max(1);
        config.set_max_memory_pages((pages.min(u32::MAX as u64)) as u32);
        let mut store = Store::create().map_err(|e| format!("wasm store: {}", e))?;
        let wasi_mod =
            WasiModule::create(None, None, None).map_err(|e| format!("wasi module: {}", e))?;

        // Statistics are deliberately NOT attached: cost was measured but never
        // read back, and wasmedge-sys 0.20's `Executor::create` frees the moved-in
        // `Statistics` immediately while the executor keeps referencing it — a
        // use-after-free that segfaults at instantiation once cost measurement is
        // on. Passing `None` (and dropping `measure_cost`) is both correct and
        // faithful to the old behaviour, which never consumed the cost anyway.
        // Boxed so `host_data.exec` stays valid when the VmStack moves.
        let mut executor = Box::new(
            Executor::create(Some(&config), None).map_err(|e| format!("wasm executor: {}", e))?,
        );
        host_data.exec = &mut *executor as *mut Executor;

        // The `env` import module owns an (unused) i32 payload; use an owned
        // value, never a borrow of a local, so the module can outlive this fn.
        let mut extern_mod: ImportModule<i32> = ImportModule::create("env", Box::new(0i32))
            .map_err(|e| format!("env import module: {}", e))?;
        let host_func = unsafe {
            Function::create_sync_func::<HostData>(
                &FuncType::new(vec![ValType::I32, ValType::I32], vec![ValType::I64]),
                host_call,
                &mut *host_data as *mut HostData,
                1,
            )
            .map_err(|e| format!("hostCall create: {}", e))?
        };
        extern_mod.add_func("hostCall", host_func);

        executor
            .register_import_module(&mut store, &wasi_mod)
            .map_err(|e| format!("register wasi: {}", e))?;
        executor
            .register_import_module(&mut store, &extern_mod)
            .map_err(|e| format!("register env: {}", e))?;

        // Prefer a cached AOT (native-code) artifact for this module; fall back
        // to the raw `.wasm` when AOT is unavailable.
        let (load_path, _is_aot) = resolve_module_artifact(mod_path);
        let conf = Config::create().map_err(|e| format!("loader config: {}", e))?;
        let loader = Loader::create(Some(&conf)).map_err(|e| format!("loader: {}", e))?;
        let module = loader
            .from_file(&load_path)
            .map_err(|e| format!("load {}: {}", load_path, e))?;
        // WasmEdge 0.15+ requires every module — AOT-loaded ones included — to
        // carry validation state before it can be instantiated (0.14 accepted a
        // pre-validated AOT artifact unvalidated). Validation now runs once per
        // VM build, which the warm-VM cache makes rare, so the cost is amortized.
        let conf2 = Config::create().map_err(|e| format!("validator config: {}", e))?;
        let v = Validator::create(Some(&conf2)).map_err(|e| format!("validator: {}", e))?;
        v.validate(&module)
            .map_err(|e| format!("validate {}: {}", load_path, e))?;

        let mut instance = executor
            .register_active_module(&mut store, &module)
            .map_err(|e| format!("register active module: {}", e))?;

        // Run the guest runtime's one-time init (`_start`).
        {
            let mut start = instance
                .get_func_mut("_start")
                .map_err(|e| format!("missing _start export: {}", e))?;
            executor
                .call_func(&mut start, [])
                .map_err(|e| format!("_start call: {}", e))?;
        }

        // Capture the post-init linear memory so it can be rewound each run.
        let (mem_snapshot, initial_pages) = {
            let mem = instance
                .get_memory_mut("memory")
                .map_err(|e| format!("get memory: {}", e))?;
            let initial_pages = mem.size();
            let snap = mem
                .get_data(0, initial_pages.saturating_mul(WASM_PAGE as u32))
                .map_err(|e| format!("snapshot memory: {}", e))?;
            (snap, initial_pages)
        };

        Ok(VmStack {
            instance,
            _module: module,
            _env: extern_mod,
            _wasi: wasi_mod,
            _store: store,
            _config: config,
            executor,
            host_data,
            mem_snapshot,
            initial_pages,
            ram_limit_mb: self.ram_limit_mb,
            mtime: cur_mtime,
            runs: 0,
        })
    }

    /// Run one signal on a (possibly reused) warm VM: rewind linear memory to
    /// the post-`_start` snapshot, re-point host calls at this `WasmMac`, then
    /// `malloc` the input and invoke `run`. `_start` is NOT re-run — the memory
    /// rewind restores exactly the post-init state, so the guest sees an
    /// identical fresh start every time.
    fn run_on_stack(&mut self, stack: &mut VmStack, input: String) -> Result<(), String> {
        // Serve this signal's host calls: trx, callback and stop flag all live
        // on `self`. (`exec` is stable, re-set defensively.)
        stack.host_data.runtime = self as *mut WasmMac;
        stack.host_data.exec = &mut *stack.executor as *mut Executor;

        // Rewind the guest's linear memory to its post-init bytes — this resets
        // the guest allocator and every byte of in-memory state, so no data
        // survives from a previous run on this warm VM.
        {
            let mut mem = stack
                .instance
                .get_memory_mut("memory")
                .map_err(|e| format!("get memory: {}", e))?;
            mem.set_data(&stack.mem_snapshot, 0)
                .map_err(|e| format!("reset memory: {}", e))?;
        }
        stack.runs = stack.runs.saturating_add(1);

        if self.stop_.load(Ordering::Relaxed) {
            return Ok(());
        }

        // malloc(input_len) → offset in the (freshly reset) linear memory.
        let val_l = input.len() as i32;
        let val_offset = {
            let mut malloc_fn = stack
                .instance
                .get_func_mut("malloc")
                .map_err(|e| format!("missing malloc export: {}", e))?;
            let res2 = stack
                .executor
                .call_func(&mut malloc_fn, [WasmValue::from_i32(val_l)])
                .map_err(|e| format!("malloc call: {}", e))?;
            res2.get(0)
                .map(|v| v.to_i32())
                .ok_or_else(|| "malloc returned no value".to_string())?
        };

        {
            let arr: Vec<u8> = input.as_bytes().to_vec();
            let mut mem = stack
                .instance
                .get_memory_mut("memory")
                .map_err(|e| format!("get memory: {}", e))?;
            mem.set_data(arr, val_offset.cast_unsigned())
                .map_err(|e| format!("set_data: {}", e))?;
        }

        let c = ((val_offset as i64) << 32) | (val_l as i64);
        if self.stop_.load(Ordering::Relaxed) {
            return Ok(());
        }

        let mut run_fn = stack
            .instance
            .get_func_mut("run")
            .map_err(|e| format!("missing run export: {}", e))?;
        stack
            .executor
            .call_func(&mut run_fn, [WasmValue::from_i64(c)])
            .map_err(|e| format!("run call: {}", e))?;
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop_.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod execution_tests {
    //! Hermetic checks that WasmEdge (via the runtime) actually *executes* a
    //! real TinyGo/WASI module — exercising the cold build, warm-VM reuse and
    //! per-run linear-memory reset paths — without the node/QuestDB around it.
    //! A segfault or trap here fails the test process.
    use super::*;

    fn run_module_n(path: &str, n: usize) {
        assert!(
            std::path::Path::new(path).is_file(),
            "test module {} missing",
            path
        );
        for i in 0..n {
            let mut rt = WasmMac::new_vm(
                "test-machine".into(),
                format!("vm-{}", i),
                String::new(),
                path.into(),
                64,
                Box::new(|_v| String::new()),
            );
            rt.execute_on_update("{}".into())
                .unwrap_or_else(|e| panic!("run {} of {} failed: {}", i, path, e));
        }
    }

    // 25 runs > 1 build, so this covers the cold build once and then warm-VM
    // checkout + memory-reset reuse for the remaining runs.
    #[test]
    fn empty_module_runs_repeatedly() {
        run_module_n("tests_empty_module.wasm", 25);
    }

    // A module with a mutable in-memory global; running it many times through
    // the warm-VM pool must not crash (state-reset correctness itself is
    // asserted end-to-end against a live node).
    #[test]
    fn stateful_module_runs_repeatedly() {
        run_module_n("tests_state_module.wasm", 25);
    }

    // The legacy (cache-disabled) path must also still execute the module.
    #[test]
    fn empty_module_runs_with_cache_disabled() {
        std::env::set_var("CASPAR_WASM_VM_CACHE", "0");
        run_module_n("tests_empty_module.wasm", 5);
        std::env::remove_var("CASPAR_WASM_VM_CACHE");
    }
}
