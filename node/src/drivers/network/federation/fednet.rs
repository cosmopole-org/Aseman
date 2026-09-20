//! Translation of `drivers/network/federation/federation.go`.
//!
//! `FedNet` is the high-level federation driver implementing
//! [`IFederation`]. It exposes the outbound API (request / response / update
//! / request-by-callback) and registers a bridge with the federation TCP
//! server so inbound packets get dispatched to the right action / signaler
//! path.

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use serde_json::Value;

use crate::models::core::ICore;
use crate::models::packet::{build_error_json, OriginPacket};
use crate::models::ports::file::IFile;
use crate::models::ports::network::federation::{FedRequestCallback, IFederation};
use crate::models::ports::network::TlsConfig;
use crate::models::ports::signaler::ISignaler;
use crate::models::ports::storage::IStorage;
use crate::models::transaction::ITrx;
use crate::shell::api::model::StorePermissions;
use crate::shell::api::packets::{invites, stores};
use crate::shell::utils::crypto::secure_unique_string;
use crate::util::GoError;

use super::netserver::{FedApi, Socket, Tcp};

/// In-flight callback awaiting a federation response.
struct FedPacketCallback {
    user_id: String,
    key: String,
    request: Vec<u8>,
    /// Caller-supplied correlation id (Go: `UserRequestId`). Stored so
    /// downstream tooling can match a federation response back to the
    /// original user-facing request that triggered it.
    user_request_id: String,
    callback: Arc<Mutex<Option<FedRequestCallback>>>,
}

/// Concrete [`IFederation`] implementation.
pub struct FedNet {
    app: Arc<dyn ICore>,
    storage: Mutex<Option<Arc<dyn IStorage>>>,
    file: Mutex<Option<Arc<dyn IFile>>>,
    signaler: Mutex<Option<Arc<dyn ISignaler>>>,
    gateway: Mutex<Option<Arc<Tcp>>>,
    packet_callbacks: Arc<DashMap<String, Arc<FedPacketCallback>>>,
    port: Mutex<i64>,
    tls_config: Mutex<Option<TlsConfig>>,
}

impl FedNet {
    /// Stage one — `FirstStageBackFill(core)`. The federation driver is
    /// instantiated before `Core.tools` is fully wired (a chicken-and-egg
    /// problem the Go code papered over with mutation); we mirror the same
    /// two-stage handshake.
    pub fn first_stage(app: Arc<dyn ICore>) -> Arc<FedNet> {
        Arc::new(FedNet {
            app,
            storage: Mutex::new(None),
            file: Mutex::new(None),
            signaler: Mutex::new(None),
            gateway: Mutex::new(None),
            packet_callbacks: Arc::new(DashMap::new()),
            port: Mutex::new(0),
            tls_config: Mutex::new(None),
        })
    }

    /// Stage two — `SecondStageForFill(storage, file, signaler)`. Wires the
    /// driver to the rest of the runtime and installs the inbound bridge on
    /// the federation TCP gateway.
    pub fn second_stage(
        self: &Arc<Self>,
        storage: Arc<dyn IStorage>,
        file: Arc<dyn IFile>,
        signaler: Arc<dyn ISignaler>,
    ) {
        *self.storage.lock().unwrap() = Some(storage);
        *self.file.lock().unwrap() = Some(file);
        *self.signaler.lock().unwrap() = Some(signaler);

        let gateway = Tcp::new(self.app.clone());
        let trans = self.clone();
        let bridge: FedApi = Arc::new(move |socket, ip, pack| {
            trans.handle_packet(socket, ip, pack);
        });
        gateway.inject_bridge(bridge);
        *self.gateway.lock().unwrap() = Some(gateway);
    }

    fn resolve_destination(&self, dest_org: &str) -> Option<String> {
        // Resolve to IPv4, then verify the IP is in the chain's peer list
        // (matches Go's `net.LookupIP` + `Peers()` filter).
        let addrs = (dest_org, 0u16).to_socket_addrs().ok()?;
        let mut ipv4 = None;
        for a in addrs {
            if let std::net::IpAddr::V4(v4) = a.ip() {
                ipv4 = Some(v4.to_string());
                break;
            }
        }
        let ip = ipv4?;
        let peers = self.app.tools().network().chain().peers();
        if !peers.iter().any(|p| p == &ip) {
            return None;
        }
        let port = *self.port.lock().unwrap();
        Some(format!("{}:{}", dest_org, port))
    }

