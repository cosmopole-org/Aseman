//! Translation of `core/module/actor/model/secured/action.go`.
//!
//! `SecureAction` wraps an [`IAction`] with a [`Guard`] and a set of
//! protocol-specific input parsers. The three secured entry points
//! (`securely_act` / `securly_act_chain` / `securely_act_fed`) authenticate
//! the caller, route the request across federation / chain when its origin is
//! remote, and otherwise dispatch through `core.modify_state_securly`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::models::action::{IAction, ISecureAction};
use crate::models::core::{ICore, StateClosure};
use crate::models::globe::BaseResponseCallback;
use crate::models::input::IInput;
use crate::models::ports::network::federation::FedRequestCallback;
use crate::models::state::IState;
use crate::util::GoError;

use super::guard::Guard;

/// Input parser keyed by protocol name (`"tcp"`, `"ws"`, `"chain"`, `"*"`).
pub type Parse = Arc<dyn Fn(Value) -> Result<Arc<dyn IInput>> + Send + Sync>;

/// Concrete [`ISecureAction`] implementation. Owns the inner [`IAction`], a
/// [`Guard`], a back-reference to [`ICore`] (so it can dispatch to federation
/// / chain when needed), and the per-protocol parsers.
pub struct SecureAction {
    pub action: Arc<dyn IAction>,
    pub guard: Guard,
    pub core: Arc<dyn ICore>,
    pub parsers: HashMap<String, Parse>,
}

impl SecureAction {
    /// `NewSecureAction(action, guard, core, parsers)`.
    pub fn new(
        action: Arc<dyn IAction>,
        guard: Guard,
        core: Arc<dyn ICore>,
        parsers: HashMap<String, Parse>,
    ) -> SecureAction {
        SecureAction {
            action,
            guard,
            core,
            parsers,
        }
    }

    fn parser_for(&self, protocol: &str) -> Option<&Parse> {
        self.parsers.get(protocol).or_else(|| self.parsers.get("*"))
    }
}

impl ISecureAction for SecureAction {
    fn key(&self) -> String {
        self.action.key()
    }

    fn inner_action(&self) -> Arc<dyn IAction> {
        self.action.clone()
    }

    fn has_global_parser(&self) -> bool {
        self.parsers.contains_key("*")
    }

    fn parse_input(&self, protocol: &str, raw: Value) -> Result<Arc<dyn IInput>> {
        let parser = self
            .parser_for(protocol)
            .ok_or_else(|| anyhow!("no parser registered for protocol `{}`", protocol))?;
        parser(raw)
    }

    fn securly_act_chain(
        &self,
        user_id: &str,
        _packet_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
        origin: &str,
        _tag: &str,
    ) -> Result<(i64, Value)> {
        let (ok, info) = self.guard.check_validity_for_chain(
            self.core.clone(),
            packet_binary,
            packet_signature,
            user_id,
            &input.get_store_id(),
        );
        if !ok {
            return Err(anyhow!("authorization failed"));
        }
        let slot: Arc<Mutex<Option<Result<(i64, Value)>>>> = Arc::new(Mutex::new(None));
        let slot_clone = slot.clone();
        let action = self.action.clone();
        let input_clone = input.clone();
        let closure: StateClosure = Box::new(move |state: Arc<dyn IState>| {
            let res = action.act(state, input_clone.clone());
            *slot_clone.lock().unwrap() = Some(res);
            Ok(())
        });
        self.core
            .modify_state_securly_with_source(false, Arc::new(info), origin, closure);
        let taken = slot.lock().unwrap().take();
        match taken {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(e),
            None => Err(anyhow!("action produced no result")),
        }
    }

