//! Store, membership, and signal ports of one state action (ADR 0036): the store,
//! membership, metadata, and `realtime.event` models through the capsule
//! repositories over the action's transaction.

use anyhow::anyhow;
use aseman_application::ApplicationError;
use aseman_capsule::store::CapsuleStorePorts;
use aseman_contracts::legacy_realtime::SignalStreamPolicy;
use aseman_domain::store::{StoreRecord, StoreSignal};
use aseman_ports::{
    ClockPort, PortError, PortResult, SignalLog, StoreAccess, StoreDirectory, StoreMetadata,
};

use crate::api::model::Store;
use crate::api::model::access::StorePermissions;
use crate::core::trx::Trx;
use crate::models::packet::{LogPacket, LogQuery};

/// A store record as the legacy `Store` wire view.
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

/// Store records and metadata of one state action.
pub(crate) struct StorePorts<'a> {
    pub(crate) trx: &'a Trx,
}

/// Store membership of one state action.
pub(crate) struct MembershipPorts<'a> {
    pub(crate) trx: &'a Trx,
}

/// The signal log of one state action.
pub(crate) struct SignalPorts<'a> {
    pub(crate) trx: &'a Trx,
}

fn stores(trx: &Trx) -> CapsuleStorePorts<'_> {
    CapsuleStorePorts {
        repository: trx,
        stream_policy: &SignalStreamPolicy::for_store,
    }
}

impl StoreDirectory for StorePorts<'_> {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
        stores(self.trx).store(store_id)
    }
    fn record_signal(&self, store_id: &str) -> PortResult<()> {
        stores(self.trx).record_signal(store_id)
    }
    fn stores(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<StoreRecord>> {
        stores(self.trx).stores(offset, count)
    }
    fn create_store(&self, record: &StoreRecord, creator_id: &str) -> PortResult<()> {
        stores(self.trx).create_store(record, creator_id)
    }
    fn update_store(&self, record: &StoreRecord) -> PortResult<()> {
        stores(self.trx).update_store(record)
    }
    fn delete_store(&self, store_id: &str) -> PortResult<()> {
        stores(self.trx).delete_store(store_id)
    }
    fn release_creator(&self, store_id: &str, creator_id: &str) -> PortResult<()> {
        stores(self.trx).release_creator(store_id, creator_id)
    }
}

impl StoreMetadata for StorePorts<'_> {
    fn store_metadata(&self, store_id: &str, path: &str) -> PortResult<Option<String>> {
        stores(self.trx).store_metadata(store_id, path)
    }
    fn merge_store_metadata(&self, store_id: &str, document: &str) -> PortResult<()> {
        stores(self.trx).merge_store_metadata(store_id, document)
    }
    fn delete_store_metadata(&self, store_id: &str) -> PortResult<()> {
        stores(self.trx).delete_store_metadata(store_id)
    }
}

impl StoreAccess for MembershipPorts<'_> {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
        stores(self.trx).permissions(store_id, member_id)
    }
    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        stores(self.trx).set_permissions(store_id, member_id, permissions)
    }
    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
        stores(self.trx).is_member(store_id, member_id)
    }
    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
        stores(self.trx).members(store_id)
    }
    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
        stores(self.trx).stores_of(member_id)
    }
    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        stores(self.trx).join(store_id, member_id, permissions)
    }
    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
        stores(self.trx).leave(store_id, member_id)
    }
}

impl SignalLog for SignalPorts<'_> {
    fn append(
        &self,
        store_id: &str,
        sender_id: &str,
        data: &str,
        tags: &[String],
        time_millis: i64,
    ) -> PortResult<StoreSignal> {
        stores(self.trx).append(store_id, sender_id, data, tags, time_millis)
    }
    fn history(&self, store_id: &str, query: &LogQuery) -> PortResult<Vec<StoreSignal>> {
        stores(self.trx).history(store_id, query)
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