    fn open_socket(&self, address: &str) -> Option<Arc<Socket>> {
        let gateway = self.gateway.lock().unwrap().clone()?;
        let tls = self.tls_config.lock().unwrap().clone();
        gateway.new_outbound_socket(address, tls.as_ref())
    }

    fn handle_packet(&self, socket: Arc<Socket>, ip: String, pack: OriginPacket) {
        // Resolve the source channel id from the chain peer table.
        let host_name = self.resolve_hostname_for_ip(&ip);
        if host_name.is_empty() {
            return;
        }
        match pack.typ.as_str() {
            "response" => self.handle_response(pack),
            "update" => self.handle_update(pack),
            "request" => self.handle_request(socket, &host_name, pack),
            _ => {}
        }
    }

    fn resolve_hostname_for_ip(&self, ip: &str) -> String {
        let peers = self.app.tools().network().chain().peers();
        if !peers.iter().any(|p| p == ip) {
            return String::new();
        }
        let host = Arc::new(Mutex::new(String::new()));
        let host_clone = host.clone();
        let key = format!("NodeIpToHost::{}", ip);
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                *host_clone.lock().unwrap() = trx.get_link(&key);
                Ok(())
            }),
        );
        let result = host.lock().unwrap().clone();
        result
    }

    fn handle_response(&self, pack: OriginPacket) {
        let Some(cb) = self
            .packet_callbacks
            .get(&pack.request_id)
            .map(|e| e.value().clone())
        else {
            return;
        };
        if pack.res_code == 0 {
            // Side-effect: invite / join / create create the local membership
            // / hasaccess links so the requesting user can address the store.
            self.apply_response_side_effects(&cb, &pack);
        }
        self.packet_callbacks.remove(&pack.request_id);
        let mut slot = cb.callback.lock().unwrap();
        if let Some(callback) = slot.take() {
            if pack.res_code != 0 {
                let err_obj: crate::models::packet::Error =
                    serde_json::from_slice(&pack.binary).unwrap_or_default();
                callback(Vec::new(), 1, Some(anyhow::anyhow!(err_obj.message)));
            } else {
                callback(pack.binary, 0, None);
            }
        }
    }

    fn apply_response_side_effects(&self, cb: &Arc<FedPacketCallback>, pack: &OriginPacket) {
        match cb.key.as_str() {
            "/invites/accept" | "/stores/join" => {
                let store_id = if cb.key == "/invites/accept" {
                    serde_json::from_slice::<invites::AcceptInput>(&cb.request)
                        .ok()
                        .map(|m| m.store_id)
                        .unwrap_or_default()
                } else {
                    serde_json::from_slice::<stores::JoinInput>(&cb.request)
                        .ok()
                        .map(|m| m.store_id)
                        .unwrap_or_default()
                };
                if !store_id.is_empty() {
                    let user_id = cb.user_id.clone();
                    let store_id_clone = store_id.clone();
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            trx.put_link(
                                &format!("onaccess::{}::{}", store_id_clone, user_id),
                                &StorePermissions::member().encode(),
                            );
                            trx.put_link(
                                &format!("hasaccess::{}::{}", user_id, store_id_clone),
                                "true",
                            );
                            Ok(())
                        }),
                    );
                    if let Some(sig) = self.signaler.lock().unwrap().clone() {
                        sig.join_group(&store_id, &cb.user_id);
                    }
                }
            }
            "/stores/create" => {
                if let Ok(out) = serde_json::from_slice::<stores::CreateOutput>(&pack.binary) {
                    let user_id = cb.user_id.clone();
                    let store = out.store.store.clone();
                    let store_id = store.id.clone();
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            store.clone().pull(trx);
                            trx.put_link(
                                &format!("onaccess::{}::{}", store_id, user_id),
                                // The creator of a store administers it.
                                &StorePermissions::owner().encode(),
                            );
                            trx.put_link(&format!("hasaccess::{}::{}", user_id, store_id), "true");
                            Ok(())
                        }),
                    );
                    if let Some(sig) = self.signaler.lock().unwrap().clone() {
                        sig.join_group(&out.store.store.id, &cb.user_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_update(&self, pack: OriginPacket) {
        self.react_to_update(&pack.key, &pack.binary);
        if let Some(sig) = self.signaler.lock().unwrap().clone() {
            if pack.store_id.is_empty() {
                let value = serde_json::from_slice::<Value>(&pack.binary).unwrap_or(Value::Null);
                sig.signal_user(&pack.key, &pack.user_id, value, false);
            } else {
                let value = serde_json::from_slice::<Value>(&pack.binary).unwrap_or(Value::Null);
                // Resolve this node's members of the store from state, the same
                // way the originating node did — never from the group registry,
                // which only knows the stores a member had when they connected.
                // `federate: false`: this packet already came over federation,
                // and pushing it onward would bounce it around the network.
                sig.signal_store(&pack.key, &pack.store_id, value, pack.exceptions, false);
            }
        }
    }

    fn react_to_update(&self, key: &str, data: &[u8]) {
        match key {
            "stores/update" => {
                if let Ok(tc) = serde_json::from_slice::<stores::Update>(data) {
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            tc.store.push(trx);
                            Ok(())
                        }),
                    );
                }
            }
            "stores/delete" => {
                if let Ok(tc) = serde_json::from_slice::<stores::Delete>(data) {
                    let id = tc.store.id;
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            trx.del_key(&format!("obj::Store::{}", id));
                            Ok(())
                        }),
                    );
                }
            }
            "stores/addMember" | "stores/join" => {
                if let Ok(tc) = serde_json::from_slice::<stores::AddMember>(data) {
                    let store_id = tc.store_id;
                    let user_id = tc.user.id;
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            trx.put_link(
                                &format!("onaccess::{}::{}", store_id, user_id),
                                &StorePermissions::member().encode(),
                            );
                            trx.put_link(&format!("hasaccess::{}::{}", user_id, store_id), "true");
                            Ok(())
                        }),
                    );
                }
            }
            "stores/removeMember" => {
                if let Ok(tc) = serde_json::from_slice::<stores::AddMember>(data) {
                    let store_id = tc.store_id;
                    let user_id = tc.user.id;
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            trx.del_key(&format!("link::onaccess::{}::{}", store_id, user_id));
                            trx.del_key(&format!("link::hasaccess::{}::{}", user_id, store_id));
                            Ok(())
                        }),
                    );
                }
            }
            "stores/updateMember" => {
                if let Ok(tc) = serde_json::from_slice::<stores::UpdateMember>(data) {
                    let key_ = format!("member_{}_{}", tc.store_id, tc.user.id);
                    let payload = serde_json::to_value(&tc.metadata).unwrap_or(Value::Null);
                    self.app.modify_state(
                        false,
                        Box::new(move |trx: &dyn ITrx| {
                            trx.put_json(&key_, "meta", &payload, false)?;
                            Ok(())
                        }),
                    );
                }
            }
            _ => {}
        }
    }

    fn handle_request(&self, socket: Arc<Socket>, channel_id: &str, pack: OriginPacket) {
        let _ = socket; // request responses are sent via SendFedResponse below
        let Some(secure) = self.app.actor().fetch_secure_action(&pack.key) else {
            self.send_fed_response_inner(
                channel_id,
                &pack.request_id,
                1,
                &build_error_json("action not found"),
            );
            return;
        };
        let raw_payload = serde_json::from_slice::<Value>(&pack.binary).unwrap_or(Value::Null);
        let input = match secure.parse_input("fed", raw_payload) {
            Ok(i) => i,
            Err(_) => {
                self.send_fed_response_inner(
                    channel_id,
                    &pack.request_id,
                    1,
                    &build_error_json("input could not be parsed"),
                );
                return;
            }
        };
        match secure.securely_act_fed(&pack.user_id, &pack.binary, &pack.signature, input) {
            Ok((_, res)) => {
                self.send_fed_response_inner(channel_id, &pack.request_id, 0, &res);
            }
            Err(e) => {
                self.send_fed_response_inner(
                    channel_id,
                    &pack.request_id,
                    1,
                    &build_error_json(&format!("{}", e)),
                );
            }
        }
    }

    fn send_fed_response_inner<T: serde::Serialize>(
        &self,
        dest_org: &str,
        request_id: &str,
        res_code: i64,
        res: &T,
    ) {
        let Some(address) = self.resolve_destination(dest_org) else {
            return;
        };
        let Some(socket) = self.open_socket(&address) else {
            return;
        };
        let payload = serde_json::to_vec(res).unwrap_or_default();
        socket.write_response(request_id, res_code, "", &payload);
    }
}

