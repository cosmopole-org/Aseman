//! `asemanctl cluster` — orchestrate the RocksDB storage cluster (OpenRaft)
//! mesh (OpenRaft-replicated cluster of same-origin nodes).
//!
//! The commands talk to the node's cluster HTTP listener (default
//! `127.0.0.1:7440`, override with `--endpoint` or `ASEMAN_CTL_CLUSTER_ENDPOINT`;
//! set `--token` / `ASEMAN_CLUSTER_TOKEN` when the mesh uses a shared secret).
//!
//! Peers can be introduced one by one (`add-peer --id 2 --addr host:7440`)
//! or all at once from a cluster config document (`apply -f cluster.json`);
//! every configuration knob is also individually readable/settable through
//! `config get/set`.
//!
//! HTTP calls shell out to `curl`, matching the rest of the CLI.

use aseman_config::CliConfig;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

pub fn run_cluster(config: &CliConfig, args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_cluster_usage();
        return Ok(());
    }
    let a = parse_args(config, &args[1..]);
    match args[0].as_str() {
        "status" => cmd_status(&a),
        "init" => cmd_init(&a),
        "peers" => cmd_peers(&a),
        "nearest" => cmd_nearest(&a),
        "add-peer" => cmd_add_peer(&a),
        "remove-peer" => cmd_remove_peer(&a),
        "promote" => cmd_promote(&a),
        "config" => cmd_config(&a),
        "apply" => cmd_apply(&a),
        "help" | "-h" | "--help" => {
            print_cluster_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown cluster subcommand \"{}\"\n", other);
            print_cluster_usage();
            std::process::exit(1);
        }
    }
}

fn print_cluster_usage() {
    println!(
        "asemanctl cluster - orchestrate the RocksDB storage cluster (OpenRaft)\n\n\
         Usage:\n  asemanctl cluster <subcommand> [flags]\n\n\
         Subcommands:\n  \
         status                       Raft state, leader, membership, peer RTT table\n  \
         init [--include-peers]      Initialize a pristine cluster on this node\n  \
         peers                        List the peers registered in the cluster config\n  \
         nearest                      Peers sorted by measured round-trip latency\n  \
         add-peer --id N --addr H:P   Introduce another instance (one by one)\n  \
             [--region R] [--lat F] [--lon F] [--learner]\n  \
         remove-peer --id N           Remove an instance from the mesh\n  \
         promote --ids 1,2,3          Set the exact raft voter membership\n  \
         config list                  Print the full cluster configuration\n  \
         config get <key>             Read one dotted config key (e.g. peers.2.addr)\n  \
         config set <key> <value>     Set one dotted config key\n  \
         apply -f cluster.json        Apply a whole cluster config document\n\n\
         Global flags:\n  \
         --endpoint URL   Cluster listener (default https://127.0.0.1:7440,\n                   \
         env ASEMAN_CTL_CLUSTER_ENDPOINT)\n  \
         --token SECRET   Shared cluster secret (env ASEMAN_CLUSTER_TOKEN)\n\n\
         Every call uses the cluster's mutual TLS: ASEMAN_CLUSTER_TLS_CERTIFICATE,\n\
         ASEMAN_CLUSTER_TLS_KEY_SECRET, and ASEMAN_CLUSTER_TLS_CA name the operator's\n\
         identity and the cluster CA."
    );
}

// ───────────────────────── flag helpers ────────────────────────────────────

struct ClusterArgs {
    config: CliConfig,
    endpoint: String,
    token: String,
    values: std::collections::HashMap<String, String>,
    positional: Vec<String>,
}

fn parse_args(config: &CliConfig, args: &[String]) -> ClusterArgs {
    let mut endpoint = config.cluster_endpoint.clone();
    let mut token = config.cluster_token.clone();
    let mut values = std::collections::HashMap::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let flag_body = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-').filter(|s| !s.is_empty()));
        if let Some(stripped) = flag_body {
            let (key, inline_val) = match stripped.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (stripped.to_string(), None),
            };
            let boolean_flags = ["learner", "include-peers"];
            let value = match inline_val {
                Some(v) => v,
                None if boolean_flags.contains(&key.as_str()) => "true".to_string(),
                None => {
                    i += 1;
                    args.get(i).cloned().unwrap_or_default()
                }
            };
            match key.as_str() {
                "endpoint" => endpoint = value,
                "token" => token = value,
                _ => {
                    values.insert(key, value);
                }
            }
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    ClusterArgs {
        config: config.clone(),
        endpoint: endpoint.trim_end_matches('/').to_string(),
        token,
        values,
        positional,
    }
}

// ───────────────────────── HTTP via curl ───────────────────────────────────

