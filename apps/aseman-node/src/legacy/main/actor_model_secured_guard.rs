//! Translation of `core/module/actor/model/secured/guard.go`.
//!
//! `Guard` authenticates the caller of a secured action: it validates the
//! signature against the user id, optionally checks store access, and returns
//! the [`Info`] context the action's state modifier closures will see.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::models::core::ICore;
use crate::models::transaction::ITrx;

use crate::core::actor::model::base::info::Info;

/// Helper: read the `User.<id>.type` column inside a read-only state
/// modification. Used by `check_validity*` to identify `machine` users that
/// bypass the signature check via the `#appletsign` marker.
fn read_user_type(app: &Arc<dyn ICore>, user_id: &str) -> String {
    let slot = Arc::new(Mutex::new(String::new()));
    let slot_clone = slot.clone();
    let user_id_owned = user_id.to_string();
    app.modify_state(
        true,
        Box::new(move |trx: &dyn ITrx| {
            *slot_clone.lock().unwrap() = aseman_ports::CreatureDirectory::creature(
                &crate::shell::api::model::creature_ports::CreaturePorts { trx },
                &user_id_owned,
            )
            .ok()
            .flatten()
            .map(|record| record.creature_type)
            .unwrap_or_default();
            Ok(())
        }),
    );
    let out = slot.lock().unwrap().clone();
    out
}

/// `Guard{IsUser, IsInStore}` flags — derived directly from the action
/// definition and shared by `CheckValidity` / `CheckValidityForChain` /
/// `CheckIdentity`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Guard {
    #[serde(rename = "isUser", default)]
    pub is_user: bool,
    #[serde(rename = "isInStore", default)]
    pub is_in_store: bool,
    /// Whether a machine may authenticate through the in-process applet
    /// marker. Financial actions set this to false so value movement always
    /// requires a real signature from the declared authority.
    #[serde(rename = "allowAppletSign", default)]
    pub allow_applet_sign: bool,
}

impl Guard {
    /// Optional-identity path: empty user/sig means "anonymous OK".
    fn optional_identity(
        &self,
        app: &dyn ICore,
        packet: &[u8],
        signature: &str,
        user_id: &str,
        store_id: &str,
    ) -> (bool, Info) {
        if user_id.is_empty() && signature.is_empty() {
            return (true, Info::new("", ""));
        }
        if user_id.is_empty() || signature.is_empty() {
            return (false, Info::new("", ""));
        }
        let (identified, _, is_god) = app
            .tools()
            .security()
            .auth_with_signature(user_id, packet, signature);
        if !identified {
            return (false, Info::new("", ""));
        }
        if !self.is_in_store {
            return (true, Info::new_with_god(user_id, "", is_god));
        }
        let has_access = app
            .tools()
            .security()
            .has_access_to_store(user_id, store_id);
        if !has_access {
            return (false, Info::new("", ""));
        }
        (true, Info::new_with_god(user_id, store_id, is_god))
    }

    /// Standard path used by HTTP / WS / TCP entry points.
    pub fn check_validity(
        &self,
        app: Arc<dyn ICore>,
        packet: &[u8],
        signature: &str,
        user_id: &str,
        store_id: &str,
        insider: &[bool],
    ) -> (bool, Info) {
        if !self.is_user {
            return self.optional_identity(app.as_ref(), packet, signature, user_id, store_id);
        }
        if self.allow_applet_sign
            && matches!(insider.first(), Some(true))
            && signature == "#appletsign"
        {
            let typ = read_user_type(&app, user_id);
            if typ == "machine" {
                if !self.is_in_store {
                    return (true, Info::new_with_god(user_id, "", false));
                }
                let has_access = app
                    .tools()
                    .security()
                    .has_access_to_store(user_id, store_id);
                if !has_access {
                    return (false, Info::new("", ""));
                }
                return (true, Info::new_with_god(user_id, store_id, false));
            }
        }
        let (identified, _, is_god) = app
            .tools()
            .security()
            .auth_with_signature(user_id, packet, signature);
        if !identified {
            return (false, Info::new("", ""));
        }
        if !self.is_in_store {
            return (true, Info::new_with_god(user_id, "", is_god));
        }
        let has_access = app
            .tools()
            .security()
            .has_access_to_store(user_id, store_id);
        if !has_access {
            return (false, Info::new("", ""));
        }
        (true, Info::new_with_god(user_id, store_id, is_god))
    }

    /// Chain-origin variant: `#appletsign` is always honoured (no `insider`
    /// gating because chain packets are inherently insider).
    pub fn check_validity_for_chain(
        &self,
        app: Arc<dyn ICore>,
        packet: &[u8],
        signature: &str,
        user_id: &str,
        store_id: &str,
    ) -> (bool, Info) {
        if !self.is_user {
            return self.optional_identity(app.as_ref(), packet, signature, user_id, store_id);
        }
        if self.allow_applet_sign && signature == "#appletsign" {
            let typ = read_user_type(&app, user_id);
            if typ == "machine" {
                if !self.is_in_store {
                    return (true, Info::new_with_god(user_id, "", false));
                }
                let has_access = app
                    .tools()
                    .security()
                    .has_access_to_store(user_id, store_id);
                if !has_access {
                    return (false, Info::new("", ""));
                }
                return (true, Info::new_with_god(user_id, store_id, false));
            }
        }
        let (identified, _, is_god) = app
            .tools()
            .security()
            .auth_with_signature(user_id, packet, signature);
        if !identified {
            return (false, Info::new("", ""));
        }
        if !self.is_in_store {
            return (true, Info::new_with_god(user_id, "", is_god));
        }
        let has_access = app
            .tools()
            .security()
            .has_access_to_store(user_id, store_id);
        if !has_access {
            return (false, Info::new("", ""));
        }
        (true, Info::new_with_god(user_id, store_id, is_god))
    }

    /// Identity-only check: no store lookup, used by `logout` / `authenticate`.
    pub fn check_identity(
        &self,
        app: Arc<dyn ICore>,
        packet: &[u8],
        signature: &str,
        user_id: &str,
    ) -> bool {
        if !self.is_user {
            if user_id.is_empty() && signature.is_empty() {
                return true;
            }
            if user_id.is_empty() || signature.is_empty() {
                return false;
            }
            let (identified, _, _) = app
                .tools()
                .security()
                .auth_with_signature(user_id, packet, signature);
            return identified;
        }
        let (identified, _, _) = app
            .tools()
            .security()
            .auth_with_signature(user_id, packet, signature);
        identified
    }
}
