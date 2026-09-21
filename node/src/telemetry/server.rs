//! Translation of `telemetry/server.go`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use aseman_config::AsemanConfig;
use aseman_storage_legacy::{LegacyKvStore, LegacyKvWrite, RocksDbKvStore};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// JSON snapshot returned from `/telemetry/snapshot`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub timestamp: String,
    pub uptime_sec: i64,
    pub node: HashMap<String, Value>,
    pub chain: HashMap<String, Value>,
    pub federation: HashMap<String, Value>,
    pub clients: HashMap<String, Value>,
    pub protocol_traffic: HashMap<String, Value>,
    pub vms: HashMap<String, Value>,
    pub machines: HashMap<String, Value>,
    pub costs: HashMap<String, Value>,
    pub transactions: HashMap<String, Value>,
    pub packets: HashMap<String, Value>,
    pub messages: HashMap<String, Value>,
    pub creatures: HashMap<String, Value>,
    pub validators: HashMap<String, Value>,
    pub staking: HashMap<String, Value>,
    pub election: HashMap<String, Value>,
    /// Host CPU / memory / disk sampled straight from the machine (see
    /// `telemetry::resources`). Present whether the node runs in Docker or as a
    /// bare process, so `casparctl stats` and the admin panel show resource
    /// usage with no Docker dependency.
    #[serde(default)]
    pub resources: HashMap<String, Value>,
}

/// Telemetry server state.
pub struct TelemetryServer {
    db: Arc<RocksDbKvStore>,
    started_at: SystemTime,
    origin: String,
    chain_port: u16,
    federation_port: u16,
    client_tcp_port: u16,
    client_ws_port: u16,
    entity_port: u16,
    vm_port: u16,
    telemetry_port: u16,
    storage_root: String,
    resources: crate::telemetry::resources::ResourceSampler,
    lock: Mutex<()>,
}

/// Start telemetry from the composition root's validated configuration.
pub fn start(config: &AsemanConfig) -> Result<()> {
    let mut db_path = config.telemetry.database_path.clone();
    if db_path.trim().is_empty() {
        let root = if config.storage.root_path.trim().is_empty() {
            "."
        } else {
            &config.storage.root_path
        };
        db_path = PathBuf::from(root)
            .join("telemetry-rocks")
            .to_string_lossy()
            .into_owned();
    }
    fs::create_dir_all(&db_path).map_err(|e| anyhow!("mkdir telemetry db: {}", e))?;
    let db = Arc::new(
        RocksDbKvStore::open_default(std::path::Path::new(&db_path))
            .map_err(|e| anyhow!("open telemetry db: {}", e))?,
    );

    let server = Arc::new(TelemetryServer {
        db,
        started_at: SystemTime::now(),
        origin: config.node.origin.clone(),
        chain_port: config.network.legacy_consensus_port,
        federation_port: config.network.legacy_federation_port,
        client_tcp_port: config.network.legacy_tcp_port,
        client_ws_port: config.network.legacy_ws_port,
        entity_port: config.telemetry.entity_port,
        vm_port: config.telemetry.vm_port,
        telemetry_port: config.telemetry.api_port,
        storage_root: config.storage.root_path.clone(),
        resources: crate::telemetry::resources::ResourceSampler::new(),
        lock: Mutex::new(()),
    });

    let listener = TcpListener::bind(format!("0.0.0.0:{}", server.telemetry_port))
        .map_err(|e| anyhow!("telemetry bind: {}", e))?;
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let trans = server.clone();
            thread::spawn(move || trans.handle_connection(stream));
        }
    });
    Ok(())
}

