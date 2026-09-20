//! Translation of `src/bots/sampleBot/main.go`.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::models::core::ICore;
use crate::shell::api::packets::stores::SignalInput;
use crate::shell::utils::crypto::secure_unique_string;

/// `BotAgent` — a tiny in-process bot that emits `/stores/signal` packets
/// on behalf of a registered user.
pub struct BotAgent {
    inner: Mutex<Inner>,
}

struct Inner {
    core: Option<Arc<dyn ICore>>,
    user_id: String,
}

impl Default for BotAgent {
    fn default() -> Self {
        BotAgent::new()
    }
}

impl BotAgent {
    pub fn new() -> Self {
        BotAgent {
            inner: Mutex::new(Inner {
                core: None,
                user_id: String::new(),
            }),
        }
    }

    pub fn install(&self, core: Arc<dyn ICore>, user_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.core = Some(core);
        inner.user_id = user_id.to_string();
    }

    pub fn on_signal(&self, _input: SignalInput) -> Value {
        Value::Object(Default::default())
    }

    pub fn send_topic_packet(&self, typ: &str, store_id: &str, user_id: &str, data: &Value) {
        let inner = self.inner.lock().unwrap();
        let Some(core) = inner.core.clone() else {
            return;
        };
        let bot_user_id = inner.user_id.clone();
        drop(inner);

        let inner_data = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
        let packet = SignalInput {
            typ: typ.to_string(),
            store_id: store_id.to_string(),
            user_id: user_id.to_string(),
            data: inner_data,
            ..Default::default()
        };
        let packet_binary = serde_json::to_vec(&packet).unwrap_or_default();
        if let Some(secure) = core.actor().fetch_secure_action("/stores/signal") {
            let packet_arc: Arc<dyn crate::models::input::IInput> = Arc::new(packet.clone());
            let _ = secure.securely_act(
                &bot_user_id,
                &secure_unique_string(),
                &packet_binary,
                "#botsign",
                packet_arc,
                "",
                &[],
            );
        }
    }
}
