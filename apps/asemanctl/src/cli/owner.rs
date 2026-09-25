//! Node-owner bootstrap.
//!
//! A freshly installed node gets a placeholder owner: `install --local` mints a
//! standalone RSA key and writes `OWNER_ID=owner-node1` into the data-dir
//! `.env`. That identity is not a real Caspar **creature**, so it cannot be used
//! as an account (e.g. to mint tokens for other users).
//!
//! This module turns the node owner into a real account. On the first `run`
//! after an install we:
//!
//!   1. let the node come up (it must be serving before anyone can register),
//!   2. create the **first** creature over the node's plaintext TCP transport
//!      (`/creatures/login`, which registers on first use) — being first, it
//!      takes creature id `1@<node>`,
//!   3. persist that creature's id + private key into the data dir
//!      (`node-owner.json`, 0600) and rewrite `OWNER_ID` / `OWNER_PRIVATE_KEY`
//!      in `.env`,
//!   4. restart the node so it boots as that creature.
//!
//! Every later start needs no extra work: `launch_node` already exports the
//! `.env` pairs into the node process, so the owner id + key are set
//! automatically from then on.
//!
//! ## Wire protocol
//!
//! Request frame  `[i32BE body_len][body]`, where
//! `body = [i32BE sig_len][sig][i32BE uid_len][uid][i32BE path_len][path][i32BE pid_len][pid][payload]`.
//! Registration is anonymous, so `sig` and `uid` are empty.
//!
//! Response frame `[i32BE len][packet]` with
//! `packet = [0x02][i32BE pid_len][pid][i32BE res_code][json]` for a reply, or
//! `[0x01][i32BE key_len][key][json]` for a server push (skipped here).

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

/// Username of the creature created as the node owner. Override with
/// `CASPAR_OWNER_USERNAME` before the first `casparctl run`.
pub const DEFAULT_OWNER_USERNAME: &str = "node_owner";

/// Placeholder written by `install --local` before a real owner exists.
const PLACEHOLDER_OWNER_ID: &str = "owner-node1";

fn i32be(n: usize) -> [u8; 4] {
    (n as i32).to_be_bytes()
}

fn read_exact_timeout(s: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    s.read_exact(buf).context("reading from the caspar node")
}

/// Build one request frame for `path` with a JSON `payload` (anonymous: no
/// signature, no user id).
fn build_frame(path: &str, packet_id: &str, payload: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&i32be(0)); // signature length (anonymous)
    body.extend_from_slice(&i32be(0)); // user id length (anonymous)
    body.extend_from_slice(&i32be(path.len()));
    body.extend_from_slice(path.as_bytes());
    body.extend_from_slice(&i32be(packet_id.len()));
    body.extend_from_slice(packet_id.as_bytes());
    body.extend_from_slice(payload.as_bytes());

    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&i32be(body.len()));
    frame.extend_from_slice(&body);
    frame
}

/// Read frames until we see the response carrying `packet_id`; returns
/// `(res_code, json_body)`. Server pushes (`0x01`) are skipped.
fn read_response(stream: &mut TcpStream, packet_id: &str) -> Result<(i32, String)> {
    for _ in 0..64 {
        let mut len_buf = [0u8; 4];
        read_exact_timeout(stream, &mut len_buf)?;
        let len = i32::from_be_bytes(len_buf);
        if len <= 0 || len > 32 * 1024 * 1024 {
            bail!("caspar node sent an implausible frame length ({len})");
        }
        let mut packet = vec![0u8; len as usize];
        read_exact_timeout(stream, &mut packet)?;

        // The node expects a keepalive ack after each frame.
        let _ = stream.write_all(&[0x00, 0x00, 0x00, 0x01, 0x01]);

        let mut p = 0usize;
        let kind = *packet.first().unwrap_or(&0);
        p += 1;
        if kind == 0x02 {
            if packet.len() < p + 4 {
                continue;
            }
            let pid_len = i32::from_be_bytes(packet[p..p + 4].try_into()?) as usize;
            p += 4;
            if packet.len() < p + pid_len + 4 {
                continue;
            }
            let pid = String::from_utf8_lossy(&packet[p..p + pid_len]).to_string();
            p += pid_len;
            let res_code = i32::from_be_bytes(packet[p..p + 4].try_into()?);
            p += 4;
            let json = String::from_utf8_lossy(&packet[p..]).to_string();
            if pid == packet_id {
                return Ok((res_code, json));
            }
        }
        // kind 0x01 (push) or an unrelated reply — keep reading.
    }
    bail!("no response from the caspar node for packet {packet_id}")
}

