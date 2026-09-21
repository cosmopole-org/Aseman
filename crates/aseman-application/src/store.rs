//! Store messaging use cases (RL-004 strangler slice: `/stores/*`). The rules and
//! client-visible error texts are identical to the legacy actions; transport
//! fan-out stays in the adapter.

use crate::ApplicationError;
use aseman_domain::signal_tags::{LogQuery, validate_tags};
use aseman_domain::store::StoreSignal;
use aseman_domain::store_permissions::StorePermissions;
use aseman_ports::{ClockPort, SignalLog, StoreAccess, StoreDirectory};

/// Page size for a history read that names none.
pub const DEFAULT_HISTORY_COUNT: i64 = 100;

fn denied(message: &str) -> ApplicationError {
    ApplicationError::Denied(message.to_owned())
}

/// Result of `/stores/signal`, before fan-out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalOutcome {
    pub persisted: bool,
    pub signal: Option<StoreSignal>,
    pub tags: Vec<String>,
    pub time_millis: i64,
}

pub struct SignalStore<'a> {
    pub stores: &'a dyn StoreDirectory,
    pub access: &'a dyn StoreAccess,
    pub log: &'a dyn SignalLog,
    pub clock: &'a dyn ClockPort,
}

impl SignalStore<'_> {
    /// Persistence is the store's decision: `persHist` records every signal, and the
    /// sender may only opt one out with `temp`. Recording comes first, and its failure
    /// fails the call.
    pub fn execute(
        &self,
        sender_id: &str,
        store_id: &str,
        data: &str,
        raw_tags: &[String],
        temp: bool,
    ) -> Result<SignalOutcome, ApplicationError> {
        if store_id.is_empty() {
            return Err(denied("storeId is required"));
        }
        if !self.access.permissions(store_id, sender_id)?.signal {
            return Err(denied("not allowed to signal in this store"));
        }
        let tags = validate_tags(raw_tags).map_err(|error| denied(&error.to_string()))?;
        let store = self
            .stores
            .store(store_id)?
            .ok_or_else(|| denied("store not found"))?;
        let now = self.clock.unix_millis();
        let persisted = store.persistent_history && !temp;
        let signal = if persisted {
            let signal = self.log.append(store_id, sender_id, data, &tags, now)?;
            self.stores.record_signal(store_id)?;
            Some(signal)
        } else {
            None
        };
        Ok(SignalOutcome {
            persisted,
            signal,
            tags,
            time_millis: now,
        })
    }
}

pub struct ReadStoreHistory<'a> {
    pub access: &'a dyn StoreAccess,
    pub log: &'a dyn SignalLog,
}

impl ReadStoreHistory<'_> {
    pub fn execute(
        &self,
        reader_id: &str,
        store_id: &str,
        query: LogQuery,
    ) -> Result<Vec<StoreSignal>, ApplicationError> {
        if store_id.is_empty() {
            return Err(denied("storeId is required"));
        }
        if !self.access.permissions(store_id, reader_id)?.read {
            return Err(denied("not allowed to read this store"));
        }
        let query = LogQuery {
            count: if query.count > 0 {
                query.count
            } else {
                DEFAULT_HISTORY_COUNT
            },
            ..query
        }
        .validated()
        .map_err(|error| denied(&error.to_string()))?;
        Ok(self.log.history(store_id, &query)?)
    }
}

pub struct SetStoreAccess<'a> {
    pub access: &'a dyn StoreAccess,
}

impl SetStoreAccess<'_> {
    /// Requires `manage`; unknown permission names grant nothing.
    pub fn execute(
        &self,
        caller_id: &str,
        store_id: &str,
        member_id: &str,
        permissions: &[String],
    ) -> Result<StorePermissions, ApplicationError> {
        if store_id.is_empty() {
            return Err(denied("storeId is required"));
        }
        if member_id.is_empty() {
            return Err(denied("memberId is required"));
        }
        if !self.access.permissions(store_id, caller_id)?.manage {
            return Err(denied("not allowed to manage access in this store"));
        }
        let permissions = StorePermissions::from_list(permissions);
        self.access
            .set_permissions(store_id, member_id, permissions)?;
        Ok(permissions)
    }
}

pub struct GetStoreAccess<'a> {
    pub access: &'a dyn StoreAccess,
}

