//! Translation of `kasper/src/shell/utils`.
//!
//! Currently translated: `crypto`, `future`, `origin`, `timer`. The Go
//! `extractor.go` / `doc.go` helpers use `runtime.FuncForPC` and `go/parser`
//! to harvest action metadata from source comments — those have no portable
//! Rust counterpart and will be re-implemented in a different style in
//! Phase 5 (likely a derive macro or an attribute-driven registry).

pub mod crypto;
pub mod future;
pub mod origin;
pub mod secret_crypto;
pub mod timer;
