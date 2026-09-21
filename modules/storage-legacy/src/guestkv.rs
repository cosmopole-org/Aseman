//! ADR 0021: legacy guest `dbOp` pairs, committed as `link::{machineId}::{guestKey}`,
//! become `guest.legacy_kv` capsules owned by the machine creature.

use super::*;

pub const LEGACY_GUEST_KV_KIND: &str = "guest.legacy_kv";

impl LegacySnapshotGraph {
    /// A link family is guest KV when it is exactly a local legacy creature ID.
    pub(crate) fn is_legacy_guest_kv_family(&self, family: &str) -> bool {
        self.objects
            .contains_key(&("Creature".to_owned(), family.to_owned()))
    }

    /// Resolve an `AppletDb::{dbPrefix}::{key}` owner: the first prefix segment is the
    /// creature ID, or the program ID when the host call had no creature.
    fn applet_db_owner(&self, rest: &str) -> LegacyMigrationResult<String> {
        let first = rest.split_once("::").map_or(rest, |(first, _)| first);
        if self.is_legacy_guest_kv_family(first) {
            return Ok(first.to_owned());
        }
        if self
            .objects
            .contains_key(&("Program".to_owned(), first.to_owned()))
        {
            return self.resolve_program_creature(first);
        }
        Err(LegacyMigrationError::Invalid(format!(
            "legacy AppletDb::{rest} names no local creature or program"
        )))
    }

    /// Transform every committed guest pair into its owning creature's guest database.
    pub(crate) fn transform_legacy_guest_kv(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        for (link, value) in &self.links {
            let Some((family, rest)) = link.split_once("::") else {
                continue;
            };
            // `namespace` keeps the two legacy key spaces distinct in one reserved table.
            let (namespace, machine, guest_key) = if family == "AppletDb" {
                ("applet_db", self.applet_db_owner(rest)?, rest)
            } else if self.is_legacy_guest_kv_family(family) {
                ("dbop", family.to_owned(), rest)
            } else {
                continue;
            };
            let machine = machine.as_str();
            let value = String::from_utf8(value.clone()).map_err(|_| {
                LegacyMigrationError::Invalid(format!(
                    "legacy guest value for {machine}::{guest_key} is not UTF-8"
                ))
            })?;
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "GuestLegacyKv",
                    kind: LEGACY_GUEST_KV_KIND,
                    storage_class: StorageClass::GuestData,
                    owner_scope: OwnerScope::Creature(deterministic_legacy_capsule_id(
                        "Creature",
                        machine.as_bytes(),
                    )),
                    migration_time_micros,
                },
                &format!("{namespace}\0{machine}\0{guest_key}"),
                Vec::new(),
                BTreeMap::from([
                    (
                        "namespace".to_owned(),
                        CapsuleValue::Text(namespace.to_owned()),
                    ),
                    ("key".to_owned(), CapsuleValue::Text(guest_key.to_owned())),
                    ("value".to_owned(), CapsuleValue::Text(value)),
                ]),
            )?);
        }
        Ok(capsules)
    }
}