impl IFederation for FedNet {
    fn listen(&self, port: i64, tls_config: Option<TlsConfig>) {
        *self.port.lock().unwrap() = port;
        *self.tls_config.lock().unwrap() = tls_config.clone();
        if let Some(gateway) = self.gateway.lock().unwrap().clone() {
            gateway.listen(port, tls_config);
        }
    }

    fn send_fed_request(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
    ) {
        let Some(address) = self.resolve_destination(dest_org) else {
            return;
        };
        let Some(socket) = self.open_socket(&address) else {
            return;
        };
        socket.write_request(request_id, user_id, path, &payload, signature);
    }

    fn send_fed_response(&self, dest_org: &str, request_id: &str, res_code: i64, res: Value) {
        self.send_fed_response_inner(dest_org, request_id, res_code, &res);
    }

    fn send_fed_update(
        &self,
        dest_org: &str,
        key: &str,
        update_pack: Value,
        target_type: &str,
        target_id_val: &str,
        exceptions: Vec<String>,
    ) {
        let Some(address) = self.resolve_destination(dest_org) else {
            return;
        };
        let Some(socket) = self.open_socket(&address) else {
            return;
        };
        let payload = serde_json::to_vec(&update_pack).unwrap_or_default();
        let signature = self.app.sign_packet(&payload);
        socket.write_update(
            key,
            target_type,
            target_id_val,
            &exceptions,
            &signature,
            &payload,
        );
    }

