//! Action input payloads.
//!
//! [`IInput::as_any`] lets the secured-action layer recover the original
//! typed value from `Arc<dyn IInput>` without a JSON round-trip.

use std::any::Any;

/// An action input payload. Concrete inputs live under
/// [`crate::shell::api::packets`].
pub trait IInput: Send + Sync + Any {
    fn get_store_id(&self) -> String;
    fn origin(&self) -> String;

    /// Downcast hook — every concrete implementation should return
    /// `self` so `Arc<dyn IInput>` can recover the original typed value.
    fn as_any(&self) -> &dyn Any;
}
