//! Public files. The bytes live off-chain under the node's public-files folder and
//! only the returned id is meant to go on-chain (an avatar id in a profile). The
//! storage HTTP endpoint serves them back at `GET /storage/file/<id>`; an owner
//! sidecar records who uploaded each one.

use anyhow::{Result, anyhow};
use aseman_ports::BlobStore;
use base64::Engine;
use serde_json::{Value, json};

use super::Ctx;
use super::wire::creature::StorageUploadInput;
use crate::blobs::{PUBLIC_FILES, node_blobs};

/// The largest file accepted, as the storage HTTP endpoint serves.
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

pub(super) fn upload(ctx: &Ctx<'_>, input: StorageUploadInput) -> Result<Value> {
    let owner = &ctx.caller.user_id;
    if owner.is_empty() {
        return Err(anyhow!("not authenticated"));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(input.data_base64.trim())
        .map_err(|_| anyhow!("dataBase64 is not valid base64"))?;
    if data.is_empty() {
        return Err(anyhow!("empty file"));
    }
    if data.len() > MAX_FILE_BYTES {
        return Err(anyhow!("file too large (max {MAX_FILE_BYTES} bytes)"));
    }
    let content_type = match input.content_type.trim() {
        "" => "application/octet-stream".to_owned(),
        given => given.to_owned(),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let blobs = node_blobs(&ctx.node.tools().storage());
    blobs
        .put_blob(&format!("{PUBLIC_FILES}/{id}"), &data, &content_type, true)
        .map_err(|error| anyhow!("storage write failed: {error}"))?;
    // Sidecars: the content type (so a download round-trips it) and the owner.
    blobs
        .put_blob(
            &format!("{PUBLIC_FILES}/{id}.type"),
            content_type.as_bytes(),
            "text/plain",
            true,
        )
        .map_err(|error| anyhow!("storage write failed: {error}"))?;
    blobs
        .put_blob(
            &format!("{PUBLIC_FILES}/{id}.owner"),
            owner.as_bytes(),
            "text/plain",
            true,
        )
        .map_err(|error| anyhow!("storage write failed: {error}"))?;
    Ok(json!({ "ok": true, "id": id, "contentType": content_type }))
}
