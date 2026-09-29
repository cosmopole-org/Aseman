//! The federation ports on the storage module (ADR 0038, A704/A705): the descriptor
//! directory (`core.federation_node_descriptor`, `core.federation_workload_descriptor`)
//! and the envelope guard's nonces and answers (`core.federation_nonce`,
//! `core.federation_answer`).

use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, NodeDescriptor, WorkloadDescriptor};
use aseman_ports::federation::{Directory, EnvelopeGuard};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{
    federation_answer, federation_node_descriptor, federation_nonce, federation_workload_descriptor,
};
use aseman_storage::{Models, Storage, StorageError};

use crate::auto::AutoCommit;

/// This node's federation records in its storage.
#[derive(Clone)]
pub struct StorageFederation {
    storage: AutoCommit,
    /// The node this process is; `own_node` reads its descriptor.
    node_id: Uuid,
}

impl StorageFederation {
    #[must_use]
    pub fn new(storage: Storage, node_id: Uuid) -> Self {
        Self {
            storage: AutoCommit(storage),
            node_id,
        }
    }
}

fn encoded<T: serde::Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::invalid(error.to_string()))
}

fn decoded<T: serde::de::DeserializeOwned>(text: &str) -> PortResult<T> {
    serde_json::from_str(text).map_err(PortError::failed)
}

fn signed(value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|error| StorageError::invalid(error.to_string()))
}

fn nonce_key(envelope: &Envelope) -> String {
    format!("{}::{}", envelope.source_node, envelope.nonce)
}

impl Directory for StorageFederation {
    fn own_node(&self) -> PortResult<NodeDescriptor> {
        // A node with no descriptor of its own has not been enrolled; that is a
        // configuration failure, not an empty cache.
        self.node(self.node_id, i64::MIN)?
            .ok_or(PortError::NotFound)
    }

    fn node(&self, node_id: Uuid, now_millis: i64) -> PortResult<Option<NodeDescriptor>> {
        let row = self.storage.read(|trx| {
            trx.federation_node_descriptor()
                .find_unique(federation_node_descriptor::by_key(node_id.to_string()))
        })?;
        // An expired descriptor is not served: the home node is authoritative and this
        // is only a cache.
        row.filter(|row| now_millis < row.expires_at_millis)
            .map(|row| decoded(&row.descriptor))
            .transpose()
    }

    fn record_node(&self, descriptor: &NodeDescriptor) -> PortResult<()> {
        if descriptor.revoked_epochs.contains(&descriptor.key_epoch) {
            // A node cannot publish keys it has itself revoked.
            return Err(PortError::Denied(
                "the descriptor revokes its own key epoch",
            ));
        }
        let key = descriptor.node_id.to_string();
        self.storage.decide(|trx| {
            let sequence = signed(descriptor.sequence)?;
            let text = encoded(descriptor)?;
            match trx
                .federation_node_descriptor()
                .find_unique(federation_node_descriptor::by_key(key.clone()))?
            {
                // A descriptor must move forward, so a replayed older one cannot
                // un-rotate a key.
                Some(current) if current.sequence >= sequence => Ok(Err(PortError::Conflict)),
                Some(_) => {
                    trx.federation_node_descriptor().update(
                        federation_node_descriptor::by_key(key.clone()),
                        federation_node_descriptor::update()
                            .sequence(sequence)
                            .expires_at_millis(descriptor.expires_at_millis)
                            .descriptor(text),
                    )?;
                    Ok(Ok(()))
                }
                None => {
                    trx.federation_node_descriptor().create(
                        federation_node_descriptor::Create {
                            key: key.clone(),
                            sequence,
                            expires_at_millis: descriptor.expires_at_millis,
                            descriptor: text,
                        },
                    )?;
                    Ok(Ok(()))
                }
            }
        })
    }

