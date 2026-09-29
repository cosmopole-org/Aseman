//! Translation of `shell/api/actions/dummy/dummy.go`.

use std::sync::Arc;

use anyhow::Result;
use aseman_application::Diagnostics;
use aseman_ports::ClockPort;
use serde_json::{Value, json};

use crate::api::packets::simple::HelloInput;
use crate::core::actor::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::state::IState;

use super::util::build_secure_action;

struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

static SYSTEM_CLOCK: SystemClock = SystemClock;

pub fn hello(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<HelloInput, _>(
        app,
        "/api/hello",
        Guard::default(),
        move |_state: Arc<dyn IState>, input: HelloInput| -> Result<Value> {
            let diagnostics = Diagnostics {
                clock: &SYSTEM_CLOCK,
                advertised_port: "",
            };
            Ok(json!({"message": diagnostics.hello(&input.name)}))
        },
    )
}

pub fn time(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<HelloInput, _>(
        app,
        "/api/time",
        Guard::default(),
        move |_state: Arc<dyn IState>, _: HelloInput| -> Result<Value> {
            let diagnostics = Diagnostics {
                clock: &SYSTEM_CLOCK,
                advertised_port: "",
            };
            Ok(json!({"time": diagnostics.time_millis()}))
        },
    )
}

/// `/api/ping`, reporting `advertised_port`: the node's configured main port.
pub fn ping(app: Arc<dyn ICore>, advertised_port: String) -> Arc<dyn ISecureAction> {
    build_secure_action::<HelloInput, _>(
        app,
        "/api/ping",
        Guard::default(),
        move |_state: Arc<dyn IState>, _: HelloInput| -> Result<Value> {
            let diagnostics = Diagnostics {
                clock: &SYSTEM_CLOCK,
                advertised_port: &advertised_port,
            };
            Ok(json!(diagnostics.ping()))
        },
    )
}

pub fn install(app: Arc<dyn ICore>, advertised_port: String) {
    let actor = app.actor();
    for a in [
        hello(app.clone()),
        time(app.clone()),
        ping(app.clone(), advertised_port),
    ] {
        actor.inject_secure_action(a);
    }
}
