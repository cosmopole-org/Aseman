//! ADR 0022: durable VM intent (gateway routes and pending program alarms) migrates as
//! typed capsules; derived route links are verified and never exported.

use super::*;

const ROUTE: &str = "vmHttpRoute";
const ROUTE_FOR: &str = "vmHttpRouteFor";
const ROUTE_USER: &str = "vmHttpRouteUser";
const ALARM_FIELDS: [&str; 4] = [
    "vmAlarmStoreId",
    "vmAlarmTime",
    "vmAlarmData",
    "vmAlarmEntity",
];

/// `true` for a link family that ADR 0022 migrates or verifies as durable VM intent.
#[must_use]
pub fn is_legacy_vm_intent_link_family(family: &str) -> bool {
    matches!(family, ROUTE | ROUTE_FOR | ROUTE_USER) || ALARM_FIELDS.contains(&family)
}

impl LegacySnapshotGraph {
    fn link_text(&self, key: &str) -> LegacyMigrationResult<Option<String>> {
        self.links
            .get(key)
            .map(|value| {
                String::from_utf8(value.clone()).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy link {key} is not UTF-8"))
                })
            })
            .transpose()
    }

    /// Gateway routes: `vmHttpRoute::{creature}::{path}` holds the target JSON; its
    /// reverse link and username alias must agree exactly.
    pub(crate) fn transform_legacy_gateway_routes(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let invalid = |key: &str, detail: &str| {
            LegacyMigrationError::Invalid(format!("legacy gateway route {key} diverges: {detail}"))
        };
        let mut capsules = Vec::new();
        let mut reverse_expected = BTreeMap::new();
        for (key, value) in &self.links {
            let Some(rest) = key.strip_prefix("vmHttpRoute::") else {
                continue;
            };
            let (creature, path) = rest
                .split_once("::")
                .filter(|(creature, path)| !creature.is_empty() && !path.is_empty())
                .ok_or_else(|| invalid(key, "malformed route key"))?;
            let target: Value =
                serde_json::from_slice(value).map_err(|_| invalid(key, "target is not JSON"))?;
            let field = |name: &str| target.get(name).and_then(Value::as_str).unwrap_or("");
            let (program, entity, runtime) =
                (field("programId"), field("entityId"), field("runtime"));
            let Value::Object(members) = &target else {
                return Err(invalid(key, "target is not an object"));
            };
            if program.is_empty()
                || entity.is_empty()
                || runtime.is_empty()
                || members.keys().any(|name| {
                    !matches!(name.as_str(), "programId" | "entityId" | "vmId" | "runtime")
                })
            {
                return Err(invalid(key, "target fields are incomplete or unreviewed"));
            }
            if self.resolve_program_creature(program)? != creature {
                return Err(invalid(key, "route creature does not own the program"));
            }
            self.object("Entity", &format!("{program}::{entity}"))?;
            reverse_expected.insert(
                format!("{program}::{entity}"),
                format!("{creature}::{path}"),
            );
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "GatewayRoute",
                    kind: "core.gateway_route",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(deterministic_legacy_capsule_id(
                        "Creature",
                        creature.as_bytes(),
                    )),
                    migration_time_micros,
                },
                rest,
                vec![
                    CapsuleRelationship {
                        name: "creature".to_owned(),
                        target_kind: CapsuleKind("core.creature".to_owned()),
                        target_id: CapsuleId(deterministic_legacy_capsule_id(
                            "Creature",
                            creature.as_bytes(),
                        )),
                    },
                    CapsuleRelationship {
                        name: "program".to_owned(),
                        target_kind: CapsuleKind("core.program".to_owned()),
                        target_id: CapsuleId(deterministic_legacy_capsule_id(
                            "Program",
                            program.as_bytes(),
                        )),
                    },
                ],
                // The pinned `vmId` is an observed instance (ADR 0022) and is not carried.
                BTreeMap::from([
                    ("path".to_owned(), CapsuleValue::Text(path.to_owned())),
                    (
                        "entity_name".to_owned(),
                        CapsuleValue::Text(entity.to_owned()),
                    ),
                    ("runtime".to_owned(), CapsuleValue::Text(runtime.to_owned())),
                ]),
            )?);
        }
        for (key, value) in &self.links {
            if let Some(program_entity) = key.strip_prefix("vmHttpRouteFor::") {
                let stored = self.link_text(key)?.unwrap_or_default();
                if reverse_expected.get(program_entity) != Some(&stored) {
                    return Err(invalid(key, "reverse link names no matching route"));
                }
            } else if let Some(local_part) = key.strip_prefix("vmHttpRouteUser::") {
                let creature = String::from_utf8(value.clone()).unwrap_or_default();
                let username = required_utf8_column(
                    "Creature",
                    self.object("Creature", &creature)?,
                    "username",
                )?;
                if username.split('@').next().unwrap_or(&username) != local_part {
                    return Err(invalid(key, "alias differs from the creature's username"));
                }
            }
        }
        if reverse_expected.len()
            != self
                .links
                .keys()
                .filter(|key| key.starts_with("vmHttpRouteFor::"))
                .count()
        {
            return Err(LegacyMigrationError::Invalid(
                "legacy gateway route is missing its vmHttpRouteFor reverse link".to_owned(),
            ));
        }
        Ok(capsules)
    }

    /// Pending program alarms replayed by the legacy bootstrap (one per program).
    pub(crate) fn transform_legacy_program_alarms(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let subjects: BTreeSet<&str> = self
            .links
            .keys()
            .filter_map(|key| {
                let (family, subject) = key.split_once("::")?;
                ALARM_FIELDS.contains(&family).then_some(subject)
            })
            .collect();
        let mut capsules = Vec::with_capacity(subjects.len());
        for program in subjects {
            let invalid = |detail: &str| {
                LegacyMigrationError::Invalid(format!("legacy alarm for {program} {detail}"))
            };
            if !self
                .objects
                .contains_key(&("Program".to_owned(), program.to_owned()))
            {
                return Err(invalid(
                    "is not a program; the legacy bootstrap never replays it",
                ));
            }
            let store = self
                .link_text(&format!("vmAlarmStoreId::{program}"))?
                .filter(|store| !store.is_empty())
                .ok_or_else(|| invalid("has no store"))?;
            self.object("Store", &store)?;
            let fire_at_millis = self
                .link_text(&format!("vmAlarmTime::{program}"))?
                .and_then(|time| time.parse::<i64>().ok())
                .filter(|time| *time >= 0)
                .ok_or_else(|| invalid("has no valid fire time"))?;
            let fire_at_micros = fire_at_millis
                .checked_mul(1_000)
                .ok_or_else(|| invalid("fire time overflows microseconds"))?;
            let data = self
                .link_text(&format!("vmAlarmData::{program}"))?
                .ok_or_else(|| invalid("has no data"))?;
            // Older alarms without an entity replay as `main`, exactly like legacy.
            let entity = self
                .link_text(&format!("vmAlarmEntity::{program}"))?
                .filter(|entity| !entity.is_empty())
                .unwrap_or_else(|| "main".to_owned());
            let creature = self.resolve_program_creature(program)?;
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "ProgramAlarm",
                    kind: "core.program_alarm",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "ProgramAlarm",
                        &creature,
                    )?),
                    migration_time_micros,
                },
                program,
                vec![
                    CapsuleRelationship {
                        name: "program".to_owned(),
                        target_kind: CapsuleKind("core.program".to_owned()),
                        target_id: CapsuleId(deterministic_legacy_capsule_id(
                            "Program",
                            program.as_bytes(),
                        )),
                    },
                    CapsuleRelationship {
                        name: "store".to_owned(),
                        target_kind: CapsuleKind("core.store".to_owned()),
                        target_id: CapsuleId(deterministic_legacy_capsule_id(
                            "Store",
                            store.as_bytes(),
                        )),
                    },
                ],
                BTreeMap::from([
                    (
                        "fire_at_micros".to_owned(),
                        CapsuleValue::Integer(fire_at_micros),
                    ),
                    ("entity_name".to_owned(), CapsuleValue::Text(entity)),
                    ("data".to_owned(), CapsuleValue::Text(data)),
                ]),
            )?);
        }
        Ok(capsules)
    }
}
