//! Go-compatibility shims.
//!
//! Thin wrappers that reproduce the parts of Go's standard library used by
//! the translated codebase, so the Go-style call sites stay legible without
//! pulling in heavy dependencies:
//!   * `golog` — `log.Println` / `log.Printf` / `log.Fatal` / `log.Panic`.
//!     The macros are `#[macro_export]` so they remain reachable at crate
//!     root (`go_println!`, `go_printf!`, …).
//!   * `multipart` — `mime/multipart.FileHeader` equivalent used by the
//!     shell's file-upload routes.

#[macro_use]
pub mod golog;
pub mod multipart;
