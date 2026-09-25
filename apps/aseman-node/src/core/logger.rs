//! Translation of `core/module/logger/logger.go` (Go package `module_logger`).

/// A trivial logger facade that forwards to the standard log output.
#[derive(Debug, Default, Clone)]
pub struct Logger;

impl Logger {
    pub fn new() -> Self {
        Logger
    }

    /// Equivalent of Go's `(*Logger).Println(args ...interface{})`, which
    /// forwards the whole argument slice to the standard `log` package.
    pub fn println<T: std::fmt::Debug>(&self, args: &[T]) {
        crate::go_println!("{:?}", args);
    }
}
