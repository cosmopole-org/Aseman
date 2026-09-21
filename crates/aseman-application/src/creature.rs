//! Creature identity use cases (RL-004 strangler slice: `/creatures/create`, `get`,
//! `list`, `update`, `delete`, `getByUsername`, `find`). The rules and client-visible
//! error texts are identical to the legacy actions. Sessions, metadata documents, and
//! memberships stay with the adapter until their own slices.

use crate::ApplicationError;
use aseman_domain::creature::{CreatureRecord, HUMAN_OWNER};
use aseman_ports::{CreatureBalances, CreatureDirectory, PortError};

/// The identity that may act on any creature.
pub const ROOT_CREATURE: &str = "1@global";

fn denied(message: &str) -> ApplicationError {
    ApplicationError::Denied(message.to_owned())
}

/// A creature as the legacy wire shows it: identity plus balance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatureView {
    pub record: CreatureRecord,
    pub balance: i64,
}

fn view(
    balances: &dyn CreatureBalances,
    record: CreatureRecord,
) -> Result<CreatureView, ApplicationError> {
    let balance = match balances.balance(&record.id) {
        Err(PortError::NotFound) => 0,
        other => other?,
    };
    Ok(CreatureView { record, balance })
}

/// What `/creatures/create` asks for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NewCreature {
    /// The server-generated identity.
    pub id: String,
    pub creature_type: String,
    /// The requested name; the origin is appended as `{name}@{origin}`.
    pub name: String,
    pub origin: String,
    pub public_key: String,
    pub chain_id: Option<String>,
    pub subchain_id: Option<String>,
    pub owner_id: Option<String>,
    /// The authenticated caller, who owns a non-human creature by default.
    pub caller_id: String,
    pub opening_balance: i64,
}

pub struct CreateCreature<'a> {
    pub directory: &'a dyn CreatureDirectory,
    pub balances: &'a dyn CreatureBalances,
}

impl CreateCreature<'_> {
    /// Humans always live on `main`/`main` and own themselves; any other creature
    /// is owned by the named owner, else by the caller.
    pub fn execute(&self, request: NewCreature) -> Result<CreatureView, ApplicationError> {
        let non_empty = |value: Option<String>, default: &str| {
            value
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        let mut record = CreatureRecord {
            id: request.id,
            username: format!("{}@{}", request.name, request.origin),
            public_key: request.public_key,
            chain_id: non_empty(request.chain_id, "main"),
            subchain_id: non_empty(request.subchain_id, "main"),
            owner_id: non_empty(request.owner_id, HUMAN_OWNER),
            creature_type: request.creature_type,
        };
        if record.is_human() {
            record.chain_id = "main".to_owned();
            record.subchain_id = "main".to_owned();
            record.owner_id = HUMAN_OWNER.to_owned();
        } else if record.owner_id == HUMAN_OWNER {
            record.owner_id = request.caller_id;
        }
        if self
            .directory
            .creature_id_by_username(&record.username)?
            .is_some()
        {
            return Err(denied("creature username already exists"));
        }
        match self.directory.create(&record) {
            Err(PortError::Conflict) => Err(denied("creature username already exists")),
            other => other.map_err(ApplicationError::from),
        }?;
        self.balances.open(&record.id, request.opening_balance)?;
        Ok(CreatureView {
            record,
            balance: request.opening_balance,
        })
    }
}

pub struct GetCreature<'a> {
    pub directory: &'a dyn CreatureDirectory,
    pub balances: &'a dyn CreatureBalances,
}

impl GetCreature<'_> {
    pub fn by_id(&self, creature_id: &str) -> Result<CreatureView, ApplicationError> {
        let record = self
            .directory
            .creature(creature_id)?
            .ok_or_else(|| denied("creature not found"))?;
        view(self.balances, record)
    }

    pub fn by_username(&self, username: &str) -> Result<CreatureView, ApplicationError> {
        let record = self
            .directory
            .creature_id_by_username(username)?
            .map(|id| self.directory.creature(&id))
            .transpose()?
            .flatten()
            .ok_or_else(|| denied("user not found"))?;
        view(self.balances, record)
    }

    pub fn by_username_fragment(&self, fragment: &str) -> Result<CreatureView, ApplicationError> {
        let record = self
            .directory
            .find_by_username_fragment(fragment)?
            .ok_or_else(|| denied("user not found"))?;
        view(self.balances, record)
    }

    pub fn list(
        &self,
        creature_type: Option<&str>,
        offset: i64,
        count: Option<i64>,
    ) -> Result<Vec<CreatureView>, ApplicationError> {
        self.directory
            .creatures(creature_type, offset, count)?
            .into_iter()
            .map(|record| view(self.balances, record))
            .collect()
    }
}

