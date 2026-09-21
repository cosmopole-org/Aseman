//! Store, membership, and signal ports of one state action, routed per ADR 0026: to
//! the action's PostgreSQL unit of work when the node runs on PostgreSQL, else to the
//! legacy adapters (RL-004 strangler), whose key encodings are exactly the legacy ones.

use std::sync::Arc;

use anyhow::anyhow;
use aseman_application::ApplicationError;
use aseman_domain::store::{StoreRecord, StoreSignal};
use aseman_ports::{
    ClockPort, PortError, PortResult, SignalLog, StoreAccess, StoreDirectory, StoreMetadata,
};

use crate::models::packet::{LogPacket, LogQuery};
use crate::models::ports::storage::IStorage;
use crate::models::transaction::ITrx;
use crate::shell::api::model::access::{access_link_key, read_permissions, StorePermissions};
use crate::shell::api::model::Store;

/// Legacy store adapter: the `Store` object and its `creatorof` link behind
/// [`StoreDirectory`], and `StoreMeta` behind [`StoreMetadata`]. It needs only the
/// transaction (RL-004 strangler).
struct LegacyStores<'a> {
    trx: &'a dyn ITrx,
}

/// The legacy `Store` object columns, as `Store::push` writes them.
const STORE_COLUMNS: [&str; 8] = [
    "|",
    "id",
    "tag",
    "parentId",
    "isPublic",
    "persHist",
    "memberCount",
    "signalCount",
];

fn store_record(store: Store) -> StoreRecord {
    StoreRecord {
        id: store.id,
        persistent_history: store.pers_hist,
        signal_count: store.signal_count,
        tag: store.tag,
        parent_id: store.parent_id,
        is_public: store.is_public,
        member_count: i64::from(store.member_count),
    }
}

/// The legacy wire shape of a store.
pub(crate) fn store_view(record: StoreRecord) -> Store {
    Store {
        id: record.id,
        tag: record.tag,
        parent_id: record.parent_id,
        pers_hist: record.persistent_history,
        is_public: record.is_public,
        member_count: i32::try_from(record.member_count).unwrap_or(i32::MAX),
        signal_count: record.signal_count,
    }
}

impl StorePorts<'_> {
    /// A store as legacy `Store::pull` returned it: a missing store reads as an empty
    /// record carrying the requested id.
    pub(crate) fn store_or_empty(&self, store_id: &str) -> Store {
        match self.store(store_id).ok().flatten() {
            Some(record) => store_view(record),
            None => Store {
                id: store_id.to_owned(),
                ..Default::default()
            },
        }
    }

    /// The metadata object at `path`, as legacy `get_json(..).ok()` returned it.
    pub(crate) fn metadata_object(
        &self,
        store_id: &str,
        path: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let text = self.store_metadata(store_id, path).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Deep-merge `document` into the metadata; a non-object is ignored, as legacy
    /// `put_json` failed on it without effect.
    pub(crate) fn merge_metadata_value(
        &self,
        store_id: &str,
        document: &serde_json::Value,
    ) -> PortResult<()> {
        if !document.is_object() {
            return Ok(());
        }
        let text = serde_json::to_string(document)
            .map_err(|error| PortError::Failed(error.to_string()))?;
        self.merge_store_metadata(store_id, &text)
    }
}