    fn workload(
        &self,
        workload_id: Uuid,
        now_millis: i64,
    ) -> PortResult<Option<WorkloadDescriptor>> {
        let row = self.storage.read(|trx| {
            trx.federation_workload_descriptor().find_unique(
                federation_workload_descriptor::by_key(workload_id.to_string()),
            )
        })?;
        row.filter(|row| now_millis < row.expires_at_millis)
            .map(|row| decoded(&row.descriptor))
            .transpose()
    }

    fn record_workload(&self, descriptor: &WorkloadDescriptor) -> PortResult<()> {
        let key = descriptor.workload_id.to_string();
        self.storage.decide(|trx| {
            let revision = signed(descriptor.revision)?;
            let text = encoded(descriptor)?;
            match trx
                .federation_workload_descriptor()
                .find_unique(federation_workload_descriptor::by_key(key.clone()))?
            {
                Some(current) if current.workload_revision >= revision => {
                    Ok(Err(PortError::Conflict))
                }
                Some(_) => {
                    trx.federation_workload_descriptor().update(
                        federation_workload_descriptor::by_key(key.clone()),
                        federation_workload_descriptor::update()
                            .workload_revision(revision)
                            .expires_at_millis(descriptor.expires_at_millis)
                            .descriptor(text),
                    )?;
                    Ok(Ok(()))
                }
                None => {
                    trx.federation_workload_descriptor().create(
                        federation_workload_descriptor::Create {
                            key: key.clone(),
                            workload_revision: revision,
                            expires_at_millis: descriptor.expires_at_millis,
                            descriptor: text,
                        },
                    )?;
                    Ok(Ok(()))
                }
            }
        })
    }
}

impl EnvelopeGuard for StorageFederation {
    fn remember_nonce(&self, envelope: &Envelope) -> PortResult<bool> {
        let key = nonce_key(envelope);
        // A nonce already recorded is a replay.
        self.storage.decide(|trx| {
            if trx
                .federation_nonce()
                .find_unique(federation_nonce::by_key(key.clone()))?
                .is_some()
            {
                return Ok(Ok(false));
            }
            trx.federation_nonce().create(federation_nonce::Create {
                key: key.clone(),
                expires_at_millis: envelope.expires_at_millis,
            })?;
            Ok(Ok(true))
        })
    }

    fn forget_nonce(&self, envelope: &Envelope) -> PortResult<()> {
        let key = nonce_key(envelope);
        self.storage.decide(|trx| {
            trx.federation_nonce()
                .delete(federation_nonce::by_key(key.clone()))?;
            Ok(Ok(()))
        })
    }

    fn recorded_answer(&self, request_id: Uuid) -> PortResult<Option<String>> {
        Ok(self
            .storage
            .read(|trx| {
                trx.federation_answer()
                    .find_unique(federation_answer::by_key(request_id.to_string()))
            })?
            .map(|row| row.answer))
    }

    fn record_answer(
        &self,
        request_id: Uuid,
        answer: &str,
        expires_at_millis: i64,
    ) -> PortResult<()> {
        let key = request_id.to_string();
        // The first answer stands: a second answer is the retry path racing itself,
        // and must not replace what a caller may already have received.
        self.storage.decide(|trx| {
            if trx
                .federation_answer()
                .find_unique(federation_answer::by_key(key.clone()))?
                .is_none()
            {
                trx.federation_answer().create(federation_answer::Create {
                    key: key.clone(),
                    answer: answer.to_owned(),
                    expires_at_millis,
                })?;
            }
            Ok(Ok(()))
        })
    }

    fn purge_expired(&self, now_millis: i64) -> PortResult<u64> {
        self.storage.decide(|trx| {
            let nonces = trx
                .federation_nonce()
                .delete_many(Some(federation_nonce::expires_at_millis().lt(now_millis)))?;
            let answers = trx
                .federation_answer()
                .delete_many(Some(federation_answer::expires_at_millis().lt(now_millis)))?;
            Ok(Ok(nonces + answers))
        })
    }
}
