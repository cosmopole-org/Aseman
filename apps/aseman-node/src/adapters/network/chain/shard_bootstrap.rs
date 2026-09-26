//! Babble shard-directory bootstrap owned by the node composition layer.
//!
//! This replaces the historical `shardchain.sh` process boundary. Keeping the
//! operation here makes failures typed and testable and removes a runtime
//! dependency on Bash and curl.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerMode {
    NewShard,
    Head,
    Follower,
}

pub struct Bootstrap<'a> {
    pub storage_root: &'a Path,
    pub babble_data_dir: &'a Path,
    pub workchain_id: &'a str,
    pub shardchain_id: &'a str,
    pub peer_mode: PeerMode,
    pub root_node: Option<&'a str>,
}

impl Bootstrap<'_> {
    pub fn destination(&self) -> PathBuf {
        self.storage_root
            .join("chains")
            .join(self.workchain_id)
            .join(self.shardchain_id)
    }
}

pub fn bootstrap(config: &Bootstrap<'_>) -> Result<()> {
    let destination = config.destination();
    fs::create_dir_all(&destination)
        .with_context(|| format!("create shard directory {}", destination.display()))?;

    copy_required(config.babble_data_dir, &destination, "key.pub")?;
    copy_required(config.babble_data_dir, &destination, "priv_key")?;

    let local_genesis = config.babble_data_dir.join("peers.genesis.json");
    match config.peer_mode {
        PeerMode::NewShard => {
            copy_file(&local_genesis, &destination.join("peers.genesis.json"))?;
        }
        PeerMode::Head => copy_local_peers(&local_genesis, &destination)?,
        PeerMode::Follower if local_genesis.is_file() => {
            copy_local_peers(&local_genesis, &destination)?;
        }
        PeerMode::Follower => fetch_remote_peers(config, &destination)?,
    }
    Ok(())
}

fn copy_required(source_dir: &Path, destination: &Path, name: &str) -> Result<()> {
    let source = source_dir.join(name);
    if !source.is_file() {
        bail!("required Babble file is missing: {}", source.display());
    }
    copy_file(&source, &destination.join(name))
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination)
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))?;
    Ok(())
}

fn copy_local_peers(local_genesis: &Path, destination: &Path) -> Result<()> {
    if !local_genesis.is_file() {
        bail!(
            "required Babble peer genesis is missing: {}",
            local_genesis.display()
        );
    }
    copy_file(local_genesis, &destination.join("peers.genesis.json"))?;
    copy_file(local_genesis, &destination.join("peers.json"))
}

fn fetch_remote_peers(config: &Bootstrap<'_>, destination: &Path) -> Result<()> {
    let root = config
        .root_node
        .ok_or_else(|| anyhow!("no local peers.genesis.json and no root node was configured"))?;
    let base_url = chain_api_url(root)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build consensus peer HTTP client")?;

    let headers = |request: reqwest::blocking::RequestBuilder| {
        request
            .header("Work-Chain-Id", config.workchain_id)
            .header("Shard-Chain-Id", config.shardchain_id)
    };
    let genesis_url = format!("{base_url}/genesispeers");
    let mut last_error = None;
    let mut genesis = None;
    for attempt in 1..=10 {
        match headers(client.get(&genesis_url))
            .send()
            .and_then(|r| r.error_for_status())
        {
            Ok(response) => match response.bytes() {
                Ok(bytes) if !bytes.is_empty() => {
                    genesis = Some(bytes.to_vec());
                    break;
                }
                Ok(_) => last_error = Some("root node returned an empty peer genesis".to_owned()),
                Err(error) => last_error = Some(error.to_string()),
            },
            Err(error) => last_error = Some(error.to_string()),
        }
        if attempt < 10 {
            thread::sleep(Duration::from_secs(2));
        }
    }
    let genesis = genesis.ok_or_else(|| {
        anyhow!(
            "could not fetch {genesis_url}: {}",
            last_error.unwrap_or_else(|| "unknown error".to_owned())
        )
    })?;
    fs::write(destination.join("peers.genesis.json"), &genesis)
        .context("write fetched peer genesis")?;

    let peers_url = format!("{base_url}/peers");
    let peers = headers(client.get(&peers_url))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| bytes.to_vec())
        .unwrap_or(genesis);
    fs::write(destination.join("peers.json"), peers).context("write current peer set")?;
    Ok(())
}

