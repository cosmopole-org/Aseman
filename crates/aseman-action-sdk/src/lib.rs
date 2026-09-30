//! # Aseman action SDK
//!
//! The interface contract between the Aseman node and pluggable action
//! implementations (ADR 0040).
//!
//! An action plugin is a standalone Rust project living under `modules/actions/`
//! that:
//!
//! 1. depends on this crate,
//! 2. implements the [`plugin::ActionPlugin`] trait,
//! 3. declares the operations it contributes ([`operation::ActionOperationSpec`]
//!    — a path and where a signed packet runs it), and
//! 4. exposes a `pub fn register()` entry point that builds its plugin and calls
//!    [`registry::register_plugin`].
//!
//! At node start-up an aggregation crate (`aseman-action-plugins`, mirroring the
//! generated `vm-plugins` crate) imports every action plugin and calls its
//! `register` function, so the plugins are statically compiled into the single
//! node binary while the node itself never names an action.
//!
//! At runtime the node publishes an [`context::ActionNode`] implementation
//! through a [`context::NodeFacade`] and runs each operation's handler with an
//! [`context::ActionContext`] — the operation's transaction, the caller, and
//! the facade. Handlers reach the node's services (id minting, master key,
//! signature verification, the signaler, workloads, the VMM, network peers)
//! exclusively through that interface.
//!
//! The [`state`] and [`wire`] modules hold the state port adapters, wire
//! models, and wire shapes the node's actions run on; the node re-exports them
//! from its own `state/` and `actions/wire/` modules so its internal callers are
//! unchanged.

pub mod blobs;
pub mod caller;
pub mod context;
pub mod error;
pub mod operation;
pub mod origin;
pub mod plugin;
pub mod proxy;
pub mod registry;
pub mod state;
pub mod util;
pub mod wire;

pub use caller::ActionCaller;
pub use context::{LEGACY_ROOT, ActionContext, ActionNetwork, ActionNode, ActionSecurity, ActionSignaler,
    ActionStorage, ActionTools, ActionVmm, ActionWorkloads, NodeFacade, VmCosts};
pub use error::{ActionError, InvalidInput, action_error, parse};
pub use operation::ActionOperationSpec;
pub use origin::ActionOrigin;
pub use plugin::{ActionPlugin, ActionPluginMeta};
pub use state::{Creature, Program, Session, Store};
pub use util::{Ctx, Trx};