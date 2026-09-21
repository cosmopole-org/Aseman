//! Legacy adapters for the store use cases and every membership reader: the node
//! transaction and the legacy storage driver behind the application ports (RL-004
//! strangler). Key encodings are exactly the legacy ones.

use std::sync::Arc;

use anyhow::anyhow;
use aseman_application::ApplicationError;
use aseman_domain::store::{StoreRecord, StoreSignal};
use aseman_ports::{ClockPort, PortError, PortResult, SignalLog, StoreAccess, StoreDirectory};

use crate::models::packet::{LogPacket, LogQuery};
use crate::models::ports::storage::IStorage;
use crate::models::transaction::ITrx;
use crate::shell::api::model::access::{access_link_key, read_permissions, StorePermissions};
use crate::shell::api::model::Store;

/// Legacy adapters for the store use cases: the node transaction and the legacy
/// storage driver behind the application ports (RL-004 strangler).
pub(crate) struct LegacyStorePorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
    pub(crate) storage: Arc<dyn IStorage>,
}

impl StoreDirectory for LegacyStorePorts<'_> {
    fn store(&self, store_id: &str) -> PortResult<Option<StoreRecord>> {
        // `Store::pull` keeps the id it was handed whether or not the object
        // exists, so absence is read off the columns themselves.
        if self.trx.get_obj(Store::type_(), store_id).is_empty() {
            return Ok(None);
        }
        let store = Store {
            id: store_id.to_string(),
            ..Default::default()
        }
        .pull(self.trx);
        Ok(Some(StoreRecord {
            id: store.id,
            persistent_history: store.pers_hist,
            signal_count: store.signal_count,
        }))
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
}

/// Legacy membership adapter: the `onaccess`/`hasaccess` link pair behind
/// [`StoreAccess`]. It needs only the transaction, so guards, the signaler,
/// sessions, federation, and VM host calls share it (RL-004 strangler).
pub(crate) struct LegacyMembership<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

impl<'a> LegacyStorePorts<'a> {
    fn membership(&self) -> LegacyMembership<'a> {
        LegacyMembership { trx: self.trx }
    }
}

impl StoreAccess for LegacyStorePorts<'_> {
    fn permissions(&self, store_id: &str, member_id: &str) -> PortResult<StorePermissions> {
        self.membership().permissions(store_id, member_id)
    }

    fn set_permissions(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        self.membership()
            .set_permissions(store_id, member_id, permissions)
    }

    fn is_member(&self, store_id: &str, member_id: &str) -> PortResult<bool> {
        self.membership().is_member(store_id, member_id)
    }

    fn members(&self, store_id: &str) -> PortResult<Vec<(String, StorePermissions)>> {
        self.membership().members(store_id)
    }

    fn stores_of(&self, member_id: &str) -> PortResult<Vec<String>> {
        self.membership().stores_of(member_id)
    }

    fn join(
        &self,
        store_id: &str,
        member_id: &str,
        permissions: StorePermissions,
    ) -> PortResult<()> {
        self.membership().join(store_id, member_id, permissions)
    }

    fn leave(&self, store_id: &str, member_id: &str) -> PortResult<()> {
        self.membership().leave(store_id, member_id)
    }
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

impl LegacyMembership<'_> {
    /// The existing stores `member_id` belongs to, in store-id order, at most
    /// `limit` of them. Links to a store whose object is gone are skipped, as the
    /// legacy `Store::list` over `hasaccess` did.
    pub(crate) fn member_stores(&self, member_id: &str, limit: usize) -> PortResult<Vec<Store>> {
        Ok(self
            .stores_of(member_id)?
            .into_iter()
            .take(limit)
            .filter(|store_id| !self.trx.get_obj(Store::type_(), store_id).is_empty())
            .map(|store_id| {
                Store {
                    id: store_id,
                    ..Default::default()
                }
                .pull(self.trx)
            })
            .collect())
    }

    /// Removes `member_id` from every store and deletes each store left with no
    /// other member. Returns the ids of the deleted stores (LD-12).
    pub(crate) fn remove_member_everywhere(&self, member_id: &str) -> PortResult<Vec<String>> {
        let mut deleted = Vec::new();
        for store_id in self.stores_of(member_id)? {
            self.leave(&store_id, member_id)?;
            self.trx
                .del_key(&format!("link::creatorof::{member_id}::{store_id}"));
            let others = self
                .members(&store_id)?
                .iter()
                .any(|(member, _)| member != member_id);
            if !others && !self.trx.get_obj(Store::type_(), &store_id).is_empty() {
                Store {
                    id: store_id.clone(),
                    ..Default::default()
                }
                .pull(self.trx)
                .delete(self.trx);
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

impl SignalLog for LegacyStorePorts<'_> {
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
