//! Runtime models for the fire (Firecracker) VM plugin.

use std::path::PathBuf;
use std::process::{Child, ChildStdin};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A live Firecracker guest supervised by this plugin.
pub struct FireVmProcess {
    pub machine_id: String,
    pub vm_id: String,
    pub requester_user_id: String,
    pub stream_store_id: String,
    pub socket_path: PathBuf,
    pub child: Child,
    pub stdin: Arc<Mutex<ChildStdin>>,
    pub output: Arc<Mutex<String>>,
    pub io_stop: Arc<AtomicBool>,
    pub stdout_thread: Option<JoinHandle<()>>,
    pub stderr_thread: Option<JoinHandle<()>>,
    /// Per-session persistent sandbox directory under `{storage}/vms/...`.
    /// Retained across suspend/resume so the session can be woken with all of
    /// its installed software and data intact; only removed on an explicit
    /// purge (delete).
    pub vm_dir: Option<PathBuf>,
    /// When true the backing `vm_dir` survives `terminate_vm` (suspend); a
    /// caller asking to delete the sandbox passes `purge: true`.
    pub persistent: bool,
}
