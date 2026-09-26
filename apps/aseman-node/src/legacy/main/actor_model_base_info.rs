//! Translation of `core/module/actor/model/base/info.go`.

use std::sync::Mutex;

use crate::models::info::IInfo;

/// Identity context (user / store / god flag) attached to a state-modifying
/// call. Matches `base.Info` field-for-field; the god flag is mutable so the
/// shell can re-stamp it after a re-authentication.
pub struct Info {
    inner: Mutex<Inner>,
}

struct Inner {
    is_god: bool,
    user_id: String,
    store_id: String,
}

impl Info {
    /// Equivalent of `base.NewInfo`.
    pub fn new(user_id: &str, store_id: &str) -> Info {
        Info {
            inner: Mutex::new(Inner {
                is_god: false,
                user_id: user_id.to_string(),
                store_id: store_id.to_string(),
            }),
        }
    }

    /// Equivalent of `base.NewGodInfo`.
    pub fn new_with_god(user_id: &str, store_id: &str, is_god: bool) -> Info {
        Info {
            inner: Mutex::new(Inner {
                is_god,
                user_id: user_id.to_string(),
                store_id: store_id.to_string(),
            }),
        }
    }
}

impl IInfo for Info {
    fn is_god(&self) -> bool {
        self.inner.lock().unwrap().is_god
    }

    fn user_id(&self) -> String {
        self.inner.lock().unwrap().user_id.clone()
    }

    fn store_id(&self) -> String {
        self.inner.lock().unwrap().store_id.clone()
    }

    fn identity(&self) -> (String, String) {
        let inner = self.inner.lock().unwrap();
        let mut parts = inner.user_id.splitn(2, '@');
        match (parts.next(), parts.next()) {
            (Some(name), Some(realm)) => (name.to_string(), realm.to_string()),
            _ => (String::new(), String::new()),
        }
    }
}