impl TelemetryServer {
    fn handle_connection(self: Arc<Self>, mut stream: std::net::TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        // Consume headers.
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
        let path = parts[1];
        let (status, body) = match path {
            "/telemetry/health" => (200, br#"{"status":"ok"}"#.to_vec()),
            "/telemetry/snapshot" => match self.cached_or_collect() {
                Ok(snap) => (200, serde_json::to_vec(&snap).unwrap_or_default()),
                Err(e) => (500, format!("{{\"error\":\"{}\"}}", e).into_bytes()),
            },
            _ => (404, br#"{"error":"not found"}"#.to_vec()),
        };
        let response = format!(
            "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(&body);
    }

    fn cached_or_collect(&self) -> Result<Snapshot> {
        let _guard = self.lock.lock().unwrap();
        // Cache check.
        if let Ok(Some(bytes)) = self.db.get(b"latest_snapshot") {
            if let Ok(cached) = serde_json::from_slice::<Snapshot>(&bytes) {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&cached.timestamp) {
                    if Utc::now().signed_duration_since(ts).num_milliseconds() < 2000 {
                        return Ok(cached);
                    }
                }
            }
        }
        let snap = self.collect();
        if let Ok(bytes) = serde_json::to_vec(&snap) {
            let _ = self.db.write_batch(&[LegacyKvWrite::Put {
                key: b"latest_snapshot".to_vec(),
                value: bytes,
            }]);
        }
        Ok(snap)
    }

    fn collect(&self) -> Snapshot {
        let now = Utc::now().to_rfc3339();
        let uptime = SystemTime::now()
            .duration_since(self.started_at)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64;
        let chain_stats = fetch_json(&format!("http://127.0.0.1:{}/stats", self.chain_port))
            .unwrap_or(Value::Null);
        let peers = fetch_json(&format!("http://127.0.0.1:{}/peers", self.chain_port))
            .unwrap_or(Value::Null);

        macro_rules! map_lit { ($($k:expr => $v:expr),* $(,)?) => {{
            let mut m: HashMap<String, Value> = HashMap::new();
            $(m.insert($k.to_string(), $v);)*
            m
        }};}

        Snapshot {
            timestamp: now,
            uptime_sec: uptime,
            node: map_lit! {
                "origin" => json!(self.origin),
                "telemetry_port" => json!(self.telemetry_port),
                "entity_port" => json!(self.entity_port),
                "vm_port" => json!(self.vm_port),
            },
            chain: map_lit! {
                "port" => json!(self.chain_port),
                "stats" => chain_stats,
                "peers" => peers,
            },
            federation: map_lit! {
                "port" => json!(self.federation_port),
                "status" => json!("running"),
            },
            clients: map_lit! {
                "tcp_port" => json!(self.client_tcp_port),
                "ws_port" => json!(self.client_ws_port),
            },
            protocol_traffic: map_lit! {
                "rx_bytes" => json!(0),
                "tx_bytes" => json!(0),
                "io_details" => json!("attach protocol counters provider"),
            },
            vms: map_lit! {
                "running_count" => json!(0),
                "details" => json!([]),
                "traffic" => json!({"rx": 0, "tx": 0}),
            },
            machines: map_lit! {
                "running_count" => json!(0),
                "details" => json!([]),
            },
            costs: map_lit! {
                "total_execution_cost" => json!(0),
                "recent_task_costs" => json!([]),
            },
            transactions: map_lit! {
                "recent_processed" => json!([]),
                "count" => json!(0),
            },
            packets: map_lit! {
                "recent" => json!([]),
                "count" => json!(0),
            },
            messages: map_lit! {
                "recent_global_chain" => json!([]),
                "count" => json!(0),
            },
            creatures: map_lit! { "onchain_realtime" => json!([]) },
            validators: map_lit! { "details" => json!([]), "count" => json!(0) },
            staking: map_lit! {
                "total_staked" => json!(0),
                "stats" => json!({}),
            },
            election: map_lit! {
                "round" => json!(0),
                "status" => json!("unknown"),
            },
            resources: self.resources.collect(&self.storage_root),
        }
    }
}

fn fetch_json(url: &str) -> Result<Value> {
    // Hand-rolled HTTP client — keeps us off reqwest/hyper. Supports only
    // `http://host:port/path` URLs (no TLS, no redirects), which is all the
    // telemetry collector reaches.
    if url.contains("127.0.0.1:/") || url.trim().is_empty() {
        return Err(anyhow!("invalid url"));
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("non-http url"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let mut stream = std::net::TcpStream::connect_timeout(
        &authority
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("resolve"))?,
        Duration::from_millis(1200),
    )?;
    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, authority
    );
    stream.write_all(req.as_bytes())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let body = match split {
        Some(i) => &buf[i + 4..],
        None => &buf[..],
    };
    serde_json::from_slice::<Value>(body).map_err(|e| anyhow!("decode: {}", e))
}

use std::net::ToSocketAddrs;
