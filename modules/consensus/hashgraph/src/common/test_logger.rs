//! Translation of `chain/common/test_logger.go`.
//!
//! The Go original wires a logrus logger into `testing.T.Log` so that log
//! output is only shown for failing tests. Rust's test harness already
//! captures per-test output, so the translated helpers simply return a logrus
//! shim `Entry`/`Logger` at the requested level.

use std::sync::Arc;

use crate::logrus::{Entry, Level, Logger};

/// The level used in tests by default.
pub const TEST_LOG_LEVEL: Level = Level::Info;

/// Returns a logrus `Logger` for testing.
pub fn new_test_logger(level: Level) -> Arc<Logger> {
    let logger = Logger::new();
    logger.set_level(level);
    logger
}

/// Returns a logrus `Entry` for testing.
pub fn new_test_entry(level: Level) -> Entry {
    Entry::new(new_test_logger(level))
}
