//! The authenticated administration HTTP shared by the RocksDB storage cluster's
//! listener and the node's module administration.
//!
//! Every connection is mutual TLS: the listener presents this party's certificate and
//! accepts only clients whose certificate chains to the configured CA, and the HTTPS
//! client [`MutualTls`] builds does the same in the other direction. On top of that,
//! a request may carry a bearer token (`x-aseman-cluster-token` or `Authorization:
//! Bearer`), compared in constant time. The HTTP is deliberately minimal: one
//! request per connection, a bounded body, and a read deadline.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use aseman_config::TlsFiles;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

/// The largest request body a listener reads.
pub const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
/// How long a connection may take to complete its handshake and send its request.
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// Routes a composing process serves: `(method, path, body)` to `(status, body)`,
/// or `None` for a path it does not serve.
pub type Routes = Arc<dyn Fn(&str, &str, &[u8]) -> Option<(u16, Vec<u8>)> + Send + Sync>;

/// A mutual-TLS identity, loaded from its [`TlsFiles`].
#[derive(Clone)]
pub struct MutualTls {
    server: Arc<ServerConfig>,
    identity_pem: Vec<u8>,
    ca_pem: Vec<u8>,
}

impl std::fmt::Debug for MutualTls {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MutualTls")
    }
}

fn read(path: &std::path::Path, what: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("read {what} {}: {error}", path.display()))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

impl MutualTls {
    /// # Errors
    ///
    /// An unreadable or invalid certificate, key, or CA.
    pub fn load(files: &TlsFiles) -> Result<Self, String> {
        let certificate_pem = read(&files.certificate, "certificate")?;
        let key_pem = read(&files.key_secret, "private key")?;
        let ca_pem = read(&files.ca, "CA")?;

        let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &certificate_pem[..])
            .collect::<Result<_, _>>()
            .map_err(|error| format!("parse certificate: {error}"))?;
        if chain.is_empty() {
            return Err(format!(
                "{} holds no certificate",
                files.certificate.display()
            ));
        }
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..])
            .map_err(|error| format!("parse private key: {error}"))?
            .ok_or_else(|| format!("{} holds no private key", files.key_secret.display()))?;
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut &ca_pem[..]) {
            roots
                .add(certificate.map_err(|error| format!("parse CA: {error}"))?)
                .map_err(|error| format!("trust CA: {error}"))?;
        }
        if roots.is_empty() {
            return Err(format!("{} holds no CA certificate", files.ca.display()));
        }
        let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
            .build()
            .map_err(|error| format!("client verifier: {error}"))?;
        let server = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(|error| format!("server identity: {error}"))?;
        let mut identity_pem = certificate_pem;
        identity_pem.push(b'\n');
        identity_pem.extend_from_slice(&key_pem);
        Ok(Self {
            server: Arc::new(server),
            identity_pem,
            ca_pem,
        })
    }

    fn client_builder(&self, timeout: Duration) -> Result<reqwest::ClientBuilder, String> {
        let identity = reqwest::Identity::from_pem(&self.identity_pem)
            .map_err(|error| format!("client identity: {error}"))?;
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .identity(identity)
            .timeout(timeout);
        for certificate in reqwest::Certificate::from_pem_bundle(&self.ca_pem)
            .map_err(|error| format!("client CA: {error}"))?
        {
            builder = builder.add_root_certificate(certificate);
        }
        Ok(builder)
    }

    /// An HTTPS client presenting this identity and trusting only the CA.
    ///
    /// # Errors
    ///
    /// The identity cannot be used by the client.
    pub fn http_client(&self, timeout: Duration) -> Result<reqwest::Client, String> {
        self.client_builder(timeout)?
            .build()
            .map_err(|error| format!("https client: {error}"))
    }

    /// A blocking HTTPS client presenting this identity and trusting only the CA.
    ///
    /// # Errors
    ///
    /// The identity cannot be used by the client.
    pub fn blocking_http_client(
        &self,
        timeout: Duration,
    ) -> Result<reqwest::blocking::Client, String> {
        let identity = reqwest::Identity::from_pem(&self.identity_pem)
            .map_err(|error| format!("client identity: {error}"))?;
        let mut builder = reqwest::blocking::Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .identity(identity)
            .timeout(timeout);
        for certificate in reqwest::Certificate::from_pem_bundle(&self.ca_pem)
            .map_err(|error| format!("client CA: {error}"))?
        {
            builder = builder.add_root_certificate(certificate);
        }
        builder
            .build()
            .map_err(|error| format!("https client: {error}"))
    }
}

