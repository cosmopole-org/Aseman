//! Translation of `kasper/src/shell` — the Caspar HTTP/TCP shell.
//!
//! Phase 5 of the plan covers the full API tree. This Phase 3 commit lands
//! only the lightweight `utils/` submodules that other modules already depend
//! on (`crypto`, `future`, `origin`, `timer`). The bulk of `shell::api` and
//! the source-comment-driven `extractor` / `doc` helpers wait for Phase 5.

pub mod api;
pub(crate) mod audit;
pub(crate) mod authority;
pub mod kasper;
pub mod storage_http;
pub mod utils;
pub(crate) mod workloads;