impl StoreDirectory for LegacyStores<'_> {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
        // `Store::pull` keeps the id it was handed whether or not the object
        // exists, so absence is read off the columns themselves.
        if self.trx.get_obj(Store::type_(), store_id).is_empty() {
            return Ok(None);
        }
        Ok(Some(store_record(
            Store {
                id: store_id.to_string(),
                ..Default::default()
            }
            .pull(self.trx),
        )))
    }

    fn record_signal(&self, store_id: &str) -> PortResult<()> {
        let mut store = Store {
            id: store_id.to_string(),
            ..Default::default()
        }
        .pull(self.trx);
        store.signal_count += 1;
        store.push(self.trx);
        Ok(())
    }

    fn stores(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<StoreRecord>> {
        let mut all = self
            .trx
            .get_obj_list(Store::type_(), &["*".to_owned()], &Default::default(), &[])
            .map_err(|error| PortError::Failed(error.to_string()))?
            .into_iter()
            .map(|(id, columns)| Store::from_columns(id, &columns))
            .collect::<Vec<_>>();
        all.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(aseman_domain::creature::legacy_page(
            all.into_iter().map(store_record),
            offset,
            count,
        ))
    }

    fn create_store(&self, record: &StoreRecord, creator_id: &str) -> PortResult<()> {
        if !self.trx.get_obj(Store::type_(), &record.id).is_empty() {
            return Err(PortError::Conflict);
        }
        store_view(record.clone()).push(self.trx);
        self.trx
            .put_link(&format!("creatorof::{creator_id}::{}", record.id), "true");
        Ok(())
    }

    fn update_store(&self, record: &StoreRecord) -> PortResult<()> {
        if self.store(&record.id)?.is_none() {
            return Err(PortError::NotFound);
        }
        store_view(record.clone()).push(self.trx);
        Ok(())
    }

    fn delete_store(&self, store_id: &str) -> PortResult<()> {
        for column in STORE_COLUMNS {
            self.trx
                .del_key(&format!("obj::Store::{store_id}::{column}"));
        }
        // The creator link goes with the store (ADR 0018 export verifies it).
        // A suffix to match, not a key.
        let suffix = ["::", store_id].concat();
        for link in self
            .trx
            .get_links_list("creatorof::", -1, -1, &[])
            .unwrap_or_default()
        {
            if let Some(creator) = link
                .strip_prefix("creatorof::")
                .and_then(|rest| rest.strip_suffix(&suffix))
            {
                self.trx
                    .del_key(&format!("link::creatorof::{creator}::{store_id}"));
            }
        }
        Ok(())
    }

    fn release_creator(&self, store_id: &str, creator_id: &str) -> PortResult<()> {
        self.trx
            .del_key(&format!("link::creatorof::{creator_id}::{store_id}"));
        Ok(())
    }
}

fn store_metadata_key(store_id: &str) -> String {
    format!("StoreMeta::{store_id}")
}

impl StoreMetadata for LegacyStores<'_> {
    fn store_metadata(&self, store_id: &str, path: &str) -> PortResult<Option<String>> {
        match self.trx.get_json(&store_metadata_key(store_id), path) {
            Ok(object) => serde_json::to_string(&object)
                .map(Some)
                .map_err(|error| PortError::Failed(error.to_string())),
            Err(_) => Ok(None),
        }
    }

    fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()> {
        let document = match serde_json::from_str::<serde_json::Value>(document) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => {
                return Err(PortError::Failed(
                    "metadata must be a JSON object".to_owned(),
                ))
            }
        };
        self.trx
            .put_json(&store_metadata_key(store_id), "metadata", &document, true)
            .map_err(|error| PortError::Failed(error.to_string()))
    }

    fn delete_store_metadata(&self, store_id: &str) -> PortResult<()> {
        self.trx.del_json(&store_metadata_key(store_id), "metadata");
        Ok(())
    }
}

/// Legacy membership adapter: the `onaccess`/`hasaccess` link pair behind
/// [`StoreAccess`]. It needs only the transaction, so guards, the signaler,
/// sessions, federation, and VM host calls share it (RL-004 strangler).
struct LegacyMembership<'a> {
    trx: &'a dyn ITrx,
}

