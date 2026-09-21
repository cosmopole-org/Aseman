//! ADR 0018 legacy store membership: paired `hasaccess`/`onaccess` links become typed
//! `core.store_membership` capsules with exact permission sets and resolved principals.

use super::*;

const LEGACY_PERMISSIONS: [&str; 3] = ["read", "signal", "manage"];

/// `true` when a `link::` family is a legacy membership link (ADR 0018).
#[must_use]
pub fn is_legacy_membership_link_family(family: &str) -> bool {
    matches!(family, "hasaccess" | "onaccess")
}

/// Canonicalize a legacy permission set without widening or dropping any grant.
///
/// The pre-permission literal `true` and the empty value both parse to the empty set,
/// which legacy authorization treats as deny-all.
pub fn canonical_legacy_permissions(raw: &str) -> LegacyMigrationResult<String> {
    if raw.is_empty() || raw == "true" {
        return Ok(String::new());
    }
    let mut granted = [false; 3];
    for token in raw.split(',').map(str::trim) {
        let index = LEGACY_PERMISSIONS
            .iter()
            .position(|known| *known == token)
            .ok_or_else(|| {
                LegacyMigrationError::Invalid(format!(
                    "legacy permission set {raw:?} contains unknown token {token:?}"
                ))
            })?;
        if std::mem::replace(&mut granted[index], true) {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy permission set {raw:?} repeats {token:?}"
            )));
        }
    }
    Ok(LEGACY_PERMISSIONS
        .iter()
        .zip(granted)
        .filter_map(|(name, granted)| granted.then_some(*name))
        .collect::<Vec<_>>()
        .join(","))
}

impl LegacySnapshotGraph {
    /// Export every paired legacy membership (ADR 0018).
    pub(crate) fn transform_legacy_memberships(
        &self,
        migration_time_micros: i64,
        local_origins: &BTreeSet<String>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut permissions = BTreeMap::new();
        let mut flags = BTreeSet::new();
        for (key, value) in &self.links {
            let pair = |prefix: &str| {
                key.strip_prefix(prefix).and_then(|rest| {
                    rest.split_once("::")
                        .filter(|(left, right)| !left.is_empty() && !right.is_empty())
                })
            };
            if let Some((store, member)) = pair("onaccess::") {
                let raw = String::from_utf8(value.clone()).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy link {key} is not UTF-8"))
                })?;
                permissions.insert((store.to_owned(), member.to_owned()), raw);
            } else if let Some((member, store)) = pair("hasaccess::") {
                if value != b"true" {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy link {key} does not hold the literal flag `true`"
                    )));
                }
                flags.insert((store.to_owned(), member.to_owned()));
            } else if key.starts_with("onaccess::") || key.starts_with("hasaccess::") {
                return Err(LegacyMigrationError::Invalid(format!(
                    "malformed legacy membership link {key}"
                )));
            }
        }
        if let Some((store, member)) = flags.iter().find(|pair| !permissions.contains_key(*pair)) {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy membership {store}::{member} has hasaccess without onaccess"
            )));
        }
        let mut capsules = Vec::with_capacity(permissions.len());
        for ((store, member), raw) in &permissions {
            if !flags.contains(&(store.clone(), member.clone())) {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy membership {store}::{member} has onaccess without hasaccess"
                )));
            }
            self.object("Store", store)?;
            let creator = self.resolve_store_creator(store)?;
            let (member_kind, member_relationship) = self.resolve_member(member, local_origins)?;
            let mut relationships = vec![CapsuleRelationship {
                name: "store".to_owned(),
                target_kind: CapsuleKind("core.store".to_owned()),
                target_id: CapsuleId(deterministic_legacy_capsule_id("Store", store.as_bytes())),
            }];
            relationships.extend(member_relationship);
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "StoreMembership",
                    kind: "core.store_membership",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "StoreMembership",
                        &creator,
                    )?),
                    migration_time_micros,
                },
                &format!("{store}\0{member}"),
                relationships,
                BTreeMap::from([
                    (
                        "member_kind".to_owned(),
                        CapsuleValue::Text(member_kind.to_owned()),
                    ),
                    ("member_ref".to_owned(), CapsuleValue::Text(member.clone())),
                    (
                        "permissions".to_owned(),
                        CapsuleValue::Text(canonical_legacy_permissions(raw)?),
                    ),
                    // Legacy never recorded join time; zero means unknown (ADR 0018).
                    ("joined_at_micros".to_owned(), CapsuleValue::Integer(0)),
                ]),
            )?);
        }
        Ok(capsules)
    }

    fn resolve_member(
        &self,
        member: &str,
        local_origins: &BTreeSet<String>,
    ) -> LegacyMigrationResult<(&'static str, Option<CapsuleRelationship>)> {
        let local = |family: &str, name: &str, kind: &str| CapsuleRelationship {
            name: name.to_owned(),
            target_kind: CapsuleKind(kind.to_owned()),
            target_id: CapsuleId(deterministic_legacy_capsule_id(family, member.as_bytes())),
        };
        if self
            .objects
            .contains_key(&("Creature".to_owned(), member.to_owned()))
        {
            return Ok((
                "creature",
                Some(local("Creature", "creature", "core.creature")),
            ));
        }
        if self
            .objects
            .contains_key(&("Program".to_owned(), member.to_owned()))
        {
            return Ok(("program", Some(local("Program", "program", "core.program"))));
        }
        let origin = member
            .rsplit_once('@')
            .filter(|(local_part, origin)| !local_part.is_empty() && !origin.is_empty())
            .map(|(_, origin)| origin)
            .ok_or_else(|| {
                LegacyMigrationError::Invalid(format!(
                    "legacy store member {member} is neither local nor an origin-qualified identity"
                ))
            })?;
        if local_origins.is_empty() {
            return Err(LegacyMigrationError::Unmapped {
                family: "StoreMembership.local_origins".to_owned(),
                key: member.to_owned(),
            });
        }
        if local_origins.contains(origin) {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy store member {member} has a local origin but no local creature or program"
            )));
        }
        Ok(("remote_principal", None))
    }
}
