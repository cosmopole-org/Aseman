//! Public file storage over HTTP — the "Caspar storage shell API".
//!
//! A tiny, dependency-light HTTP server (same hand-rolled style as
//! `telemetry::server`) that lets the platform store and serve **public**
//! binary blobs (avatars, images) without pushing them through the signed
//! action/consensus path. It is deliberately internal: only the Nest backend
//! is expected to reach it (it proxies uploads for authenticated users and
//! re-serves downloads to clients), so the node port stays off the public
//! internet just like the docker gateway / telemetry ports.
//!
//! Routes:
//!   * `POST /storage/upload`         — body is the raw file bytes; the
//!     `Content-Type` header is preserved. Returns `{ "id": "<uuid>" }`.
//!   * `GET  /storage/file/<id>`      — serves the stored bytes back with the
//!     original content type.
//!   * `GET  /storage/health`         — `{ "status": "ok" }`.
//!
//! Blobs live under `<storage_root>/public-files/` via the existing
//! [`IFile`](crate::models::ports::file::IFile) global-storage driver: `<id>`
//! holds the bytes and `<id>.type` the content type. Ids are opaque UUIDs and
//! are validated on read so a crafted path can never escape the directory.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use uuid::Uuid;

use crate::models::core::ICore;
use aseman_contracts::legacy_storage_http::{escape_json, is_safe_id, sanitize_content_type};

/// Where public blobs live, relative to the node storage root.
const PUBLIC_DIR: &str = "public-files";
/// Spawn the storage HTTP server on `port` (no-op when `port <= 0`).
pub fn start(app: Arc<dyn ICore>, port: i64, max_bytes: usize) {
    if port <= 0 {
        return;
    }
    let listener = match TcpListener::bind(format!("0.0.0.0:{}", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("storage http bind :{} failed: {}", port, e);
            return;
        }
    };
    let server = Arc::new(StorageHttp { app, max_bytes });
    eprintln!("[startup] storage http listening on :{}", port);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let s = server.clone();
            thread::spawn(move || s.handle(stream));
        }
    });
}

struct StorageHttp {
    app: Arc<dyn ICore>,
    max_bytes: usize,
}

impl StorageHttp {
    fn public_root(&self) -> String {
        format!(
            "{}/{}",
            self.app.tools().storage().storage_root(),
            PUBLIC_DIR
        )
    }

    fn handle(self: Arc<Self>, mut stream: TcpStream) {
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        });

        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() || request_line.trim().is_empty() {
            return;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        // Read headers.
        let mut content_length: usize = 0;
        let mut content_type = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(':') {
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim();
                match key.as_str() {
                    "content-length" => content_length = val.parse().unwrap_or(0),
                    "content-type" => content_type = val.to_string(),
                    _ => {}
                }
            }
        }

        // Strip a query string for routing (uploads may pass ?filename=…).
        let route = path.split('?').next().unwrap_or(&path).to_string();

        match (method.as_str(), route.as_str()) {
            ("GET", "/storage/health") => {
                write_response(&mut stream, 200, "application/json", b"{\"status\":\"ok\"}");
            }
            ("POST", "/storage/upload") | ("PUT", "/storage/upload") => {
                self.handle_upload(&mut stream, &mut reader, content_length, &content_type);
            }
            ("GET", p) | ("HEAD", p) if p.starts_with("/storage/file/") => {
                let id = &p["/storage/file/".len()..];
                self.handle_download(&mut stream, id, method == "HEAD");
            }
            _ => write_response(
                &mut stream,
                404,
                "application/json",
                b"{\"error\":\"not found\"}",
            ),
        }
    }

    fn handle_upload(
        self: &Arc<Self>,
        stream: &mut TcpStream,
        reader: &mut BufReader<TcpStream>,
        content_length: usize,
        content_type: &str,
    ) {
        if content_length == 0 {
            write_response(
                stream,
                400,
                "application/json",
                b"{\"error\":\"empty body\"}",
            );
            return;
        }
        if content_length > self.max_bytes {
            write_response(
                stream,
                413,
                "application/json",
                b"{\"error\":\"file too large\"}",
            );
            return;
        }
        let mut data = vec![0u8; content_length];
        if reader.read_exact(&mut data).is_err() {
            write_response(
                stream,
                400,
                "application/json",
                b"{\"error\":\"truncated body\"}",
            );
            return;
        }

        let id = Uuid::new_v4().to_string();
        let ctype = sanitize_content_type(content_type);
        let root = self.public_root();
        let file = self.app.tools().file();
        if let Err(e) = file.save_data_to_global_storage(&root, &data, &id, true) {
            write_response(
                stream,
                500,
                "application/json",
                format!("{{\"error\":\"{}\"}}", escape_json(&e.to_string())).as_bytes(),
            );
            return;
        }
        // Sidecar holding the content type so downloads round-trip it.
        let _ = file.save_data_to_global_storage(
            &root,
            ctype.as_bytes(),
            &format!("{}.type", id),
            true,
        );

        write_response(
            stream,
            200,
            "application/json",
            format!("{{\"id\":\"{}\",\"contentType\":\"{}\"}}", id, ctype).as_bytes(),
        );
    }

    fn handle_download(self: &Arc<Self>, stream: &mut TcpStream, id: &str, head_only: bool) {
        if !is_safe_id(id) {
            write_response(
                stream,
                400,
                "application/json",
                b"{\"error\":\"invalid id\"}",
            );
            return;
        }
        let root = self.public_root();
        let file = self.app.tools().file();
        let bytes = match file.read_file_by_path(&format!("{}/{}", root, id)) {
            Ok(b) => b,
            Err(_) => {
                write_response(
                    stream,
                    404,
                    "application/json",
                    b"{\"error\":\"not found\"}",
                );
                return;
            }
        };
        let ctype = file
            .read_file_by_path(&format!("{}/{}.type", root, id))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "application/octet-stream".to_string());

        // Public, immutable blobs — safe to cache aggressively.
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: public, max-age=31536000, immutable\r\nConnection: close\r\n\r\n",
            sanitize_content_type(&ctype),
            bytes.len()
        );
        let _ = stream.write_all(header.as_bytes());
        if !head_only {
            let _ = stream.write_all(&bytes);
        }
    }
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod characterization_tests {
    use super::*;
    use std::net::Shutdown;

    #[test]
    fn response_writer_preserves_the_current_http_wire_shape() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let writer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test connection");
            write_response(
                &mut stream,
                413,
                "application/json",
                br#"{"error":"too large"}"#,
            );
            stream.shutdown(Shutdown::Write).expect("finish response");
        });

        let mut client = TcpStream::connect(address).expect("connect test client");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read response");
        writer.join().expect("writer thread");

        assert_eq!(
            response,
            concat!(
                "HTTP/1.1 413 Payload Too Large\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 21\r\n",
                "Connection: close\r\n",
                "\r\n",
                "{\"error\":\"too large\"}"
            )
        );
    }
}
