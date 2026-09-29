//! The packets the main chain commits, as this node runs them: ordered base
//! requests (through the operations), `vm.execute` messages and their pay-lock
//! consumption, and cost negotiation.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::actions::wire::creature::ConsumeLockInput;
use crate::node::Node;
use crate::transports::chain::globe::ChainPacketOp;
use crate::util::crypto::secure_unique_string;
use aseman_contracts::wire::chain::{ChainBaseRequest, ChainMessage, ChainPayPacket};

impl Node {
    pub(crate) fn chain_message_targets_local(&self, packet: &ChainMessage) -> bool {
        packet.recievers.contains_key("*") || packet.recievers.contains_key(&self.id)
    }

    pub(crate) fn chain_message_machine_ids(&self, packet: &ChainMessage) -> HashMap<String, bool> {
        let mut machine_ids = HashMap::new();
        if let Some(map) = packet.recievers.get(&self.id) {
            for k in map.keys() {
                machine_ids.insert(k.clone(), true);
            }
        }
        if let Some(pay) = &packet.pay {
            for m in &pay.machine_ids {
                machine_ids.insert(m.clone(), true);
            }
        }
        machine_ids
    }

    fn run_chain_message(self: &Arc<Self>, packet: ChainMessage) {
        let machine_ids = self.chain_message_machine_ids(&packet);
        for machine_id in machine_ids.keys() {
            let runtime_type = self
                .read(|trx| {
                    Ok(crate::state::program_ports::ProgramPorts { trx }
                        .program_or_empty(machine_id)
                        .runtime)
                })
                .unwrap_or_default();
            let offered = self
                .vmm()
                .is_some_and(|remote| remote.offers(&runtime_type));
            if !offered {
                continue;
            }
            // The program's signal listener delivers the message to its workloads;
            // without one (no assign yet), it goes to the entity directly.
            let listener = self
                .tools()
                .signaler()
                .listeners()
                .get(machine_id)
                .map(|e| e.value().clone());
            let payload = packet.payload.clone();
            match listener {
                Some(listener) => {
                    thread::spawn(move || {
                        let value = Value::String(String::from_utf8_lossy(&payload).into_owned());
                        (listener.signal)("creatures/signal".to_string(), value);
                    });
                }
                None => {
                    let machine_id_owned = machine_id.clone();
                    let store_id = packet.store_id.clone();
                    let trans = self.clone();
                    thread::spawn(move || {
                        let data = String::from_utf8_lossy(&payload).into_owned();
                        trans.tools().workloads().run_vm_entity(
                            &machine_id_owned,
                            &store_id,
                            &data,
                            "",
                        );
                    });
                }
            }
        }
    }

    fn consume_pay_lock_on_chain(self: &Arc<Self>, pay: Option<&ChainPayPacket>) -> bool {
        let Some(pay) = pay else {
            return false;
        };
        if pay.lock_id.is_empty()
            || pay.user_id.is_empty()
            || pay.lock_signature.is_empty()
            || pay.amount <= 0
        {
            return false;
        }
        let Some(globe) = self.globe.get().cloned() else {
            return false;
        };
        let input = ConsumeLockInput {
            typ: "pay".to_string(),
            user_id: pay.user_id.clone(),
            lock_id: pay.lock_id.clone(),
            signature: pay.lock_signature.clone(),
            amount: pay.amount,
            step: None,
        };
        let inp = serde_json::to_vec(&input).unwrap_or_default();
        let sign = self.sign_packet_as_owner(&inp);
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        let owner = self.owner_id.clone();
        let cb: crate::transports::chain::callbacks::BaseResponseCallback = Box::new(
            move |_data: Vec<u8>, status: i64, err: Option<anyhow::Error>| {
                let ok = err.is_none() && status < 400;
                let _ = tx.send(ok);
            },
        );
        globe.send_base_request_on_chain("/creatures/consumeLock", inp, &sign, &owner, "", cb);
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or(false)
    }

