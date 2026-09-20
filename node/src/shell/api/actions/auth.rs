//! Translation of `shell/api/actions/auth/auth.go`.

use std::sync::Arc;

use anyhow::Result;
use aseman_application::{GetServerPeers, GetServerPublicKey};
use aseman_ports::{PeerDirectoryPort, PortResult, ServerIdentityPort};
use serde_json::{json, Value};

use crate::core::actor::model::secured::guard::Guard;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::state::IState;
use crate::shell::api::packets::auth::{GetServerKeyInput, GetServersMapInput};
use crate::shell::api::packets::auth::{GetServerKeyOutput, GetServersMapOutput};

use super::util::build_secure_action;

struct LegacyAuthPorts {
    app: Arc<dyn ICore>,
}

impl ServerIdentityPort for LegacyAuthPorts {
    fn server_public_key(&self) -> PortResult<String> {
        let pair = self.app.tools().security().fetch_key_pair("server_key");
        Ok(pair
            .get(1)
            .map(|key| String::from_utf8_lossy(key).into_owned())
            .unwrap_or_default())
    }
}

impl PeerDirectoryPort for LegacyAuthPorts {
    fn peer_servers(&self) -> PortResult<Vec<String>> {
        Ok(self.app.tools().network().chain().peers())
    }
}

pub fn get_server_public_key(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let ports = Arc::new(LegacyAuthPorts { app: app.clone() });
    build_secure_action::<GetServerKeyInput, _>(
        app,
        "/auths/getServerPublicKey",
        Guard::default(),
        move |_state: Arc<dyn IState>, _: GetServerKeyInput| -> Result<Value> {
            let public_key = GetServerPublicKey {
                identity: ports.as_ref(),
            }
            .execute()?;
            Ok(json!(GetServerKeyOutput { public_key }))
        },
    )
}

pub fn get_servers_map(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let ports = Arc::new(LegacyAuthPorts { app: app.clone() });
    build_secure_action::<GetServersMapInput, _>(
        app,
        "/auths/getServersMap",
        Guard::default(),
        move |_state: Arc<dyn IState>, _: GetServersMapInput| -> Result<Value> {
            let servers = GetServerPeers {
                peers: ports.as_ref(),
            }
            .execute()?;
            Ok(json!(GetServersMapOutput { servers }))
        },
    )
}

/// Plug every auth action into the actor.
pub fn install(app: Arc<dyn ICore>) {
    let actor = app.actor();
    let actions: Vec<Arc<dyn ISecureAction>> = vec![
        get_server_public_key(app.clone()),
        get_servers_map(app.clone()),
    ];
    for a in actions {
        actor.inject_secure_action(a);
    }
}