impl GetStoreAccess<'_> {
    /// A member may always read their own grant; another member's needs `manage`.
    /// Returns the resolved member ID and its permissions.
    pub fn execute(
        &self,
        caller_id: &str,
        store_id: &str,
        member_id: &str,
    ) -> Result<(String, StorePermissions), ApplicationError> {
        if store_id.is_empty() {
            return Err(denied("storeId is required"));
        }
        let target = if member_id.is_empty() {
            caller_id
        } else {
            member_id
        };
        let caller = self.access.permissions(store_id, caller_id)?;
        if target != caller_id && !caller.manage {
            return Err(denied("not allowed to read another member's access"));
        }
        let permissions = if target == caller_id {
            caller
        } else {
            self.access.permissions(store_id, target)?
        };
        Ok((target.to_owned(), permissions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::store::StoreRecord;
    use aseman_ports::{PortError, PortResult};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        stores: Mutex<BTreeMap<String, StoreRecord>>,
        grants: Mutex<BTreeMap<(String, String), StorePermissions>>,
        signals: Mutex<Vec<StoreSignal>>,
        fail_log: bool,
    }

    impl StoreDirectory for Memory {
        fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
            Ok(self.stores.lock().unwrap().get(store_id).cloned())
        }
        fn record_signal(&self, store_id: &str) -> PortResult<()> {
            let mut stores = self.stores.lock().unwrap();
            stores
                .get_mut(store_id)
                .ok_or(PortError::NotFound)?
                .signal_count += 1;
            Ok(())
        }
    }

    impl StoreAccess for Memory {
        fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
            Ok(self
                .grants
                .lock()
                .unwrap()
                .get(&(store_id.to_owned(), member_id.to_owned()))
                .copied()
                .unwrap_or_default())
        }
        fn set_permissions(
            &self,
            store_id: &str,
            member_id: &str,
            permissions: StorePermissions,
        ) -> PortResult<()> {
            self.grants
                .lock()
                .unwrap()
                .insert((store_id.to_owned(), member_id.to_owned()), permissions);
            Ok(())
        }
        fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
            Ok(self
                .grants
                .lock()
                .unwrap()
                .contains_key(&(store_id.to_owned(), member_id.to_owned())))
        }
        fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
            Ok(self
                .grants
                .lock()
                .unwrap()
                .iter()
                .filter(|((store, _), _)| store == store_id)
                .map(|((_, member), permissions)| (member.clone(), *permissions))
                .collect())
        }
        fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
            Ok(self
                .grants
                .lock()
                .unwrap()
                .keys()
                .filter(|(_, member)| member == member_id)
                .map(|(store, _)| store.clone())
                .collect())
        }
        fn join(
            &self,
            store_id: &str,
            member_id: &str,
            permissions: StorePermissions,
        ) -> PortResult<()> {
            self.set_permissions(store_id, member_id, permissions)
        }
        fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
            self.grants
                .lock()
                .unwrap()
                .remove(&(store_id.to_owned(), member_id.to_owned()));
            Ok(())
        }
    }

    impl SignalLog for Memory {
        fn append(
            &self,
            store_id: &str,
            sender_id: &str,
            data: &str,
            tags: &[String],
            time_millis: i64,
        ) -> PortResult<StoreSignal> {
            if self.fail_log {
                return Err(PortError::Unavailable("signal log"));
            }
            let signal = StoreSignal {
                id: format!("s{}", self.signals.lock().unwrap().len()),
                store_id: store_id.to_owned(),
                sender_id: sender_id.to_owned(),
                data: data.to_owned(),
                tags: tags.to_vec(),
                time_millis,
                edited: false,
            };
            self.signals.lock().unwrap().push(signal.clone());
            Ok(signal)
        }
        fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>> {
            let mut signals = self
                .signals
                .lock()
                .unwrap()
                .iter()
                .filter(|signal| signal.store_id == store_id)
                .filter(|signal| query.tags_all.iter().all(|tag| signal.tags.contains(tag)))
                .cloned()
                .collect::<Vec<_>>();
            signals.reverse();
            signals.truncate(usize::try_from(query.count).unwrap_or(0));
            Ok(signals)
        }
    }

    struct Clock;
    impl ClockPort for Clock {
        fn unix_millis(&self) -> i64 {
            42
        }
    }

    fn memory(fail_log: bool) -> Memory {
        let memory = Memory {
            fail_log,
            ..Memory::default()
        };
        memory.stores.lock().unwrap().insert(
            "s1".to_owned(),
            StoreRecord {
                id: "s1".to_owned(),
                persistent_history: true,
                signal_count: 0,
            },
        );
        memory
            .set_permissions("s1", "alice", StorePermissions::owner())
            .unwrap();
        memory
            .set_permissions("s1", "viewer", StorePermissions::viewer())
            .unwrap();
        memory
    }

    fn message(error: ApplicationError) -> String {
        match error {
            ApplicationError::Denied(message) => message,
            other => other.to_string(),
        }
    }

    #[test]
    fn signal_records_before_fanout_and_respects_permissions() {
        let memory = memory(false);
        let signal = SignalStore {
            stores: &memory,
            access: &memory,
            log: &memory,
            clock: &Clock,
        };
        let outcome = signal
            .execute("alice", "s1", "hi", &["kind=message".to_owned()], false)
            .unwrap();
        assert!(outcome.persisted);
        assert_eq!(outcome.signal.unwrap().time_millis, 42);
        assert_eq!(memory.store("s1").unwrap().unwrap().signal_count, 1);
        // `temp` opts one signal out of history.
        assert!(
            !signal
                .execute("alice", "s1", "typing", &[], true)
                .unwrap()
                .persisted
        );
        assert_eq!(
            message(signal.execute("viewer", "s1", "x", &[], false).unwrap_err()),
            "not allowed to signal in this store"
        );
        memory
            .set_permissions("s2", "alice", StorePermissions::owner())
            .unwrap();
        assert_eq!(
            message(signal.execute("alice", "s2", "x", &[], false).unwrap_err()),
            "store not found"
        );
        assert_eq!(
            message(signal.execute("alice", "", "x", &[], false).unwrap_err()),
            "storeId is required"
        );
    }

    #[test]
    fn a_failed_log_write_fails_the_signal_and_counts_nothing() {
        let memory = memory(true);
        let signal = SignalStore {
            stores: &memory,
            access: &memory,
            log: &memory,
            clock: &Clock,
        };
        assert!(signal.execute("alice", "s1", "hi", &[], false).is_err());
        assert_eq!(memory.store("s1").unwrap().unwrap().signal_count, 0);
    }

    #[test]
    fn history_and_access_rules_match_legacy() {
        let memory = memory(false);
        let signal = SignalStore {
            stores: &memory,
            access: &memory,
            log: &memory,
            clock: &Clock,
        };
        signal
            .execute("alice", "s1", "one", &["t=a".to_owned()], false)
            .unwrap();
        signal.execute("alice", "s1", "two", &[], false).unwrap();
        let history = ReadStoreHistory {
            access: &memory,
            log: &memory,
        };
        assert_eq!(
            history
                .execute("viewer", "s1", LogQuery::default())
                .unwrap()
                .len(),
            2
        );
        let filtered = history
            .execute(
                "viewer",
                "s1",
                LogQuery {
                    tags_all: vec!["t=a".to_owned()],
                    ..LogQuery::default()
                },
            )
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            message(
                history
                    .execute("mallory", "s1", LogQuery::default())
                    .unwrap_err()
            ),
            "not allowed to read this store"
        );

        let set = SetStoreAccess { access: &memory };
        assert_eq!(
            set.execute(
                "alice",
                "s1",
                "bob",
                &["read".to_owned(), "teleport".to_owned()]
            )
            .unwrap(),
            StorePermissions::viewer()
        );
        assert_eq!(
            message(
                set.execute("viewer", "s1", "bob", &["manage".to_owned()])
                    .unwrap_err()
            ),
            "not allowed to manage access in this store"
        );
        let get = GetStoreAccess { access: &memory };
        assert_eq!(
            get.execute("bob", "s1", "").unwrap(),
            ("bob".to_owned(), StorePermissions::viewer())
        );
        assert_eq!(
            message(get.execute("bob", "s1", "alice").unwrap_err()),
            "not allowed to read another member's access"
        );
        assert_eq!(
            get.execute("alice", "s1", "bob").unwrap().1,
            StorePermissions::viewer()
        );
    }
}