/// One request.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// The presented bearer token, empty when none.
    pub token: String,
    pub body: Vec<u8>,
}

/// Whether `presented` is `expected`, in time independent of where they differ.
#[must_use]
pub fn token_matches(presented: &str, expected: &str) -> bool {
    let (presented, expected) = (presented.as_bytes(), expected.as_bytes());
    let mut difference = u8::from(presented.len() != expected.len());
    for (index, byte) in expected.iter().enumerate() {
        difference |= byte ^ presented.get(index).copied().unwrap_or(!byte);
    }
    difference == 0
}

fn read_request(stream: &mut impl Read) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut length = 0_usize;
    let mut token = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        let value = line.split_once(':').map_or("", |(_, value)| value).trim();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            length = rest.trim().parse().ok()?;
        } else if lower.starts_with("x-aseman-cluster-token:") {
            value.clone_into(&mut token);
        } else if lower.starts_with("authorization:")
            && let Some(bearer) = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        {
            bearer.clone_into(&mut token);
        }
    }
    if length > MAX_BODY_BYTES {
        return None;
    }
    let mut body = vec![0_u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Request {
        method,
        path,
        token,
        body,
    })
}

fn respond(stream: &mut impl Write, status: u16, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status < 400 { "OK" } else { "ERR" },
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// Serve mutual-TLS connections on `listener`, one thread per connection, answering
/// each request with `handle`.
///
/// # Errors
///
/// The accept thread cannot be spawned.
pub fn serve(
    listener: TcpListener,
    tls: &MutualTls,
    name: &str,
    handle: impl Fn(&Request) -> (u16, Vec<u8>) + Send + Sync + 'static,
) -> Result<(), String> {
    let server = tls.server.clone();
    let handle = Arc::new(handle);
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (server, handle) = (server.clone(), handle.clone());
                thread::spawn(move || connection(stream, &server, handle.as_ref()));
            }
        })
        .map(|_| ())
        .map_err(|error| format!("{name} spawn: {error}"))
}

fn connection(
    stream: TcpStream,
    server: &Arc<ServerConfig>,
    handle: &(impl Fn(&Request) -> (u16, Vec<u8>) + ?Sized),
) {
    if stream.set_read_timeout(Some(READ_DEADLINE)).is_err() {
        return;
    }
    let Ok(session) = ServerConnection::new(server.clone()) else {
        return;
    };
    let mut stream = StreamOwned::new(session, stream);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let (status, body) = handle(&request);
    respond(&mut stream, status, &body);
    stream.conn.send_close_notify();
    let _ = stream.flush();
}

/// Serve `routes` alone on `listen`: every request must carry `token`.
///
/// # Errors
///
/// An empty token, or the address cannot be bound.
pub fn serve_routes(
    listen: &str,
    tls: &MutualTls,
    token: &str,
    name: &str,
    routes: Routes,
) -> Result<(), String> {
    if token.is_empty() {
        return Err("an administration listener requires an auth token".to_owned());
    }
    let listener =
        TcpListener::bind(listen).map_err(|error| format!("{name} bind {listen}: {error}"))?;
    let token = token.to_owned();
    serve(listener, tls, name, move |request| {
        if !token_matches(&request.token, &token) {
            return (401, br#"{"error":"invalid administration token"}"#.to_vec());
        }
        routes(&request.method, &request.path, &request.body)
            .unwrap_or_else(|| (404, br#"{"error":"not found"}"#.to_vec()))
    })
}

#[cfg(feature = "testing")]
pub mod testing;

#[cfg(test)]
mod tests;