/// The optional fields of `/creatures/update`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CreaturePatch {
    pub public_key: Option<String>,
    pub creature_type: Option<String>,
    /// A new name; the caller's origin is appended.
    pub name: Option<String>,
}

fn authorize(caller_id: &str, creature_id: &str) -> Result<(), ApplicationError> {
    if creature_id != caller_id && caller_id != ROOT_CREATURE {
        return Err(denied("access denied"));
    }
    Ok(())
}

pub struct UpdateCreature<'a> {
    pub directory: &'a dyn CreatureDirectory,
}

impl UpdateCreature<'_> {
    pub fn execute(
        &self,
        caller_id: &str,
        creature_id: &str,
        origin: &str,
        patch: CreaturePatch,
    ) -> Result<CreatureRecord, ApplicationError> {
        authorize(caller_id, creature_id)?;
        let mut record = self
            .directory
            .creature(creature_id)?
            .ok_or_else(|| denied("user not found"))?;
        if let Some(public_key) = patch.public_key {
            record.public_key = public_key;
        }
        if let Some(creature_type) = patch.creature_type {
            record.creature_type = creature_type;
        }
        if let Some(name) = patch.name {
            let current = record.username.split('@').next().unwrap_or_default();
            if name != current {
                let next = format!("{name}@{origin}");
                if self.directory.creature_id_by_username(&next)?.is_some() {
                    return Err(denied("username already exists"));
                }
                record.username = next;
            }
        }
        match self.directory.update(&record) {
            Err(PortError::Conflict) => Err(denied("username already exists")),
            Err(PortError::NotFound) => Err(denied("user not found")),
            other => other.map_err(ApplicationError::from),
        }?;
        Ok(record)
    }
}

pub struct DeleteCreature<'a> {
    pub directory: &'a dyn CreatureDirectory,
    pub balances: &'a dyn CreatureBalances,
}

