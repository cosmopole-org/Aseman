//! Gateway route repositories on the capsule protocol (RL-004 strangler, target side).
//!
//! A route is a `core.gateway_route` capsule, as the A308 export writes it: scoped to
//! its creature and related to the creature and the program. The reverse index and the
//! username aliases are derived by query. A pinned VM instance is observed runtime
//! state (ADR 0022) and is not persisted.

use crate::store::{body, next_revision, port_error};
use crate::support::{
    Capsules, MAX_CAS_ATTEMPTS, equal, failed, new_capsule, relationship, text, tombstone,
};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleKind, CapsuleQuery, CapsuleValue, MAX_QUERY_LIMIT, OwnerScope,
    QueryPredicate, StorageClass,
};
use aseman_contracts::legacy_gateway::username_local_part;
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::gateway::GatewayRoute;
use aseman_ports::{GatewayRoutes, PortResult};
use std::collections::{BTreeMap, BTreeSet};

const ROUTE: &str = "core.gateway_route";
const CREATURE: &str = "core.creature";
const PROGRAM: &str = "core.program";

/// Gateway route ports over any [`CapsuleStore`].
pub struct CapsuleGatewayRoutes<'a> {
    pub repository: &'a dyn CapsuleStore,
}

fn route_id(creature_id: &str, path: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id(
        "GatewayRoute",
        [creature_id, "::", path].concat().as_bytes(),
    )
}

fn id_of(legacy_family: &str, legacy_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id(legacy_family, legacy_id.as_bytes())
}

impl CapsuleGatewayRoutes<'_> {
    fn capsules(&self) -> Capsules<'_> {
        Capsules(self.repository)
    }

    fn query(
        &self,
        kind: &str,
        predicate: Option<QueryPredicate>,
    ) -> PortResult<Vec<CapsuleEnvelope>> {
        Ok(self
            .repository
            .query(&CapsuleQuery {
                kind: CapsuleKind(kind.to_owned()),
                predicate,
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: MAX_QUERY_LIMIT,
                cursor: None,
            })
            .map_err(port_error)?
            .into_iter()
            .filter(|capsule| !capsule.tombstone)
            .collect())
    }

    fn related(&self, capsule: &CapsuleEnvelope, name: &str, kind: &str) -> PortResult<String> {
        let target = capsule
            .relationships
            .iter()
            .find(|relationship| relationship.name == name)
            .ok_or_else(|| failed(format!("gateway route has no {name}")))?;
        self.capsules().legacy_id_of(kind, target.target_id.0)
    }
}

impl GatewayRoutes for CapsuleGatewayRoutes<'_> {
    fn route(&self, creature_id: &str, path: &str) -> PortResult<Option<GatewayRoute>> {
        let Some(capsule) = self.capsules().live(ROUTE, route_id(creature_id, path))? else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or_else(|| failed("gateway route has no body"))?;
        Ok(Some(GatewayRoute {
            creature_id: creature_id.to_owned(),
            path: path.to_owned(),
            program_id: self.related(&capsule, "program", PROGRAM)?,
            entity_id: text(fields, "entity_name"),
            runtime: text(fields, "runtime"),
            pinned_vm_id: String::new(),
        }))
    }

    fn route_of_entity(
        &self,
        program_id: &str,
        entity_id: &str,
    ) -> PortResult<Option<(String, String)>> {
        let rows = self.query(
            ROUTE,
            Some(QueryPredicate::And {
                predicates: vec![
                    equal(
                        "program",
                        CapsuleValue::Bytes(id_of("Program", program_id).to_vec()),
                    ),
                    equal("entity_name", CapsuleValue::Text(entity_id.to_owned())),
                ],
            }),
        )?;
        // Legacy's reverse link names the most recently stored route of the entity.
        let Some(latest) = rows.iter().max_by(|left, right| {
            left.updated_at_micros
                .cmp(&right.updated_at_micros)
                .then(left.revision.cmp(&right.revision))
        }) else {
            return Ok(None);
        };
        let path = body(latest)
            .map(|fields| text(fields, "path"))
            .ok_or_else(|| failed("gateway route has no body"))?;
        Ok(Some((self.related(latest, "creature", CREATURE)?, path)))
    }

    fn put_route(&self, route: &GatewayRoute) -> PortResult<()> {
        let creature = id_of("Creature", &route.creature_id);
        let fields = BTreeMap::from([
            ("path".to_owned(), CapsuleValue::Text(route.path.clone())),
            (
                "entity_name".to_owned(),
                CapsuleValue::Text(route.entity_id.clone()),
            ),
            (
                "runtime".to_owned(),
                CapsuleValue::Text(route.runtime.clone()),
            ),
        ]);
        let relationships = vec![
            relationship("creature", CREATURE, creature),
            relationship("program", PROGRAM, id_of("Program", &route.program_id)),
        ];
        let id = route_id(&route.creature_id, &route.path);
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.capsules().get(ROUTE, id)? {
                Some(current) => {
                    let next = CapsuleEnvelope {
                        relationships: relationships.clone(),
                        ..next_revision(&current, fields.clone())?
                    }
                    .seal()
                    .map_err(failed)?;
                    self.repository.put(&next, Some(current.revision))
                }
                None => self.repository.put(
                    &new_capsule(
                        id,
                        ROUTE,
                        StorageClass::Core,
                        OwnerScope::Creature(creature),
                        relationships.clone(),
                        fields.clone(),
                    )?,
                    None,
                ),
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(aseman_ports::PortError::Conflict)
    }

    fn delete_route(&self, creature_id: &str, path: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.capsules().live(ROUTE, route_id(creature_id, path))? else {
                return Ok(());
            };
            match self
                .repository
                .put(&tombstone(&current)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(aseman_ports::PortError::Conflict)
    }

    fn alias(&self, local_part: &str) -> PortResult<Option<String>> {
        // Aliases are derived: the creature whose username's local part matches,
        // first in username byte order.
        let mut matches = self
            .capsules()
            .scan(CREATURE, Vec::new(), "username")?
            .into_iter()
            .filter_map(|capsule| {
                let username = text(body(&capsule)?, "username");
                (username_local_part(&username) == local_part).then_some((username, capsule))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
        match matches.first() {
            Some((_, capsule)) => self
                .capsules()
                .legacy_id_of(CREATURE, capsule.id.0)
                .map(Some),
            None => Ok(None),
        }
    }

    fn put_alias(&self, _local_part: &str, _creature_id: &str) -> PortResult<()> {
        // Derived from the creature's username; nothing to store.
        Ok(())
    }
}
