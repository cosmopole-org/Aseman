//! Translation shim for Go's standard `log` package.
//!
//! Go code throughout the node uses `log.Println` / `log.Printf` / `log.Fatal`
//! / `log.Panic`. These macros reproduce that behaviour: a `Ldate | Ltime`
//! timestamp prefix followed by the message on stderr.

/// Produces the `2006/01/02 15:04:05` timestamp prefix Go's `log` uses.
pub fn now_prefix() -> String {
    chrono::Local::now().format("%Y/%m/%d %H:%M:%S").to_string()
}

/// Equivalent of `log.Println` / `log.Print`.
#[macro_export]
macro_rules! go_println {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        eprintln!("{} {}", $crate::compat::golog::now_prefix(), __m.trim_end_matches('\n'));
    }};
}

/// Equivalent of `log.Printf`.
#[macro_export]
macro_rules! go_printf {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        eprintln!("{} {}", $crate::compat::golog::now_prefix(), __m.trim_end_matches('\n'));
    }};
}

/// Equivalent of `log.Fatal` / `log.Fatalf` / `log.Fatalln` — print then exit(1).
#[macro_export]
macro_rules! go_fatal {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        eprintln!("{} {}", $crate::compat::golog::now_prefix(), __m.trim_end_matches('\n'));
        std::process::exit(1);
    }};
}

/// Equivalent of `log.Panic` / `log.Panicf` / `log.Panicln` — print then panic.
#[macro_export]
macro_rules! go_panic {
    ($($arg:tt)*) => {{
        let __m = format!($($arg)*);
        eprintln!("{} {}", $crate::compat::golog::now_prefix(), __m.trim_end_matches('\n'));
        panic!("{}", __m);
    }};
}
