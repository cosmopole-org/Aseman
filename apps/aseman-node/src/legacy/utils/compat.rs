//! Go-translation compatibility aliases used by the gated legacy translation.
//!
//! These types mirror Go idioms for the translated node code; they belong to the
//! legacy surface and are replaced as the families they support are migrated.

/// Convenience alias mirroring Go's `error` value type.
pub type GoError = anyhow::Error;

/// Opaque value, the translation of Go's empty interface `interface{}` / `any`
/// when it is used for dynamic, downcastable values rather than JSON payloads.
pub type AnyVal = std::sync::Arc<dyn std::any::Any + Send + Sync>;
