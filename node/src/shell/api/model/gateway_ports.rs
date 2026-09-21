//! Legacy adapter for gateway routes: the node transaction behind [`GatewayRoutes`]
//! (RL-004 strangler). Key encodings are exactly the legacy `vmHttpRoute` target, its
//! `vmHttpRouteFor` reverse link, and the `vmHttpRouteUser` alias, as owned by
//! `aseman_contracts::legacy_gateway`.

use aseman_contracts::legacy_gateway::{
    decode_target, encode_target, route_alias_link_key, route_link_key, route_rev_link_key,
};
use aseman_domain::gateway::GatewayRoute;
use aseman_ports::{GatewayRoutes, PortResult};

use crate::models::transaction::ITrx;

/// The legacy adapter: `vmHttpRoute`, `vmHttpRouteFor`, and `vmHttpRouteUser` links.
struct LegacyGatewayRoutes<'a> {
    trx: &'a dyn ITrx,
}

impl GatewayRoutes for LegacyGatewayRoutes<'_> {
    fn route(&self, creature_id: &str, path: &str) -> PortResult<Option<GatewayRoute>> {
        let stored = self.trx.get_link(&route_link_key(creature_id, path));
        Ok(decode_target(&stored, &[]).map(|target| GatewayRoute {
            creature_id: creature_id.to_owned(),
            path: path.to_owned(),
            program_id: target.program_id,
            entity_id: target.entity_id,
            runtime: target.runtime,
            pinned_vm_id: target.vm_id,
        }))
    }

    fn route_of_entity(
        &self,
        program_id: &str,
        entity_id: &str,
    ) -> PortResult<Option<(String, String)>> {
        let stored = self
            .trx
            .get_link(&route_rev_link_key(program_id, entity_id));
        Ok(stored
            .split_once("::")
            .map(|(creature, path)| (creature.to_owned(), path.to_owned())))
    }

    fn put_route(&self, route: &GatewayRoute) -> PortResult<()> {
        self.trx.put_link(
            &route_link_key(&route.creature_id, &route.path),
            &encode_target(
                &route.program_id,
                &route.entity_id,
                &route.pinned_vm_id,
                &route.runtime,
            ),
        );
        self.trx.put_link(
            &route_rev_link_key(&route.program_id, &route.entity_id),
            &[route.creature_id.as_str(), route.path.as_str()].join("::"),
        );
        Ok(())
    }

    fn delete_route(&self, creature_id: &str, path: &str) -> PortResult<()> {
        let Some(route) = self.route(creature_id, path)? else {
            return Ok(());
        };
        self.trx
            .del_key(&["link::", &route_link_key(creature_id, path)].concat());
        if self.route_of_entity(&route.program_id, &route.entity_id)?
            == Some((creature_id.to_owned(), path.to_owned()))
        {
            self.trx.del_key(
                &[
                    "link::",
                    &route_rev_link_key(&route.program_id, &route.entity_id),
                ]
                .concat(),
            );
        }
        Ok(())
    }

    fn alias(&self, local_part: &str) -> PortResult<Option<String>> {
        let creature = self.trx.get_link(&route_alias_link_key(local_part));
        Ok((!creature.is_empty()).then_some(creature))
    }

    fn put_alias(&self, local_part: &str, creature_id: &str) -> PortResult<()> {
        self.trx
            .put_link(&route_alias_link_key(local_part), creature_id);
        Ok(())
    }
}

/// The gateway routes of one state action, routed per ADR 0026.
pub(crate) struct GatewayPorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {
        match crate::shell::api::model::core_storage::current_unit() {
            Some(unit) => {
                let $ports = aseman_capsule_repositories::gateway::CapsuleGatewayRoutes {
                    repository: &*unit,
                };
                $call
            }
            None => {
                let $ports = LegacyGatewayRoutes { trx: $self.trx };
                $call
            }
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::core::actor::model::trx::TrxWrapper;
    use crate::models::ports::storage::IStorage;
    use std::sync::Arc;

    #[test]
    fn legacy_gateway_routes_pass_the_conformance_suite() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let routes = LegacyGatewayRoutes { trx: &*trx };
        aseman_ports::conformance::gateway_routes(&routes, "1@global", "alice", "10@global");
        // A pin is kept by the legacy provider.
        let pinned = GatewayRoute {
            creature_id: "1@global".into(),
            path: "pinned".into(),
            program_id: "10@global".into(),
            entity_id: "main".into(),
            runtime: "wasm".into(),
            pinned_vm_id: "vm-7".into(),
        };
        routes.put_route(&pinned).unwrap();
        assert_eq!(routes.route("1@global", "pinned"), Ok(Some(pinned)));
    }
}