impl DeleteCreature<'_> {
    /// Removes the identity record; the adapter then clears the creature's sessions,
    /// metadata, and memberships.
    pub fn execute(&self, caller_id: &str, creature_id: &str) -> Result<(), ApplicationError> {
        authorize(caller_id, creature_id)?;
        if self.directory.creature(creature_id)?.is_none() {
            return Err(denied("user not found"));
        }
        self.directory.delete(creature_id)?;
        self.balances.close(creature_id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_ports::PortResult;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        creatures: Mutex<BTreeMap<String, (CreatureRecord, i64)>>,
    }

    impl CreatureDirectory for Memory {
        fn creature(&self, id: &str) -> PortResult<Option<CreatureRecord>> {
            Ok(self
                .creatures
                .lock()
                .unwrap()
                .get(id)
                .map(|entry| entry.0.clone()))
        }
        fn creature_id_by_username(&self, username: &str) -> PortResult<Option<String>> {
            Ok(self
                .creatures
                .lock()
                .unwrap()
                .values()
                .find(|entry| entry.0.username == username)
                .map(|entry| entry.0.id.clone()))
        }
        fn find_by_username_fragment(&self, fragment: &str) -> PortResult<Option<CreatureRecord>> {
            let creatures = self.creatures.lock().unwrap();
            let mut found = creatures
                .values()
                .filter(|entry| entry.0.username.contains(fragment))
                .map(|entry| entry.0.clone())
                .collect::<Vec<_>>();
            found.sort_by(|left, right| left.username.cmp(&right.username));
            Ok(found.into_iter().next())
        }
        fn creatures(
            &self,
            creature_type: Option<&str>,
            offset: i64,
            count: Option<i64>,
        ) -> PortResult<Vec<CreatureRecord>> {
            let creatures = self.creatures.lock().unwrap();
            Ok(aseman_domain::creature::legacy_page(
                creatures
                    .values()
                    .map(|entry| entry.0.clone())
                    .filter(|record| creature_type.is_none_or(|kind| record.creature_type == kind)),
                offset,
                count,
            ))
        }
        fn create(&self, record: &CreatureRecord) -> PortResult<()> {
            if self.creature_id_by_username(&record.username)?.is_some() {
                return Err(PortError::Conflict);
            }
            self.creatures
                .lock()
                .unwrap()
                .insert(record.id.clone(), (record.clone(), 0));
            Ok(())
        }
        fn update(&self, record: &CreatureRecord) -> PortResult<()> {
            let mut creatures = self.creatures.lock().unwrap();
            let entry = creatures.get_mut(&record.id).ok_or(PortError::NotFound)?;
            entry.0 = record.clone();
            Ok(())
        }
        fn delete(&self, id: &str) -> PortResult<()> {
            self.creatures.lock().unwrap().remove(id);
            Ok(())
        }
    }

    impl CreatureBalances for Memory {
        fn open(&self, id: &str, balance: i64) -> PortResult<()> {
            self.set_balance(id, balance)
        }
        fn close(&self, _id: &str) -> PortResult<()> {
            Ok(())
        }
        fn balance(&self, id: &str) -> PortResult<i64> {
            self.creatures
                .lock()
                .unwrap()
                .get(id)
                .map(|entry| entry.1)
                .ok_or(PortError::NotFound)
        }
        fn set_balance(&self, id: &str, balance: i64) -> PortResult<()> {
            self.creatures
                .lock()
                .unwrap()
                .get_mut(id)
                .ok_or(PortError::NotFound)?
                .1 = balance;
            Ok(())
        }
    }

    fn request(id: &str, creature_type: &str, name: &str) -> NewCreature {
        NewCreature {
            id: id.to_owned(),
            creature_type: creature_type.to_owned(),
            name: name.to_owned(),
            origin: "global".to_owned(),
            public_key: "pem".to_owned(),
            caller_id: "1@global".to_owned(),
            opening_balance: 7,
            ..NewCreature::default()
        }
    }

    fn denied_text(error: ApplicationError) -> String {
        match error {
            ApplicationError::Denied(message) => message,
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    #[test]
    fn create_normalizes_humans_and_defaults_machine_owners() {
        let memory = Memory::default();
        let create = CreateCreature {
            directory: &memory,
            balances: &memory,
        };
        let human = create
            .execute(NewCreature {
                chain_id: Some("side".to_owned()),
                owner_id: Some("5@global".to_owned()),
                ..request("2@global", "human", "alice")
            })
            .unwrap();
        assert_eq!(human.record.username, "alice@global");
        assert_eq!(
            (
                human.record.chain_id.as_str(),
                human.record.owner_id.as_str()
            ),
            ("main", "free")
        );
        assert_eq!(human.balance, 7);
        let machine = create
            .execute(NewCreature {
                subchain_id: Some(String::new()),
                ..request("3@global", "machine", "bot")
            })
            .unwrap();
        assert_eq!(machine.record.owner_id, "1@global");
        assert_eq!(machine.record.subchain_id, "main");
        assert_eq!(
            denied_text(
                create
                    .execute(request("4@global", "machine", "bot"))
                    .unwrap_err()
            ),
            "creature username already exists"
        );
    }

    #[test]
    fn reads_update_and_delete_keep_legacy_rules_and_texts() {
        let memory = Memory::default();
        let create = CreateCreature {
            directory: &memory,
            balances: &memory,
        };
        create
            .execute(request("2@global", "human", "alice"))
            .unwrap();
        create
            .execute(request("3@global", "machine", "bot"))
            .unwrap();
        let get = GetCreature {
            directory: &memory,
            balances: &memory,
        };
        assert_eq!(get.by_username("bot@global").unwrap().record.id, "3@global");
        assert_eq!(
            get.by_username_fragment("bo").unwrap().record.id,
            "3@global"
        );
        assert_eq!(
            denied_text(get.by_id("9@global").unwrap_err()),
            "creature not found"
        );
        assert_eq!(
            denied_text(get.by_username("x@global").unwrap_err()),
            "user not found"
        );
        assert_eq!(get.list(Some("machine"), 0, None).unwrap().len(), 1);

        let update = UpdateCreature { directory: &memory };
        let rename = |name: &str| CreaturePatch {
            name: Some(name.to_owned()),
            ..CreaturePatch::default()
        };
        assert_eq!(
            denied_text(
                update
                    .execute("2@global", "3@global", "global", rename("x"))
                    .unwrap_err()
            ),
            "access denied"
        );
        assert_eq!(
            denied_text(
                update
                    .execute("3@global", "3@global", "global", rename("alice"))
                    .unwrap_err()
            ),
            "username already exists"
        );
        // Keeping the current name is not a rename.
        update
            .execute("3@global", "3@global", "global", rename("bot"))
            .unwrap();
        let renamed = update
            .execute(ROOT_CREATURE, "3@global", "global", rename("robot"))
            .unwrap();
        assert_eq!(renamed.username, "robot@global");
        // LD-13: a missing creature is refused, not recreated.
        assert_eq!(
            denied_text(
                update
                    .execute(ROOT_CREATURE, "9@global", "global", rename("ghost"))
                    .unwrap_err()
            ),
            "user not found"
        );

        let delete = DeleteCreature {
            directory: &memory,
            balances: &memory,
        };
        assert_eq!(
            denied_text(delete.execute("2@global", "3@global").unwrap_err()),
            "access denied"
        );
        delete.execute("3@global", "3@global").unwrap();
        assert_eq!(
            denied_text(delete.execute(ROOT_CREATURE, "3@global").unwrap_err()),
            "user not found"
        );
    }
}
