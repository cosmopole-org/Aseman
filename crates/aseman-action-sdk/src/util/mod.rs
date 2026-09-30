//! Shared helpers the action handlers call: the [`Ctx`] wrapper a plugin builds
//! from its [`crate::ActionContext`], the storage transaction, the node clock,
//! and the small utilities the handlers use.

pub mod crypto;
pub mod future;
pub mod secret_crypto;

use aseman_ports::ClockPort;

pub use aseman_storage::Trx;

pub use crypto::secure_unique_string;
pub use future::async_once;

use crate::caller::ActionCaller;
use crate::context::NodeFacade;

/// The node clock, as the application's ports read it.
pub struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// What a launched instance may use (the `resources` input, normalized).
#[derive(Clone, Copy, Debug)]
pub struct LaunchResources {
    pub cpu_cores: i64,
    pub ram_mb: i64,
    pub disk_gb: i64,
    pub max_exec_time_seconds: i64,
}

impl Default for LaunchResources {
    fn default() -> Self {
        Self {
            cpu_cores: 1,
            ram_mb: 64,
            disk_gb: 1,
            max_exec_time_seconds: 60,
        }
    }
}

/// What a handler runs against: the node facade, the operation's transaction,
/// and the caller — the shape the plugin handlers were written against.
pub struct Ctx<'a> {
    pub node: &'a NodeFacade,
    pub trx: &'a Trx,
    pub caller: &'a ActionCaller,
}

impl<'a> Ctx<'a> {
    /// Wrap an [`crate::ActionContext`] into the handler shape.
    #[must_use]
    pub fn new(ctx: &'a dyn crate::ActionContext) -> Self {
        Self {
            node: ctx.node(),
            trx: ctx.trx(),
            caller: ctx.caller(),
        }
    }
}