/// Register/lookup a creature over the node's plaintext TCP transport and
/// return `(creature_id, private_key_pem)`.
///
/// `/creatures/login` registers the account on first use, so calling it on a
/// fresh node creates the very first creature (id `1@<node>`).
pub fn login_creature(
    host: &str,
    port: u16,
    username: &str,
    email: &str,
) -> Result<(String, String)> {
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow!("could not resolve {host}:{port}"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(20))
        .with_context(|| format!("connecting to the caspar node at {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    stream.set_nodelay(true).ok();

    let payload = serde_json::json!({
        "username": username,
        "emailToken": email,
        "metadata": { "public": { "profile": { "name": username } } },
    })
    .to_string();
    // Mirrors the JS client's packet id (a random decimal string).
    let packet_id = format!("{}", std::process::id() as u64 * 1_000_003 % 1_000_000_007);

    let frame = build_frame("/creatures/login", &packet_id, &payload);
    stream
        .write_all(&frame)
        .context("sending /creatures/login to the caspar node")?;
    stream.flush().ok();

    let (res_code, json) = read_response(&mut stream, &packet_id)?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    if res_code != 0 {
        bail!("/creatures/login failed (resCode {res_code}): {json}");
    }

    let v: serde_json::Value =
        serde_json::from_str(&json).context("parsing the /creatures/login response")?;
    let id = v
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(|i| i.as_str())
        .ok_or_else(|| anyhow!("login response has no user.id: {json}"))?
        .to_string();
    let key = v
        .get("privateKey")
        .and_then(|k| k.as_str())
        .ok_or_else(|| anyhow!("login response has no privateKey"))?
        .to_string();
    Ok((id, key))
}

/// True when the data dir still carries the placeholder owner (no real creature
/// has been bootstrapped yet).
pub fn needs_bootstrap(dir: &Path) -> bool {
    if dir.join("node-owner.json").exists() {
        return false;
    }
    match fs::read_to_string(dir.join(".env")) {
        Ok(env) => env.lines().any(|l| {
            let l = l.trim();
            l.starts_with("OWNER_ID=")
                && l.trim_start_matches("OWNER_ID=").trim() == PLACEHOLDER_OWNER_ID
        }),
        Err(_) => false,
    }
}

/// Replace `OWNER_ID` / `OWNER_PRIVATE_KEY` in the data-dir `.env` with the
/// bootstrapped creature, preserving every other line.
///
/// `OWNER_PRIVATE_KEY` is a multi-line PEM written as one double-quoted value —
/// the same shape `write_env` produces and `env_lines` parses.
fn rewrite_env_owner(dir: &Path, owner_id: &str, owner_key: &str) -> Result<()> {
    let path = dir.join(".env");
    let raw = fs::read_to_string(&path).context("reading .env to set the node owner")?;

    let mut out = String::with_capacity(raw.len() + owner_key.len());
    let mut in_quoted = false;
    for line in raw.lines() {
        if in_quoted {
            // Inside a multi-line double-quoted value (the old PEM) — drop it.
            if line.trim_end().ends_with('"') {
                in_quoted = false;
            }
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("OWNER_ID=") {
            continue; // replaced below
        }
        if t.starts_with("OWNER_PRIVATE_KEY=") {
            // Skip the whole value, which may span lines when quoted.
            let after = t.trim_start_matches("OWNER_PRIVATE_KEY=");
            let starts_quote = after.starts_with('"');
            let ends_quote = after.len() > 1 && after.trim_end().ends_with('"');
            if starts_quote && !ends_quote {
                in_quoted = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    let owner_block = format!(
        "OWNER_ID={owner_id}\nOWNER_PRIVATE_KEY=\"{}\"\n",
        owner_key.trim()
    );
    let env = format!("{owner_block}{out}");
    fs::write(&path, env).context("writing .env with the bootstrapped node owner")?;
    Ok(())
}

/// Persist the owner record next to the node config (0600 — it holds a private
/// key) and point `.env` at it.
pub fn save_owner(dir: &Path, username: &str, owner_id: &str, owner_key: &str) -> Result<()> {
    let record = serde_json::json!({
        "username": username,
        "userId": owner_id,
        "privateKey": owner_key,
        "note": "Node owner creature. OWNER_ID/OWNER_PRIVATE_KEY in .env are kept \
                 in sync with this record; casparctl exports them to caspar-node \
                 on every start.",
    });
    let path = dir.join("node-owner.json");
    fs::write(&path, serde_json::to_string_pretty(&record)?).context("writing node-owner.json")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    rewrite_env_owner(dir, owner_id, owner_key)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout_is_length_prefixed() {
        let f = build_frame("/creatures/login", "42", "{}");
        // total length prefix + body
        let body_len = i32::from_be_bytes(f[0..4].try_into().unwrap()) as usize;
        assert_eq!(body_len, f.len() - 4);
        // anonymous: first two length fields are zero
        assert_eq!(i32::from_be_bytes(f[4..8].try_into().unwrap()), 0);
        assert_eq!(i32::from_be_bytes(f[8..12].try_into().unwrap()), 0);
        // then path
        let path_len = i32::from_be_bytes(f[12..16].try_into().unwrap()) as usize;
        assert_eq!(path_len, "/creatures/login".len());
        assert_eq!(&f[16..16 + path_len], b"/creatures/login");
    }

    #[test]
    fn rewrite_env_replaces_single_line_owner() {
        let d = std::env::temp_dir().join(format!("ctl-owner-{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        fs::write(d.join(".env"), "OWNER_ID=owner-node1\nFOO=bar\nBAZ=1\n").unwrap();
        rewrite_env_owner(
            &d,
            "1@abc",
            "-----BEGIN PRIVATE KEY-----\nkey\n-----END PRIVATE KEY-----",
        )
        .unwrap();
        let out = fs::read_to_string(d.join(".env")).unwrap();
        assert!(out.contains("OWNER_ID=1@abc"));
        assert!(out.contains("FOO=bar") && out.contains("BAZ=1"));
        assert_eq!(out.matches("OWNER_ID=").count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rewrite_env_replaces_multiline_pem_owner() {
        let d = std::env::temp_dir().join(format!("ctl-owner-pem-{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        fs::write(
            d.join(".env"),
            "OWNER_ID=owner-node1\nOWNER_PRIVATE_KEY=\"-----BEGIN PRIVATE KEY-----\nold\nline\n-----END PRIVATE KEY-----\"\nKEEP=yes\n",
        )
        .unwrap();
        rewrite_env_owner(
            &d,
            "1@node",
            "-----BEGIN PRIVATE KEY-----\nnew\n-----END PRIVATE KEY-----",
        )
        .unwrap();
        let out = fs::read_to_string(d.join(".env")).unwrap();
        assert!(out.contains("OWNER_ID=1@node"), "{out}");
        assert!(out.contains("KEEP=yes"), "{out}");
        assert!(!out.contains("old"), "old PEM leaked: {out}");
        assert_eq!(out.matches("OWNER_PRIVATE_KEY=").count(), 1, "{out}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn needs_bootstrap_detects_placeholder_and_marker() {
        let d = std::env::temp_dir().join(format!("ctl-boot-{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        fs::write(d.join(".env"), "OWNER_ID=owner-node1\n").unwrap();
        assert!(needs_bootstrap(&d));
        fs::write(d.join(".env"), "OWNER_ID=1@real\n").unwrap();
        assert!(!needs_bootstrap(&d));
        fs::write(d.join(".env"), "OWNER_ID=owner-node1\n").unwrap();
        fs::write(d.join("node-owner.json"), "{}").unwrap();
        assert!(!needs_bootstrap(&d), "marker file must win");
        let _ = fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod wire_compat {
    use super::*;
    /// The request frame must be byte-identical to what the Nest JS client
    /// (caspar-client.ts `createRequest`) emits for the same anonymous request.
    /// Uses a fixed payload string so this asserts the FRAMING, independent of
    /// JSON key ordering (serde sorts keys; JSON.stringify preserves insertion
    /// order — both are equivalent JSON to the node).
    #[test]
    fn frame_matches_js_client_bytes() {
        let payload = r#"{"username":"node_owner","emailToken":"a@b.c"}"#;
        let f = build_frame("/creatures/login", "42", payload);
        let hex: String = f.iter().map(|b| format!("{b:02x}")).collect();
        // Produced by the JS client's createRequest() for the same inputs.
        let expected = "000000500000000000000000000000102f6372656174757265732f6c6f67696e0000000234327b22757365726e616d65223a226e6f64655f6f776e6572222c22656d61696c546f6b656e223a226140622e63227d";
        assert_eq!(hex, expected, "rust frame != js client frame");
    }
}
