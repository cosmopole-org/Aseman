//! A minimal translation shim for `github.com/sirupsen/logrus`.
//!
//! The vendored Babble hashgraph/node code logs through logrus
//! (`*logrus.Entry`, `*logrus.Logger`, `logrus.FieldLogger`). This module
//! reproduces the parts of the API the node uses: levelled logging plus the
//! `WithField` / `WithError` / `WithFields` structured-field builders.

use std::fmt::Display;
use std::sync::{Arc, RwLock};

/// Mirrors `logrus.Level`. Numeric order matches logrus exactly (lower is more
/// severe), so `logger.level >= msg.level` decides whether a line is emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Panic = 0,
    Fatal = 1,
    Error = 2,
    Warn = 3,
    Info = 4,
    Debug = 5,
    Trace = 6,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Panic => "panic",
            Level::Fatal => "fatal",
            Level::Error => "error",
            Level::Warn => "warning",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
}

/// Mirrors `logrus.Logger` — essentially just a shared, mutable level.
#[derive(Debug)]
pub struct Logger {
    level: RwLock<Level>,
}

impl Logger {
    /// Equivalent of `logrus.New()`.
    pub fn new() -> Arc<Logger> {
        Arc::new(Logger {
            level: RwLock::new(Level::Info),
        })
    }

    pub fn set_level(&self, level: Level) {
        *self.level.write().unwrap() = level;
    }

    pub fn level(&self) -> Level {
        *self.level.read().unwrap()
    }
}

impl Default for Logger {
    fn default() -> Self {
        Logger {
            level: RwLock::new(Level::Info),
        }
    }
}

/// Mirrors `logrus.Entry` — a logger plus a set of accumulated fields.
///
/// Cloning an `Entry` is cheap (it shares the underlying `Logger`).
#[derive(Clone)]
pub struct Entry {
    logger: Arc<Logger>,
    fields: Vec<(String, String)>,
}

impl Entry {
    /// Equivalent of `logrus.NewEntry(logger)`.
    pub fn new(logger: Arc<Logger>) -> Entry {
        Entry {
            logger,
            fields: Vec::new(),
        }
    }

    /// Convenience constructor producing a brand new logger + entry.
    pub fn standalone() -> Entry {
        Entry::new(Logger::new())
    }

    pub fn logger(&self) -> Arc<Logger> {
        self.logger.clone()
    }

    /// `WithField(key, value)`.
    pub fn with_field(&self, key: &str, value: impl Display) -> Entry {
        let mut fields = self.fields.clone();
        fields.push((key.to_string(), value.to_string()));
        Entry {
            logger: self.logger.clone(),
            fields,
        }
    }

    /// `WithFields(logrus.Fields{...})`.
    pub fn with_fields(&self, kvs: &[(&str, String)]) -> Entry {
        let mut fields = self.fields.clone();
        for (k, v) in kvs {
            fields.push((k.to_string(), v.clone()));
        }
        Entry {
            logger: self.logger.clone(),
            fields,
        }
    }

    /// `WithError(err)`.
    pub fn with_error(&self, err: impl Display) -> Entry {
        self.with_field("error", err)
    }

    fn emit(&self, level: Level, msg: &str) {
        if self.logger.level() < level {
            return;
        }
        let mut line = format!(
            "{} [{}] {}",
            crate::compat::golog::now_prefix(),
            level.label(),
            msg
        );
        for (k, v) in &self.fields {
            line.push_str(&format!(" {}={}", k, v));
        }
        eprintln!("{}", line);
    }

    pub fn trace(&self, msg: impl Display) {
        self.emit(Level::Trace, &msg.to_string());
    }
    pub fn debug(&self, msg: impl Display) {
        self.emit(Level::Debug, &msg.to_string());
    }
    pub fn info(&self, msg: impl Display) {
        self.emit(Level::Info, &msg.to_string());
    }
    pub fn warn(&self, msg: impl Display) {
        self.emit(Level::Warn, &msg.to_string());
    }
    pub fn error(&self, msg: impl Display) {
        self.emit(Level::Error, &msg.to_string());
    }
    pub fn fatal(&self, msg: impl Display) {
        self.emit(Level::Fatal, &msg.to_string());
        std::process::exit(1);
    }
    pub fn panic(&self, msg: impl Display) {
        let s = msg.to_string();
        self.emit(Level::Panic, &s);
        panic!("{}", s);
    }
}

impl Default for Entry {
    fn default() -> Self {
        Entry::standalone()
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "logrus::Entry({} fields)", self.fields.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_set_and_get_level_round_trip() {
        let logger = Logger::new();
        logger.set_level(Level::Warn);
        assert_eq!(logger.level(), Level::Warn);
        logger.set_level(Level::Trace);
        assert_eq!(logger.level(), Level::Trace);
    }

    #[test]
    fn level_ordering_is_ascending_by_verbosity() {
        // logrus levels: Panic(0) < Fatal < Error < Warn < Info < Debug < Trace(6).
        // The Rust enum derives PartialOrd from its discriminant, so more
        // verbose levels compare greater.
        assert!(Level::Panic < Level::Error);
        assert!(Level::Error < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn standalone_entry_has_no_fields() {
        let e = Entry::standalone();
        assert!(format!("{:?}", e).contains("0 fields"));
    }

    #[test]
    fn with_field_grows_field_count() {
        let e = Entry::standalone().with_field("k", "v");
        assert!(format!("{:?}", e).contains("1 fields"));
    }

    #[test]
    fn with_fields_appends_all_pairs() {
        let e = Entry::standalone().with_fields(&[("a", "1".to_string()), ("b", "2".to_string())]);
        assert!(format!("{:?}", e).contains("2 fields"));
    }

    #[test]
    fn with_error_records_a_field() {
        let e = Entry::standalone().with_error("oops");
        assert!(format!("{:?}", e).contains("1 fields"));
    }

    #[test]
    fn emit_at_higher_verbosity_than_logger_is_suppressed_without_panic() {
        let logger = Logger::new();
        logger.set_level(Level::Error);
        let e = Entry::new(logger);
        // These all exercise the suppressed branch (level > Error). They
        // should not panic nor terminate the process.
        e.trace("nope");
        e.debug("nope");
        e.info("nope");
        e.warn("nope");
    }
}