    pub(crate) fn handle_chain_packet(self: &Arc<Self>, typ: &str, trx_payload: &[u8]) -> String {
        // Stake and election are owned entirely by the consensus
        // provider's application handler; they never reach the chain module.
        // Only request/response/message traffic is routed here.
        match typ {
            "message" => {
                let packet: ChainMessage = match serde_json::from_slice(trx_payload) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("handle_chain_packet: bad message: {}", e);
                        return String::new();
                    }
                };
                let mut packet = packet;
                if packet.message_type.is_empty() {
                    packet.message_type = "vm.execute".to_string();
                }
                // A reply answers a message its sender waits on; this node sends
                // none that wait, so replies addressed here carry nothing to do.
                if packet.reply_to.is_empty() && self.chain_message_targets_local(&packet) {
                    match packet.message_type.as_str() {
                        "vm.cost.negotiate" => {
                            if packet.author == self.id {
                                return String::new();
                            }
                            let cost_per_second = self.finance.costs().execution_per_second;
                            let pay = ChainPayPacket {
                                typ: "vm.cost.ack".to_string(),
                                session_id: packet.request_id.clone(),
                                cost_per_second,
                                ..Default::default()
                            };
                            let key = packet.key.clone();
                            let submitter = packet.submitter.clone();
                            let mut receivers: HashMap<String, HashMap<String, bool>> =
                                HashMap::new();
                            receivers.insert(submitter.clone(), HashMap::new());
                            let id = self.id.clone();
                            let signed = self.sign_packet(cost_per_second.to_string().as_bytes());
                            let trans = self.clone();
                            thread::spawn(move || {
                                let reply = ChainMessage {
                                    key,
                                    message_type: "vm.cost.ack".to_string(),
                                    reply_to: packet.request_id.clone(),
                                    recievers: receivers,
                                    signatures: vec![signed],
                                    submitter: id.clone(),
                                    request_id: secure_unique_string(),
                                    author: id,
                                    pay: Some(pay),
                                    ..ChainMessage::default()
                                };
                                trans.submit_chain_op("main", ChainPacketOp::Message(reply));
                            });
                        }
                        "vm.execute.request" | "vm.execute.charge" | "vm.execute" => {
                            if matches!(
                                packet.message_type.as_str(),
                                "vm.execute.request" | "vm.execute.charge"
                            ) && let Some(pay) = packet.pay.as_ref()
                            {
                                let free = self.finance.is_free_node(&packet.submitter);
                                if !free && !self.consume_pay_lock_on_chain(Some(pay)) {
                                    return String::new();
                                }
                                let mut packet_cpy = packet.clone();
                                let cps = self.finance.costs().execution_per_second;
                                if let Some(p) = packet_cpy.pay.as_mut()
                                    && p.accepted_seconds <= 0
                                    && cps > 0
                                {
                                    p.accepted_seconds = p.amount / cps;
                                }
                                let trans = self.clone();
                                thread::spawn(move || {
                                    trans.run_chain_message(packet_cpy);
                                });
                                return String::new();
                            }
                            self.run_chain_message(packet);
                        }
                        _ => {}
                    }
                }
                String::new()
            }
            "base" => {
                let packet: ChainBaseRequest = match serde_json::from_slice(trx_payload) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("handle_chain_packet: bad base: {}", e);
                        return String::new();
                    }
                };
                // Only the submitting node holds a callback for its own base
                // request (registered in `send_base_request_on_chain`) and
                // reaps it below once it processes the committed request. On any
                // other node this `or_insert_with` used to park a no-op callback
                // that nothing ever removed — one leaked entry per base request
                // routed through the node, unbounded over its lifetime.
                if packet.submitter == self.id {
                    self.tools()
                        .network()
                        .chain()
                        .park_chain_callback(&packet.request_id);
                }
                let user_id = packet
                    .author
                    .strip_prefix("user::")
                    .unwrap_or("")
                    .to_string();
                let signature = packet.signatures.get(1).cloned().unwrap_or_default();
                let res = self.router().run_ordered(
                    &packet.key,
                    &crate::actions::guard::SignedPacket {
                        user_id: &user_id,
                        payload: &packet.payload,
                        signature: &signature,
                    },
                    &packet.submitter,
                );
                if packet.submitter == self.id {
                    let cb = self
                        .tools()
                        .network()
                        .chain()
                        .take_chain_callback(&packet.request_id);
                    if let Some(cb) = cb {
                        match res {
                            Ok(value) => {
                                let data = serde_json::to_vec(&value).unwrap_or_default();
                                (cb.fn_)(data, 0, None);
                            }
                            Err(refusal) => {
                                (cb.fn_)(
                                    b"{}".to_vec(),
                                    500,
                                    Some(anyhow::anyhow!(refusal.message())),
                                );
                            }
                        }
                    }
                }
                String::new()
            }
            _ => String::new(),
        }
    }

    pub(crate) fn submit_chain_op(&self, chain_id: &str, op: ChainPacketOp) {
        self.tools().network().chain().submit_chain_op(chain_id, op);
    }
}