impl StoreAccess for LegacyMembership<'_> {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
        Ok(read_permissions(self.trx, store_id, member_id))
    }

    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        self.trx
            .put_link(&access_link_key(store_id, member_id), &permissions.encode());
        Ok(())
    }

    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
        Ok(self
            .trx
            .get_link(&format!("hasaccess::{member_id}::{store_id}"))
            == "true")
    }

    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
        let prefix = format!("onaccess::{store_id}::");
        let mut members = self
            .trx
            .get_links_list(&prefix, -1, -1, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|key| {
                let member = key.strip_prefix(&prefix)?.to_string();
                (!member.is_empty()).then(|| {
                    let permissions = StorePermissions::parse(&self.trx.get_link(&key));
                    (member, permissions)
                })
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(members)
    }

    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
        let prefix = format!("hasaccess::{member_id}::");
        let mut stores = self
            .trx
            .get_links_list(&prefix, -1, -1, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
            .filter(|store| !store.is_empty())
            .collect::<Vec<_>>();
        stores.sort();
        Ok(stores)
    }

    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        self.trx
            .put_link(&access_link_key(store_id, member_id), &permissions.encode());
        self.trx
            .put_link(&format!("hasaccess::{member_id}::{store_id}"), "true");
        Ok(())
    }

    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
        self.trx
            .del_key(&format!("link::{}", access_link_key(store_id, member_id)));
        self.trx
            .del_key(&format!("link::hasaccess::{member_id}::{store_id}"));
        Ok(())
    }
}

impl MembershipPorts<'_> {
    /// The existing stores `member_id` belongs to, in store-id order, at most
    /// `limit` of them. Memberships of a store whose object is gone are skipped, as
    /// the legacy `Store::list` over `hasaccess` did.
    pub(crate) fn member_stores(&self, member_id: &str, limit: usize) -> PortResult<Vec<Store>> {
        let stores = StorePorts { trx: self.trx };
        let mut found = Vec::new();
        for store_id in self.stores_of(member_id)?.into_iter().take(limit) {
            if let Some(record) = stores.store(&store_id)? {
                found.push(store_view(record));
            }
        }
        Ok(found)
    }

    /// Removes `member_id` from every store and deletes each store left with no
    /// other member. Returns the ids of the deleted stores (LD-12).
    pub(crate) fn remove_member_everywhere(&self, member_id: &str) -> PortResult<Vec<String>> {
        let stores = StorePorts { trx: self.trx };
        let mut deleted = Vec::new();
        for store_id in self.stores_of(member_id)? {
            self.leave(&store_id, member_id)?;
            stores.release_creator(&store_id, member_id)?;
            let others = self
                .members(&store_id)?
                .iter()
                .any(|(member, _)| member != member_id);
            if !others && stores.store(&store_id)?.is_some() {
                stores.delete_store(&store_id)?;
                stores.delete_store_metadata(&store_id)?;
                deleted.push(store_id);
            }
        }
        Ok(deleted)
    }
}

fn store_signal(packet: LogPacket) -> StoreSignal {
    StoreSignal {
        id: packet.id,
        store_id: packet.store_id,
        sender_id: packet.user_id,
        data: packet.data,
        tags: packet.tags,
        time_millis: packet.time,
        edited: packet.edited,
    }
}

pub(crate) fn log_packet(signal: StoreSignal) -> LogPacket {
    LogPacket {
        id: signal.id,
        user_id: signal.sender_id,
        data: signal.data,
        store_id: signal.store_id,
        tags: signal.tags,
        time: signal.time_millis,
        edited: signal.edited,
    }
}

/// The legacy signal log: the QuestDB time series behind the storage driver.
struct LegacySignalLog {
    storage: Arc<dyn IStorage>,
}

impl SignalLog for LegacySignalLog {
    fn append(
        &self,
        store_id: &str,
        sender_id: &str,
        data: &str,
        tags: &[String],
        time_millis: i64,
    ) -> PortResult<StoreSignal> {
        self.storage
            .log_time_sieries(store_id, sender_id, data, tags, time_millis)
            .map(store_signal)
            .map_err(|error| PortError::Failed(error.to_string()))
    }

    fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>> {
        self.storage
            .read_store_logs(store_id, query)
            .map(|packets| packets.into_iter().map(store_signal).collect())
            .map_err(|error| PortError::Failed(error.to_string()))
    }
}

/// Store records and metadata of one state action (ADR 0026 routing).
pub(crate) struct StorePorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

/// Store membership of one state action (ADR 0026 routing).
pub(crate) struct MembershipPorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