fn curl(a: &ClusterArgs, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
    super::admin::call(
        &a.config,
        &a.token,
        method,
        &format!("{}{}", a.endpoint, path),
        body,
    )
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

// ───────────────────────── subcommands ─────────────────────────────────────

fn cmd_status(a: &ClusterArgs) -> Result<()> {
    print_json(&curl(a, "GET", "/cluster/status", None)?);
    Ok(())
}

fn cmd_init(a: &ClusterArgs) -> Result<()> {
    let include_peers = a
        .values
        .get("include-peers")
        .map(|v| v == "true")
        .unwrap_or(false);
    let body = json!({"includePeers": include_peers});
    print_json(&curl(a, "POST", "/cluster/init", Some(&body))?);
    Ok(())
}

fn cmd_peers(a: &ClusterArgs) -> Result<()> {
    print_json(&curl(a, "GET", "/cluster/peers", None)?);
    Ok(())
}

fn cmd_nearest(a: &ClusterArgs) -> Result<()> {
    print_json(&curl(a, "GET", "/cluster/nearest", None)?);
    Ok(())
}

fn cmd_add_peer(a: &ClusterArgs) -> Result<()> {
    let id: u64 = a
        .values
        .get("id")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow!("--id <number> is required"))?;
    let addr = a
        .values
        .get("addr")
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("--addr <host:port> is required (domain or IP)"))?;
    let mut body = json!({
        "id": id,
        "addr": addr,
        "voter": a.values.get("learner").map(|v| v != "true").unwrap_or(true),
    });
    if let Some(region) = a.values.get("region") {
        body["region"] = json!(region);
    }
    if let Some(lat) = a.values.get("lat").and_then(|v| v.parse::<f64>().ok()) {
        body["latitude"] = json!(lat);
    }
    if let Some(lon) = a.values.get("lon").and_then(|v| v.parse::<f64>().ok()) {
        body["longitude"] = json!(lon);
    }
    print_json(&curl(a, "POST", "/cluster/add-peer", Some(&body))?);
    Ok(())
}

fn cmd_remove_peer(a: &ClusterArgs) -> Result<()> {
    let id: u64 = a
        .values
        .get("id")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow!("--id <number> is required"))?;
    let body = json!({"id": id});
    print_json(&curl(a, "POST", "/cluster/remove-peer", Some(&body))?);
    Ok(())
}

fn cmd_promote(a: &ClusterArgs) -> Result<()> {
    let ids: Vec<u64> = a
        .values
        .get("ids")
        .map(|raw| {
            raw.split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        bail!("--ids 1,2,3 is required");
    }
    let body = json!({"ids": ids});
    print_json(&curl(a, "POST", "/cluster/promote", Some(&body))?);
    Ok(())
}

fn cmd_config(a: &ClusterArgs) -> Result<()> {
    match a.positional.first().map(|s| s.as_str()) {
        Some("list") | None => {
            print_json(&curl(a, "GET", "/cluster/config", None)?);
            Ok(())
        }
        Some("get") => {
            let key = a
                .positional
                .get(1)
                .ok_or_else(|| anyhow!("usage: asemanctl cluster config get <key>"))?;
            let cfg = curl(a, "GET", "/cluster/config", None)?;
            let pointer = format!("/{}", key.replace('.', "/"));
            match cfg.pointer(&pointer) {
                Some(v) => {
                    print_json(v);
                    Ok(())
                }
                None => bail!("config key not found: {}", key),
            }
        }
        Some("set") => {
            let key = a
                .positional
                .get(1)
                .ok_or_else(|| anyhow!("usage: asemanctl cluster config set <key> <value>"))?;
            let value = a
                .positional
                .get(2)
                .ok_or_else(|| anyhow!("usage: asemanctl cluster config set <key> <value>"))?;
            let body = json!({"key": key, "value": value});
            print_json(&curl(a, "POST", "/cluster/config", Some(&body))?);
            Ok(())
        }
        Some(other) => bail!("unknown config subcommand \"{}\" (list|get|set)", other),
    }
}

fn cmd_apply(a: &ClusterArgs) -> Result<()> {
    let file = a
        .values
        .get("f")
        .or_else(|| a.values.get("file"))
        .or_else(|| a.positional.first())
        .ok_or_else(|| anyhow!("usage: asemanctl cluster apply -f <cluster.json>"))?;
    let raw = std::fs::read_to_string(file).map_err(|e| anyhow!("read {}: {}", file, e))?;
    let doc: Value =
        serde_json::from_str(&raw).map_err(|e| anyhow!("{} is not valid JSON: {}", file, e))?;
    print_json(&curl(a, "POST", "/cluster/config/apply", Some(&doc))?);
    Ok(())
}
