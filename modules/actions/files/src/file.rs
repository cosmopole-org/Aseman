//! The public file upload handler.

use anyhow::{Result, anyhow};
use aseman_ports::BlobStore;
use base64::Engine;
use serde_json::{Value, json};

use aseman_action_sdk::blobs::PUBLIC_FILES;
use aseman_action_sdk::util::Ctx;
use aseman_action_sdk::wire::creature::StorageUploadInput;

/// The largest file accepted, as the storage HTTP endpoint serves.
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

pub fn upload(ctx: &Ctx<'_>, input: StorageUploadInput) -> Result<Value> {
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
    let blobs = ctx.node.tools().storage().blob_store();
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