    fn securely_act(
        &self,
        user_id: &str,
        packet_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
        _dummy: &str,
        insider: &[bool],
    ) -> Result<(i64, Value)> {
        let mut origin = input.origin();
        if origin.is_empty() {
            origin = self.core.id();
        }

        // origin == "global" → bounce through the chain via globe.
        if origin == "global" {
            return self.dispatch_via_chain(user_id, packet_binary, packet_signature);
        }

        // origin == our id → run locally.
        if self.core.id() == origin {
            let (ok, info) = self.guard.check_validity(
                self.core.clone(),
                packet_binary,
                packet_signature,
                user_id,
                &input.get_store_id(),
                insider,
            );
            if !ok {
                return Err(anyhow!("authorization failed"));
            }
            let slot: Arc<Mutex<Option<Result<(i64, Value)>>>> = Arc::new(Mutex::new(None));
            let slot_clone = slot.clone();
            let action = self.action.clone();
            let input_clone = input.clone();
            let origin_owned = origin.clone();
            let closure: StateClosure = Box::new(move |state: Arc<dyn IState>| {
                state.set_source(&origin_owned);
                let res = action.act(state, input_clone.clone());
                *slot_clone.lock().unwrap() = Some(res);
                Ok(())
            });
            self.core
                .modify_state_securly(false, Arc::new(info), closure);
            let taken = slot.lock().unwrap().take();
            return match taken {
                Some(Ok(v)) => Ok(v),
                Some(Err(e)) => Err(e),
                None => Err(anyhow!("action produced no result")),
            };
        }

        // origin is a different node → identity-check and forward via
        // federation.
        let ok =
            self.guard
                .check_identity(self.core.clone(), packet_binary, packet_signature, user_id);
        if !ok {
            return Err(anyhow!("authorization failed"));
        }
        self.dispatch_via_federation(&origin, packet_id, user_id, packet_binary, packet_signature)
    }

    fn securely_act_fed(
        &self,
        user_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
        input: Arc<dyn IInput>,
    ) -> Result<(i64, Value)> {
        let (ok, info) = self.guard.check_validity(
            self.core.clone(),
            packet_binary,
            packet_signature,
            user_id,
            &input.get_store_id(),
            &[],
        );
        if !ok {
            return Err(anyhow!("authorization failed"));
        }
        let slot: Arc<Mutex<Option<Result<(i64, Value)>>>> = Arc::new(Mutex::new(None));
        let slot_clone = slot.clone();
        let action = self.action.clone();
        let input_clone = input.clone();
        let closure: StateClosure = Box::new(move |state: Arc<dyn IState>| {
            state.set_source(&input_clone.origin());
            let res = action.act(state, input_clone.clone());
            *slot_clone.lock().unwrap() = Some(res);
            Ok(())
        });
        self.core
            .modify_state_securly(false, Arc::new(info), closure);
        let taken = slot.lock().unwrap().take();
        match taken {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(e),
            None => Err(anyhow!("action produced no result")),
        }
    }
}

impl SecureAction {
    fn dispatch_via_chain(
        &self,
        user_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
    ) -> Result<(i64, Value)> {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<(i64, Value, Option<GoError>)>();
        let cb: BaseResponseCallback = Box::new(move |data, res_code, err| {
            let mut value = Value::Object(Map::new());
            if !data.is_empty() {
                if let Ok(v) = serde_json::from_slice::<Value>(&data) {
                    value = v;
                }
            }
            let _ = tx.send((res_code, value, err));
        });
        self.core.globe().send_base_request_on_chain(
            &self.key(),
            packet_binary.to_vec(),
            packet_signature,
            user_id,
            "",
            cb,
        );
        let (sc, v, err) = rx
            .recv()
            .map_err(|e| anyhow!("chain response channel closed: {}", e))?;
        match err {
            Some(e) => Err(anyhow!("{}", e)),
            None => Ok((sc, v)),
        }
    }

    fn dispatch_via_federation(
        &self,
        origin: &str,
        packet_id: &str,
        user_id: &str,
        packet_binary: &[u8],
        packet_signature: &str,
    ) -> Result<(i64, Value)> {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<(i64, Value, Option<GoError>)>();
        let cb: FedRequestCallback = Box::new(move |data, res_code, err| {
            let mut value = Value::Object(Map::new());
            if !data.is_empty() {
                if let Ok(v) = serde_json::from_slice::<Value>(&data) {
                    value = v;
                } else {
                    let _ = tx.send((3, value.clone(), err));
                    return;
                }
            }
            let _ = tx.send((res_code, value, err));
        });
        self.core
            .tools()
            .network()
            .federation()
            .send_fed_request_by_callback(
                origin,
                packet_id,
                user_id,
                &self.key(),
                packet_binary.to_vec(),
                packet_signature,
                cb,
            );
        let (sc, v, err) = rx
            .recv()
            .map_err(|e| anyhow!("federation response channel closed: {}", e))?;
        match err {
            Some(e) => Err(anyhow!("{}", e)),
            None => Ok((sc, v)),
        }
    }
}
