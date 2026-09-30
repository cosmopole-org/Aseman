//! The gateway routes of one state action (ADR 0036), through the capsule
//! repository over the action's transaction.

use aseman_domain::gateway::GatewayRoute;
use aseman_ports::{GatewayRoutes, PortResult};

use crate::util::Trx;

/// The gateway routes of one state action.
pub struct GatewayPorts<'a> {
    pub trx: &'a Trx,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {{
        let $ports = aseman_capsule::gateway::CapsuleGatewayRoutes {
            repository: $self.trx,
        };
        $call
    }};
}

impl GatewayRoutes for GatewayPorts<'_> {
    fn route(&self, creature_id: &str, path: &str) -> PortResult<Option<GatewayRoute>> {
        route!(self, |ports| ports.route(creature_id, path))
    }
    fn route_of_entity(
        &self,
        program_id: &str,
        entity_id: &str,
    ) -> PortResult<Option<(String, String)>> {
        route!(self, |ports| ports.route_of_entity(program_id, entity_id))
    }
    fn put_route(&self, route: &GatewayRoute) -> PortResult<()> {
        route!(self, |ports| ports.put_route(route))
    }
    fn delete_route(&self, creature_id: &str, path: &str) -> PortResult<()> {
        route!(self, |ports| ports.delete_route(creature_id, path))
    }
    fn alias(&self, local_part: &str) -> PortResult<Option<String>> {
        route!(self, |ports| ports.alias(local_part))
    }
    fn put_alias(&self, local_part: &str, creature_id: &str) -> PortResult<()> {
        route!(self, |ports| ports.put_alias(local_part, creature_id))
    }
}