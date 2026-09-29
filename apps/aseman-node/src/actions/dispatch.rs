//! Signed-packet requests (TCP, WebSocket, a guest's `execShellAction`, and the
//! chain and federation transports) to the router.
//!
//! Where a packet runs follows its operation's origin: on this node; ordered on
//! the main chain and run by every node against its own state; or, when its
//! `origin` names another node, on that node over federation. The node that runs
//! it authenticates the packet with the operation's guard ([`super::guard`]) and
//! authorizes it (A402) before the handler runs.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::{Map, Value};

use super::guard::{self, Entry, SignedPacket};
use super::{Operation, Origin, Router};

/// The signed-packet transports' answer codes.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// No operation at the path (code 1).
    NotFound,
    /// The payload is not the operation's input (code 2).
    Invalid(String),
    /// The guard, the policy, or the handler refused, or a remote node did (code 3).
    Failed(String),
}

impl Refusal {
    pub(crate) fn code(&self) -> i64 {
        match self {
            Self::NotFound => 1,
            Self::Invalid(_) => 2,
            Self::Failed(_) => 3,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotFound => "action not found".to_owned(),
            Self::Invalid(message) | Self::Failed(message) => message.clone(),
        }
    }
}

/// One signed packet addressed to an operation.
pub(crate) struct SignedRequest<'a> {
    pub(crate) path: &'a str,
    pub(crate) packet: SignedPacket<'a>,
}

/// How long a packet waits for the chain or a remote node to answer.
const REMOTE_ANSWER: Duration = Duration::from_secs(60);

fn parse_input(payload: &[u8]) -> Result<Value, Refusal> {
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(payload)
        .map_err(|error| Refusal::Invalid(format!("invalid input: {error}")))
}

impl Router {
    fn signed_operation(&self, path: &str) -> Result<&Operation, Refusal> {
        self.operation(path).ok_or(Refusal::NotFound)
    }

    /// Run a signed packet that reached this node from `entry`, where its
    /// operation's origin says.
    pub(crate) fn dispatch(
        &self,
        request: &SignedRequest<'_>,
        entry: Entry,
    ) -> Result<Value, Refusal> {
        let operation = self.signed_operation(request.path)?;
        let input = parse_input(request.packet.payload)?;
        let node_id = self.node().id();
        let origin = match operation.origin {
            Origin::Local => node_id.clone(),
            Origin::Replicated => "global".to_owned(),
            Origin::Requested => input
                .get("origin")
                .and_then(Value::as_str)
                .filter(|origin| !origin.is_empty())
                .map_or_else(|| node_id.clone(), str::to_owned),
        };
        if origin == "global" {
            // Every node runs an ordered packet as from inside, so it is admitted
            // here first, as from where it really came.
            guard::admit(
                self.node(),
                operation,
                &request.packet,
                &input,
                entry,
                &node_id,
            )
            .map_err(|error| Refusal::Failed(error.to_string()))?;
            return self.order_on_chain(request);
        }
        if origin == node_id {
            return self.run_signed(operation, &request.packet, &input, entry, &node_id);
        }
        if !guard::identified(self.node(), operation, &request.packet) {
            return Err(Refusal::Failed("authorization failed".to_owned()));
        }
        self.forward(&origin, request)
    }

    /// Run a packet the chain ordered, on this node, as submitted by `submitter`.
    pub(crate) fn run_ordered(
        &self,
        path: &str,
        packet: &SignedPacket<'_>,
        submitter: &str,
    ) -> Result<Value, Refusal> {
        let operation = self.signed_operation(path)?;
        let input = parse_input(packet.payload)?;
        self.run_signed(operation, packet, &input, Entry::Inside, submitter)
    }

    /// Run a packet another node forwarded here because this node is its origin.
    pub(crate) fn run_forwarded(
        &self,
        path: &str,
        packet: &SignedPacket<'_>,
    ) -> Result<Value, Refusal> {
        let operation = self.signed_operation(path)?;
        let input = parse_input(packet.payload)?;
        let source = input
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        self.run_signed(operation, packet, &input, Entry::Client, &source)
    }

    fn run_signed(
        &self,
        operation: &Operation,
        packet: &SignedPacket<'_>,
        input: &Value,
        entry: Entry,
        source: &str,
    ) -> Result<Value, Refusal> {
        let caller = guard::admit(self.node(), operation, packet, input, entry, source)
            .map_err(|error| Refusal::Failed(error.to_string()))?;
        self.execute(&caller, operation, packet.payload, true)
            .map_err(|error| match error {
                super::OperationError::Invalid(message) => Refusal::Invalid(message),
                other => Refusal::Failed(other.to_string()),
            })
    }

    /// Submit the packet to the main chain and wait for this node's run of it.
    fn order_on_chain(&self, request: &SignedRequest<'_>) -> Result<Value, Refusal> {
        let (sender, receiver) = mpsc::channel();
        self.node().globe().send_base_request_on_chain(
            request.path,
            request.packet.payload.to_vec(),
            request.packet.signature,
            request.packet.user_id,
            "",
            Box::new(move |data, _status, error| {
                let _ = sender.send(match error {
                    Some(error) => Err(error.to_string()),
                    None => Ok(answer(&data)),
                });
            }),
        );
        receiver
            .recv_timeout(REMOTE_ANSWER)
            .map_err(|_| Refusal::Failed("the chain did not answer".to_owned()))?
            .map_err(Refusal::Failed)
    }

    /// Forward the packet to its origin node and wait for that node's answer.
    fn forward(&self, origin: &str, request: &SignedRequest<'_>) -> Result<Value, Refusal> {
        let (sender, receiver) = mpsc::channel();
        self.node()
            .tools()
            .network()
            .federation()
            .send_fed_request_by_callback(
                origin,
                request.packet.user_id,
                request.path,
                request.packet.payload.to_vec(),
                request.packet.signature,
                Box::new(move |data, _status, error| {
                    let _ = sender.send(match error {
                        Some(error) => Err(error.to_string()),
                        None => serde_json::from_slice::<Value>(&data)
                            .or_else(|_| {
                                if data.is_empty() {
                                    Ok(Value::Object(Map::new()))
                                } else {
                                    Err(())
                                }
                            })
                            .map_err(|()| "the origin node answered with invalid JSON".to_owned()),
                    });
                }),
            );
        receiver
            .recv_timeout(REMOTE_ANSWER)
            .map_err(|_| Refusal::Failed(anyhow!("node {origin} did not answer").to_string()))?
            .map_err(Refusal::Failed)
    }
}

/// A remote answer's JSON body (an empty body is an empty object).
fn answer(data: &[u8]) -> Value {
    serde_json::from_slice(data).unwrap_or_else(|_| Value::Object(Map::new()))
}