    fn send_fed_request_by_callback(
        &self,
        dest_org: &str,
        request_id: &str,
        user_id: &str,
        path: &str,
        payload: Vec<u8>,
        signature: &str,
        callback: FedRequestCallback,
    ) {
        let Some(address) = self.resolve_destination(dest_org) else {
            callback(
                Vec::new(),
                0,
                Some(anyhow::anyhow!("no route to destination")),
            );
            return;
        };
        let callback_id = secure_unique_string();
        let cb = Arc::new(FedPacketCallback {
            user_id: user_id.to_string(),
            key: path.to_string(),
            request: payload.clone(),
            user_request_id: request_id.to_string(),
            callback: Arc::new(Mutex::new(Some(callback))),
        });
        self.packet_callbacks
            .insert(callback_id.clone(), cb.clone());

        // 120 s timeout (matches Go).
        let trans_callbacks = self.packet_callbacks.clone();
        let cb_for_timeout = cb.clone();
        let callback_id_for_timeout = callback_id.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(120));
            if trans_callbacks.remove(&callback_id_for_timeout).is_some() {
                let mut slot = cb_for_timeout.callback.lock().unwrap();
                if let Some(cb) = slot.take() {
                    cb(
                        Vec::new(),
                        0,
                        Some(anyhow::anyhow!("federation callback timeout")),
                    );
                }
            }
        });
        let _ = cb;

        // If we can't open the socket the callback will never be called via
        // the normal response path.  Remove it from the map and invoke it
        // immediately so callers don't silently block until the 120 s timeout.
        let Some(socket) = self.open_socket(&address) else {
            if let Some((_, pending)) = self.packet_callbacks.remove(&callback_id) {
                let mut slot = pending.callback.lock().unwrap();
                if let Some(cb) = slot.take() {
                    cb(
                        Vec::new(),
                        0,
                        Some(anyhow::anyhow!(
                            "federation: could not open socket to {}",
                            address
                        )),
                    );
                }
            }
            return;
        };
        socket.write_request(&callback_id, user_id, path, &payload, signature);
    }
}

// Pull in a small helper so the unused-import lints stay quiet when only
// some methods are used.
#[allow(dead_code)]
fn _force_keep_imports() -> HashMap<String, String> {
    HashMap::new()
}
