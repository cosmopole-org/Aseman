//! Runtime profiling HTTP server — Rust-native replacement for the Go
//! `net/http/pprof` block that lived in `node.old/node/main.go:30-42`.
//!
//! Backed by the [`pprof`](https://crates.io/crates/pprof) crate (CPU
//! sampling profiler with flamegraph + protobuf output). Designed to be
//! queried by `casparctl pprof <subcmd>`; nothing else in the node touches
//! it.
//!
//! Endpoints, all served from the typed pprof port (default `9999`, same port as
//! the Go original):
//!
//! | Path                          | Body                                  |
//! |-------------------------------|---------------------------------------|
//! | `/debug/pprof/`               | text index of the routes below        |
//! | `/debug/pprof/runtime`        | JSON: pid, uptime, threads, os, arch  |
//! | `/debug/pprof/flamegraph?seconds=N` | SVG flamegraph (N up to 60)     |
//! | `/debug/pprof/profile?seconds=N`    | pprof protobuf (`go tool pprof`-compatible) |
//! | `/debug/pprof/heap`           | JSON: rss/vsize/rss_peak from `/proc/self/status` |
//! | `/debug/pprof/threads`        | JSON: list of `/proc/self/task/*` names + states |
//!
//! All sampling is opt-in (the profiler is started **per request**) so
//! the steady-state cost when nobody is querying is zero.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pprof::ProfilerGuardBuilder;
use serde_json::{Value, json};

static STARTED_AT_UNIX: AtomicU64 = AtomicU64::new(0);

/// Spawn the profiling HTTP server in a background thread. Returns
/// immediately. Errors during bind are logged but don't kill the node.
pub fn start(port: u16) {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    STARTED_AT_UNIX.store(started, Ordering::Relaxed);
    let listener = match TcpListener::bind(format!("0.0.0.0:{}", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pprof server listen failed: {}", e);
            return;
        }
    };
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || handle(stream));
        }
    });
}

fn handle(mut stream: TcpStream) {
    let peer = stream.try_clone();
    let mut reader = match peer {
        Ok(s) => BufReader::new(s),
        Err(_) => return,
    };
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let raw_path = parts[1];
    let (path, query) = match raw_path.find('?') {
        Some(i) => (&raw_path[..i], &raw_path[i + 1..]),
        None => (raw_path, ""),
    };

    let (status, content_type, body): (u16, &str, Vec<u8>) = match path {
        "/debug/pprof/" | "/debug/pprof" => (200, "text/plain", index_body().into_bytes()),
        "/debug/pprof/runtime" => json_response(runtime_info()),
        "/debug/pprof/heap" => json_response(heap_info()),
        "/debug/pprof/threads" => json_response(threads_info()),
        "/debug/pprof/flamegraph" => match render_flamegraph(parse_seconds(query)) {
            Ok(svg) => (200, "image/svg+xml", svg),
            Err(e) => json_response(json!({ "error": e })),
        },
        "/debug/pprof/profile" => match render_pprof_protobuf(parse_seconds(query)) {
            Ok(pb) => (200, "application/octet-stream", pb),
            Err(e) => json_response(json!({ "error": e })),
        },
        _ => json_response(json!({ "error": "not found" })),
    };

    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        status_phrase(status),
        content_type,
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

fn status_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

fn json_response(v: Value) -> (u16, &'static str, Vec<u8>) {
    let body = serde_json::to_vec(&v).unwrap_or_else(|_| b"{}".to_vec());
    (200, "application/json", body)
}

fn parse_seconds(query: &str) -> u64 {
    for pair in query.split('&') {
        if let Some(("seconds", v)) = pair.split_once('=') {
            if let Ok(n) = v.parse::<u64>() {
                return n.clamp(1, 60);
            }
        }
    }
    5
}

fn index_body() -> String {
    "casparctl pprof endpoints:\n\
     /debug/pprof/runtime               — runtime info (json)\n\
     /debug/pprof/heap                  — heap / memory stats (json)\n\
     /debug/pprof/threads               — per-thread states (json)\n\
     /debug/pprof/flamegraph?seconds=N  — CPU flamegraph (svg)\n\
     /debug/pprof/profile?seconds=N     — CPU profile (pprof protobuf)\n"
        .to_string()
}

fn runtime_info() -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let started = STARTED_AT_UNIX.load(Ordering::Relaxed);
    let uptime = now.saturating_sub(started);
    json!({
        "pid": std::process::id(),
        "started_at_unix": started,
        "uptime_sec": uptime,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "rust_version": option_env!("CARGO_PKG_RUST_VERSION").unwrap_or("unknown"),
        "threads": list_threads().len(),
    })
}

fn heap_info() -> Value {
    let mut out = json!({});
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                let v = v.trim();
                if matches!(
                    k,
                    "VmPeak"
                        | "VmSize"
                        | "VmHWM"
                        | "VmRSS"
                        | "VmData"
                        | "VmStk"
                        | "VmExe"
                        | "VmLib"
                        | "VmPTE"
                        | "VmSwap"
                ) {
                    out[k.to_string()] = json!(v);
                }
            }
        }
    }
    out
}

fn threads_info() -> Value {
    let threads: Vec<Value> = list_threads()
        .into_iter()
        .map(|(tid, name, state)| {
            json!({
                "tid": tid,
                "name": name,
                "state": state,
            })
        })
        .collect();
    json!({ "count": threads.len(), "threads": threads })
}

fn list_threads() -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let task_dir = match fs::read_dir("/proc/self/task") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in task_dir.flatten() {
        let tid = entry.file_name().to_string_lossy().to_string();
        let comm = fs::read_to_string(entry.path().join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let state = fs::read_to_string(entry.path().join("stat"))
            .ok()
            .and_then(|s| {
                // /proc/<tid>/stat — field 3 is single-char state. Format
                // is `pid (comm) state ppid ...`; comm may contain spaces
                // and parens, so slice from the last ')'.
                s.rfind(')').and_then(|i| {
                    let rest = &s[i + 1..];
                    rest.split_whitespace().next().map(|t| t.to_string())
                })
            })
            .unwrap_or_else(|| "?".to_string());
        out.push((tid, comm, state));
    }
    out
}

fn build_profiler(seconds: u64) -> Result<pprof::Report, String> {
    let guard = ProfilerGuardBuilder::default()
        .frequency(99)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .map_err(|e| format!("profiler build: {}", e))?;
    thread::sleep(Duration::from_secs(seconds));
    guard.report().build().map_err(|e| format!("report: {}", e))
}

fn render_flamegraph(seconds: u64) -> Result<Vec<u8>, String> {
    let report = build_profiler(seconds)?;
    let mut svg = Vec::new();
    report
        .flamegraph(&mut svg)
        .map_err(|e| format!("flamegraph: {}", e))?;
    Ok(svg)
}

fn render_pprof_protobuf(seconds: u64) -> Result<Vec<u8>, String> {
    use pprof::protos::Message;
    let report = build_profiler(seconds)?;
    let profile = report.pprof().map_err(|e| format!("pprof: {}", e))?;
    profile
        .write_to_bytes()
        .map_err(|e| format!("encode: {}", e))
}
