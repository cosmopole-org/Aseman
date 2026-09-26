//! Login grants: how a node that does not verify identities itself lets a
//! trusted program vouch for a person.
//!
//! `/creatures/login` answers with the account's private key. Its fallback
//! accepts an email with no proof at all, so on a node where accounts matter
//! anyone who knows an address could sign in as its owner. With
//! `CASPAR_LOGIN_MODE=grant` the login instead requires a grant: a single-use,
//! short-lived nonce bound to one email, issued by `grantLogin` to a program the
//! NODE OWNER owns, after that program has verified the person by whatever means
//! it offers (a password, a mailed code, a Google ID token).

use crate::drivers::vmm::globals::with_global_app;
use crate::drivers::vmm::prelude::*;
use crate::models::transaction::ITrx;

/// The longest a grant may live. A login follows its grant within seconds.
const MAX_TTL_SECS: i64 = 300;

fn grant_key(nonce: &str) -> String {
    format!("LoginGrant::{}", nonce)
}

/// Whether `/creatures/login` requires a grant on this node.
pub(crate) fn grant_mode() -> bool {
    aseman_config::legacy_adapter_snapshot()
        .map(|config| config.login_grant_required)
        .unwrap_or(false)
}

/// `grantLogin` host op: `{email, ttlSecs?}` -> `{ok, grant, expiresAt}`.
///
/// Only a program whose owning user is the node owner may call it: a grant is a
/// login as any account on the node, so it is exactly as powerful as the owner.
pub(crate) fn host_fn_grant_login(caller_program_id: &str, input: &JsonValue) -> String {
    let email = input["email"].as_str().unwrap_or("").trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return json!({"ok": false, "error": "grantLogin requires an email"}).to_string();
    }
    let Some(app) = with_global_app(|app| app.clone()) else {
        return json!({"ok": false, "error": "vmm not initialised"}).to_string();
    };
    let owner =
        crate::drivers::vmm::host::functions::vm_ownership::program_owner_user(caller_program_id);
    if owner.is_empty() || owner != app.owner_id() {
        return json!({"ok": false, "error": "only a program owned by the node owner may grant a login"}).to_string();
    }
    let ttl = input["ttlSecs"]
        .as_i64()
        .unwrap_or(120)
        .clamp(10, MAX_TTL_SECS);
    let expires_at = chrono::Utc::now().timestamp_millis() + ttl * 1000;
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let key = grant_key(&nonce);
    let value = format!("{}|{}", expires_at, email);
    app.modify_state(
        false,
        Box::new(move |trx: &dyn ITrx| {
            trx.put_link(&key, &value);
            Ok(())
        }),
    );
    json!({"ok": true, "grant": nonce, "expiresAt": expires_at}).to_string()
}

/// Check and spend a grant inside the login transaction.
pub(crate) fn consume(trx: &dyn ITrx, nonce: &str, email: &str) -> anyhow::Result<()> {
    let nonce = nonce.trim();
    if nonce.is_empty() {
        return Err(anyhow::anyhow!(
            "this node requires a login grant; sign in through the platform"
        ));
    }
    let key = grant_key(nonce);
    let stored = trx.get_link(&key);
    if stored.is_empty() {
        return Err(anyhow::anyhow!(
            "that login grant is invalid or already used"
        ));
    }
    // Spent before anything else is checked, so a grant is never usable twice.
    trx.del_key(&format!("link::{}", key));
    let (expires_at, granted_email) =
        parse(&stored).ok_or_else(|| anyhow::anyhow!("that login grant is malformed"))?;
    if chrono::Utc::now().timestamp_millis() > expires_at {
        return Err(anyhow::anyhow!("that login grant has expired"));
    }
    if granted_email != email {
        return Err(anyhow::anyhow!(
            "that login grant is for a different account"
        ));
    }
    Ok(())
}

fn parse(stored: &str) -> Option<(i64, String)> {
    let (expires, email) = stored.split_once('|')?;
    Some((expires.parse::<i64>().ok()?, email.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_grant_reads_back_its_expiry_and_email() {
        assert_eq!(
            parse("1700000000000|a@b.co"),
            Some((1700000000000, "a@b.co".to_string()))
        );
    }

    #[test]
    fn a_malformed_grant_is_rejected() {
        assert_eq!(parse("not-a-grant"), None);
        assert_eq!(parse("soon|a@b.co"), None);
    }
}
