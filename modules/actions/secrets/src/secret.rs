//! The secret handlers.

use anyhow::{Result, anyhow};
use chrono::Utc;
use serde_json::{Value, json};

use aseman_action_sdk::state::secrets;
use aseman_action_sdk::util::{Ctx, secret_crypto};
use aseman_action_sdk::wire::creature::{
    SecretGetInput, SecretGrantInput, SecretListGrantedInput, SecretListInput, SecretPutInput,
    SecretRevokeInput,
};

/// Names and ids are components of secret record keys, so a `:` would reach
/// into another namespace: refused rather than sanitized.
fn valid_component(value: &str) -> bool {
    !value.is_empty() && !value.contains(':')
}

fn caller(ctx: &Ctx<'_>) -> Result<String> {
    if ctx.caller.user_id.is_empty() {
        return Err(anyhow!("not authenticated"));
    }
    Ok(ctx.caller.user_id.clone())
}

fn master_key(ctx: &Ctx<'_>) -> Result<[u8; 32]> {
    ctx.node.tools().storage().master_key()
}

pub fn put(ctx: &Ctx<'_>, input: SecretPutInput) -> Result<Value> {
    let owner = caller(ctx)?;
    if !valid_component(&input.name) {
        return Err(anyhow!("secret name is required and must not contain ':'"));
    }
    if input.value.is_empty() {
        return Err(anyhow!("secret value is required"));
    }
    let key = master_key(ctx)?;
    let blob = secret_crypto::encrypt(input.value.as_bytes(), &key)?;
    secrets::put_blob(ctx.trx, &owner, &input.name, &blob, &key)?;
    Ok(json!({ "ok": true, "name": input.name }))
}

/// Read a secret: the caller's own, or another owner's under an unexpired grant.
pub fn get(ctx: &Ctx<'_>, input: SecretGetInput) -> Result<Value> {
    let caller = caller(ctx)?;
    if !valid_component(&input.name) {
        return Err(anyhow!("secret name is required"));
    }
    let owner = if input.owner.is_empty() {
        caller.clone()
    } else {
        input.owner
    };
    if owner != caller {
        let expires_at = secrets::grant_expiry(ctx.trx, &owner, &input.name, &caller)?;
        if expires_at <= 0 || Utc::now().timestamp_millis() >= expires_at {
            return Err(anyhow!("access denied: no valid grant for this secret"));
        }
    }
    let Some(blob) = secrets::blob(ctx.trx, &owner, &input.name)? else {
        return Err(anyhow!("secret not found"));
    };
    let plaintext = secret_crypto::decrypt(&blob, &master_key(ctx)?)?;
    let value =
        String::from_utf8(plaintext).map_err(|_| anyhow!("stored secret is not valid UTF-8"))?;
    Ok(json!({ "ok": true, "owner": owner, "name": input.name, "value": value }))
}

/// Grant `grantee` read access to one of the caller's secrets for `ttlSeconds`.
pub fn grant(ctx: &Ctx<'_>, input: SecretGrantInput) -> Result<Value> {
    let owner = caller(ctx)?;
    if !valid_component(&input.name) || !valid_component(&input.grantee) {
        return Err(anyhow!(
            "name and grantee are required and must not contain ':'"
        ));
    }
    if input.ttl_seconds <= 0 {
        return Err(anyhow!("ttlSeconds must be positive"));
    }
    if secrets::blob(ctx.trx, &owner, &input.name)?.is_none() {
        return Err(anyhow!("secret not found"));
    }
    let expires_at = Utc::now().timestamp_millis() + input.ttl_seconds * 1000;
    secrets::grant(ctx.trx, &owner, &input.name, &input.grantee, expires_at)?;
    Ok(json!({ "ok": true, "grantee": input.grantee, "expiresAt": expires_at }))
}

pub fn revoke(ctx: &Ctx<'_>, input: SecretRevokeInput) -> Result<Value> {
    let owner = caller(ctx)?;
    if !valid_component(&input.name) || !valid_component(&input.grantee) {
        return Err(anyhow!("name and grantee are required"));
    }
    secrets::revoke(ctx.trx, &owner, &input.name, &input.grantee)?;
    Ok(json!({ "ok": true }))
}

/// The secrets granted to the caller, so a grantee (the agent backbone, for one)
/// discovers what it may read without knowing the owners.
pub fn list_granted(ctx: &Ctx<'_>, _: SecretListGrantedInput) -> Result<Value> {
    let grants = secrets::granted_to(ctx.trx, &caller(ctx)?)?;
    Ok(json!({ "ok": true, "grants": grants }))
}

pub fn list(ctx: &Ctx<'_>, _: SecretListInput) -> Result<Value> {
    let names = secrets::names(ctx.trx, &caller(ctx)?)?;
    Ok(json!({ "ok": true, "names": names }))
}