fn chain_api_url(root_node: &str) -> Result<String> {
    let value = root_node.trim().trim_end_matches('/');
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(value.to_owned());
    }
    let (host, tcp_port) = value
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("root node must be host:port or an HTTP URL: {value}"))?;
    let port: u16 = tcp_port
        .parse()
        .with_context(|| format!("invalid root-node TCP port in {value}"))?;
    let chain_port = port
        .checked_add(4)
        .ok_or_else(|| anyhow!("root-node TCP port is too high to derive chain API: {port}"))?;
    Ok(format!("http://{host}:{chain_port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aseman-shard-bootstrap-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn fixture(root: &Path) -> PathBuf {
        let source = root.join("babble");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("key.pub"), b"public").unwrap();
        fs::write(source.join("priv_key"), b"private").unwrap();
        fs::write(source.join("peers.genesis.json"), b"[genesis]").unwrap();
        source
    }

    #[test]
    fn head_copies_keys_and_both_peer_files() {
        let root = temp_dir("head");
        let source = fixture(&root);
        let config = Bootstrap {
            storage_root: &root.join("storage"),
            babble_data_dir: &source,
            workchain_id: "main",
            shardchain_id: "shard-main",
            peer_mode: PeerMode::Head,
            root_node: None,
        };
        bootstrap(&config).unwrap();
        let destination = config.destination();
        assert_eq!(fs::read(destination.join("key.pub")).unwrap(), b"public");
        assert_eq!(
            fs::read(destination.join("peers.genesis.json")).unwrap(),
            b"[genesis]"
        );
        assert_eq!(
            fs::read(destination.join("peers.json")).unwrap(),
            b"[genesis]"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn new_shard_preserves_precomputed_current_peers() {
        let root = temp_dir("new");
        let source = fixture(&root);
        let storage = root.join("storage");
        let config = Bootstrap {
            storage_root: &storage,
            babble_data_dir: &source,
            workchain_id: "work",
            shardchain_id: "shard-a",
            peer_mode: PeerMode::NewShard,
            root_node: None,
        };
        fs::create_dir_all(config.destination()).unwrap();
        fs::write(config.destination().join("peers.json"), b"[filtered]").unwrap();
        bootstrap(&config).unwrap();
        assert_eq!(
            fs::read(config.destination().join("peers.json")).unwrap(),
            b"[filtered]"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn derives_chain_api_from_legacy_tcp_endpoint() {
        assert_eq!(
            chain_api_url("node.example:8074").unwrap(),
            "http://node.example:8078"
        );
        assert_eq!(
            chain_api_url("https://node.example:9443/").unwrap(),
            "https://node.example:9443"
        );
    }

    #[test]
    fn follower_fetches_genesis_and_falls_back_when_current_peers_are_unavailable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for expected_path in ["/genesispeers", "/peers"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let size = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                assert!(request.starts_with(&format!("GET {expected_path} ")));
                assert!(request.to_ascii_lowercase().contains("work-chain-id: work"));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("shard-chain-id: shard-a")
                );
                if expected_path == "/genesispeers" {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n[remote]")
                        .unwrap();
                } else {
                    stream
                        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
                        .unwrap();
                }
            }
        });

        let root = temp_dir("follower");
        let source = root.join("babble");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("key.pub"), b"public").unwrap();
        fs::write(source.join("priv_key"), b"private").unwrap();
        let config = Bootstrap {
            storage_root: &root.join("storage"),
            babble_data_dir: &source,
            workchain_id: "work",
            shardchain_id: "shard-a",
            peer_mode: PeerMode::Follower,
            root_node: Some(&endpoint),
        };
        bootstrap(&config).unwrap();
        assert_eq!(
            fs::read(config.destination().join("peers.genesis.json")).unwrap(),
            b"[remote]"
        );
        assert_eq!(
            fs::read(config.destination().join("peers.json")).unwrap(),
            b"[remote]"
        );
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