/// The signal log of one state action (ADR 0026 routing).
pub(crate) struct SignalPorts {
    pub(crate) storage: Arc<dyn IStorage>,
}

/// Run `$call` on the capsule store adapter of the current unit of work, or on
/// `$legacy`, bound as `$ports`.
macro_rules! route {
    ($legacy:expr, |$ports:ident| $call:expr) => {
        match crate::shell::api::model::core_storage::current_unit() {
            Some(unit) => {
                let policy = aseman_contracts::legacy_realtime::SignalStreamPolicy::for_store;
                let $ports = aseman_capsule_repositories::store::CapsuleStorePorts {
                    repository: &*unit,
                    stream_policy: &policy,
                };
                $call
            }
            None => {
                let $ports = $legacy;
                $call
            }
        }
    };
}

impl StoreDirectory for StorePorts<'_> {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .store(store_id))
    }
    fn record_signal(&self, store_id: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .record_signal(store_id))
    }
    fn stores(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<StoreRecord>> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .stores(offset, count))
    }
    fn create_store(&self, record: &StoreRecord, creator_id: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .create_store(record, creator_id))
    }
    fn update_store(&self, record: &StoreRecord) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .update_store(record))
    }
    fn delete_store(&self, store_id: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .delete_store(store_id))
    }
    fn release_creator(&self, store_id: &str, creator_id: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .release_creator(store_id, creator_id))
    }
}

impl StoreMetadata for StorePorts<'_> {
    fn store_metadata(&self, store_id: &str, path: &str) -> PortResult<Option<String>> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .store_metadata(store_id, path))
    }
    fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .merge_store_metadata(store_id, document))
    }
    fn delete_store_metadata(&self, store_id: &str) -> PortResult<()> {
        route!(LegacyStores { trx: self.trx }, |ports| ports
            .delete_store_metadata(store_id))
    }
}

impl StoreAccess for MembershipPorts<'_> {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .permissions(store_id, member_id))
    }
    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .set_permissions(store_id, member_id, permissions))
    }
    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .is_member(store_id, member_id))
    }
    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .members(store_id))
    }
    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .stores_of(member_id))
    }
    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports.join(
            store_id,
            member_id,
            permissions
        ))
    }
    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
        route!(LegacyMembership { trx: self.trx }, |ports| ports
            .leave(store_id, member_id))
    }
}

impl SignalLog for SignalPorts {
    fn append(
        &self,
        store_id: &str,
        sender_id: &str,
        data: &str,
        tags: &[String],
        time_millis: i64,
    ) -> PortResult<StoreSignal> {
        route!(
            LegacySignalLog {
                storage: self.storage.clone()
            },
            |ports| ports.append(store_id, sender_id, data, tags, time_millis)
        )
    }
    fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>> {
        route!(
            LegacySignalLog {
                storage: self.storage.clone()
            },
            |ports| ports.history(store_id, query)
        )
    }
}

pub(crate) struct SystemClock;

impl ClockPort for SystemClock {
    fn unix_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Client-visible legacy error texts pass through unchanged.
pub(crate) fn legacy_error(error: ApplicationError) -> anyhow::Error {
    match error {
        ApplicationError::Denied(message) | ApplicationError::Port(PortError::Failed(message)) => {
            anyhow!(message)
        }
        other => anyhow!(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::core::actor::model::trx::TrxWrapper;
    use std::sync::Arc;

    #[test]
    fn legacy_stores_pass_the_store_conformance_suite() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let stores = LegacyStores { trx: &*trx };
        aseman_ports::conformance::store_directory(&stores, &stores, "1@global");
        aseman_ports::conformance::store_access(
            &LegacyMembership { trx: &*trx },
            "s-1@conformance",
            ["1@global", "2@global"],
        );
        // The creator link keeps the legacy encoding and leaves with the store.
        assert_eq!(trx.get_link("creatorof::1@global::s-1@conformance"), "true");
        assert_eq!(trx.get_link("creatorof::1@global::s-2@conformance"), "");
    }
}
