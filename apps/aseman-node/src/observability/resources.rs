//! Host resource sampling for the telemetry snapshot.
//!
//! `casparctl stats` historically read CPU / memory from `docker stats`, which
//! only works when the node runs inside a container. The node itself, however,
//! can always read its own machine's resources straight from Linux `/proc` and
//! `statvfs(2)` — whether it was launched by Docker or as a bare
//! `casparctl run` process. Collecting them here makes CPU / RAM / disk part of
//! the `/telemetry/snapshot` payload, so every consumer (casparctl, the edge,
//! the admin panel) gets the same numbers with no Docker dependency.
//!
//! All reads are best-effort: a field that cannot be sampled is simply omitted
//! rather than failing the whole snapshot.

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

use serde_json::{Value, json};

/// One aggregate CPU-time reading from the first line of `/proc/stat`, in USER_HZ
/// ticks. `busy` excludes idle + iowait; `total` is every counted state.
#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    busy: u64,
    total: u64,
}

/// Remembers the previous CPU sample so a busy-percentage can be derived from
/// the delta between two collections (the snapshot is re-collected every ~2 s).
#[derive(Default)]
pub struct ResourceSampler {
    last_cpu: Mutex<Option<CpuSample>>,
}

impl ResourceSampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Collect a `resources` map for the snapshot. `disk_path` is the filesystem
    /// whose usage is reported (the node's storage root, falling back to `/`).
    pub fn collect(&self, disk_path: &str) -> HashMap<String, Value> {
        let mut m: HashMap<String, Value> = HashMap::new();

        if let Some(pct) = self.cpu_percent() {
            m.insert("cpu_percent".to_string(), json!(round2(pct)));
        }
        if let Some(cores) = cpu_cores() {
            m.insert("cpu_cores".to_string(), json!(cores));
        }
        if let Some((l1, l5, l15)) = load_avg() {
            m.insert("load_avg_1m".to_string(), json!(round2(l1)));
            m.insert("load_avg_5m".to_string(), json!(round2(l5)));
            m.insert("load_avg_15m".to_string(), json!(round2(l15)));
        }
        if let Some((total, available)) = mem_info() {
            let used = total.saturating_sub(available);
            m.insert("mem_total_bytes".to_string(), json!(total));
            m.insert("mem_available_bytes".to_string(), json!(available));
            m.insert("mem_used_bytes".to_string(), json!(used));
            if total > 0 {
                m.insert(
                    "mem_used_percent".to_string(),
                    json!(round2(used as f64 * 100.0 / total as f64)),
                );
            }
        }
        if let Some(rss) = process_rss_bytes() {
            m.insert("process_rss_bytes".to_string(), json!(rss));
        }
        let path = if disk_path.trim().is_empty() {
            "/"
        } else {
            disk_path
        };
        if let Some((total, free)) = disk_usage(path) {
            let used = total.saturating_sub(free);
            m.insert("disk_path".to_string(), json!(path));
            m.insert("disk_total_bytes".to_string(), json!(total));
            m.insert("disk_free_bytes".to_string(), json!(free));
            m.insert("disk_used_bytes".to_string(), json!(used));
            if total > 0 {
                m.insert(
                    "disk_used_percent".to_string(),
                    json!(round2(used as f64 * 100.0 / total as f64)),
                );
            }
        }
        if let Some(up) = host_uptime_sec() {
            m.insert("host_uptime_sec".to_string(), json!(up));
        }
        m
    }

    /// Busy-CPU percentage across all cores, from the delta since the previous
    /// sample. The first call has no prior delta, so it takes a short in-line
    /// sample (~120 ms) to return a meaningful number instead of zero.
    fn cpu_percent(&self) -> Option<f64> {
        let mut guard = self.last_cpu.lock().ok()?;
        let (prev, now) = match *guard {
            // Steady state: compare against the sample from the previous collect.
            Some(p) => (p, read_cpu_sample()?),
            // First collect: no prior delta, so take two samples ~120 ms apart.
            None => {
                let first = read_cpu_sample()?;
                std::thread::sleep(std::time::Duration::from_millis(120));
                (first, read_cpu_sample()?)
            }
        };
        *guard = Some(now);
        let d_total = now.total.saturating_sub(prev.total);
        let d_busy = now.busy.saturating_sub(prev.busy);
        if d_total == 0 {
            return Some(0.0);
        }
        Some((d_busy as f64 * 100.0 / d_total as f64).clamp(0.0, 100.0))
    }
}

fn read_cpu_sample() -> Option<CpuSample> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    if !line.starts_with("cpu ") && !line.starts_with("cpu\t") {
        return None;
    }
    // cpu user nice system idle iowait irq softirq steal guest guest_nice
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|t| t.parse::<u64>().ok())
        .collect();
    if vals.len() < 4 {
        return None;
    }
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = vals.iter().sum();
    Some(CpuSample {
        busy: total.saturating_sub(idle),
        total,
    })
}

fn cpu_cores() -> Option<u64> {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 {
        return Some(n as u64);
    }
    // Fallback: count `processor` lines in /proc/cpuinfo.
    let info = fs::read_to_string("/proc/cpuinfo").ok()?;
    let count = info.lines().filter(|l| l.starts_with("processor")).count();
    if count > 0 { Some(count as u64) } else { None }
}

fn load_avg() -> Option<(f64, f64, f64)> {
    let raw = fs::read_to_string("/proc/loadavg").ok()?;
    let mut it = raw.split_whitespace();
    let l1 = it.next()?.parse::<f64>().ok()?;
    let l5 = it.next()?.parse::<f64>().ok()?;
    let l15 = it.next()?.parse::<f64>().ok()?;
    Some((l1, l5, l15))
}

/// Returns `(total_bytes, available_bytes)` from `/proc/meminfo`.
fn mem_info() -> Option<(u64, u64)> {
    let raw = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = parse_kb(rest);
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }
    let total = total_kb?.saturating_mul(1024);
    let avail = avail_kb.unwrap_or(0).saturating_mul(1024).min(total);
    Some((total, avail))
}

fn parse_kb(rest: &str) -> Option<u64> {
    rest.split_whitespace().next()?.parse::<u64>().ok()
}

fn process_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return parse_kb(rest).map(|kb| kb.saturating_mul(1024));
        }
    }
    None
}

fn host_uptime_sec() -> Option<u64> {
    let raw = fs::read_to_string("/proc/uptime").ok()?;
    let first = raw.split_whitespace().next()?;
    first.parse::<f64>().ok().map(|s| s as u64)
}

/// `(total_bytes, free_bytes)` for the filesystem holding `path`, via `statvfs`.
fn disk_usage(path: &str) -> Option<(u64, u64)> {
    let c_path = std::ffi::CString::new(path).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    let frsize = stat.f_frsize as u64;
    let total = frsize.saturating_mul(stat.f_blocks as u64);
    // Available to unprivileged users (matches `df`'s "Avail").
    let free = frsize.saturating_mul(stat.f_bavail as u64);
    if total == 0 {
        return None;
    }
    Some((total, free))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
