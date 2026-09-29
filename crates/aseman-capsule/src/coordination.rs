//! The coordination port on the storage module (ADR 0038): fenced singleton leases
//! (`core.coordination_lease`) and fenced destinations (`core.coordination_fence`).
//!
//! Every decision is one transaction at the provider's clock. The lease row is read
//! and rewritten with its revision, so of two replicas racing for a lease exactly one
//! commits and the other decides again on what it wrote. A lease row is never
//! deleted: it carries the token counter, and tokens rise for the life of the name.

use aseman_domain::coordination::{Acquisition, FencingToken, Lease, LeaseName, plan_acquisition};
use aseman_ports::coordination::{CoordinationPort, FencedDestination};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{coordination_fence, coordination_lease};
use aseman_storage::{Models, Storage};

use crate::auto::AutoCommit;

/// Leases and fences in the node's storage.
#[derive(Clone)]
pub struct StorageCoordination(AutoCommit);

impl StorageCoordination {
    #[must_use]
    pub fn new(storage: Storage) -> Self {
        Self(AutoCommit(storage))
    }
}

fn token(value: i64) -> PortResult<FencingToken> {
    FencingToken::from_stored(u64::try_from(value).map_err(PortError::failed)?)
        .map_err(PortError::failed)
}

fn stored(token: FencingToken) -> PortResult<i64> {
    i64::try_from(token.get()).map_err(PortError::failed)
}

fn lease(name: &LeaseName, row: &coordination_lease::CoordinationLease) -> PortResult<Lease> {
    Ok(Lease {
        name: name.clone(),
        instance: row.instance.clone(),
        token: token(row.token)?,
        acquired_at_millis: row.acquired_at_millis,
        expires_at_millis: row.expires_at_millis,
    })
}

impl CoordinationPort for StorageCoordination {
    fn acquire(
        &self,
        name: &LeaseName,
        instance: &str,
        ttl_millis: i64,
    ) -> PortResult<Acquisition> {
        self.0.decide(|trx| {
            let now = match self.0.now_millis() {
                Ok(now) => now,
                Err(error) => return Ok(Err(error)),
            };
            let current = trx
                .coordination_lease()
                .find_unique(coordination_lease::by_key(name.as_str()))?;
            let current_lease = match current.as_ref().map(|row| lease(name, row)).transpose() {
                Ok(lease) => lease,
                Err(error) => return Ok(Err(error)),
            };
            let planned =
                match plan_acquisition(name, current_lease.as_ref(), instance, now, ttl_millis) {
                    Ok(planned) => planned,
                    Err(error) => return Ok(Err(PortError::failed(error))),
                };
            let Acquisition::Granted(granted) = &planned else {
                // Nothing is written: the holder keeps its row and its expiry.
                return Ok(Ok(planned));
            };
            let token = match stored(granted.token) {
                Ok(token) => token,
                Err(error) => return Ok(Err(error)),
            };
            if current.is_some() {
                trx.coordination_lease().update(
                    coordination_lease::by_key(name.as_str()),
                    coordination_lease::update()
                        .instance(granted.instance.clone())
                        .token(token)
                        .acquired_at_millis(granted.acquired_at_millis)
                        .expires_at_millis(granted.expires_at_millis),
                )?;
            } else {
                trx.coordination_lease()
                    .create(coordination_lease::Create {
                        key: name.as_str().to_owned(),
                        instance: granted.instance.clone(),
                        token,
                        acquired_at_millis: granted.acquired_at_millis,
                        expires_at_millis: granted.expires_at_millis,
                    })?;
            }
            Ok(Ok(planned))
        })
    }

    fn renew(&self, held: &Lease, ttl_millis: i64) -> PortResult<Option<Lease>> {
        if ttl_millis <= 0 {
            return Err(PortError::Denied("a lease time to live is positive"));
        }
        let held_token = stored(held.token)?;
        self.0.decide(|trx| {
            let now = match self.0.now_millis() {
                Ok(now) => now,
                Err(error) => return Ok(Err(error)),
            };
            let Some(row) = trx
                .coordination_lease()
                .find_unique(coordination_lease::by_key(held.name.as_str()))?
            else {
                return Ok(Ok(None));
            };
            // The owner, the token, and an unexpired row are all required: a holder
            // that has been taken over gets `None` and must stop, not retry.
            if row.instance != held.instance
                || row.token != held_token
                || row.expires_at_millis <= now
            {
                return Ok(Ok(None));
            }
            let renewed = trx.coordination_lease().update(
                coordination_lease::by_key(held.name.as_str()),
                coordination_lease::update().expires_at_millis(now + ttl_millis),
            )?;
            Ok(renewed
                .as_ref()
                .map(|row| lease(&held.name, row))
                .transpose())
        })
    }

    fn release(&self, held: &Lease) -> PortResult<()> {
        let held_token = stored(held.token)?;
        self.0.decide(|trx| {
            let now = match self.0.now_millis() {
                Ok(now) => now,
                Err(error) => return Ok(Err(error)),
            };
            // Scoped to this holder and token: a stale release never frees the lease
            // the next holder has already taken. It expires the lease and keeps the row.
            if let Some(row) = trx
                .coordination_lease()
                .find_unique(coordination_lease::by_key(held.name.as_str()))?
                && row.instance == held.instance
                && row.token == held_token
            {
                trx.coordination_lease().update(
                    coordination_lease::by_key(held.name.as_str()),
                    coordination_lease::update().expires_at_millis(row.acquired_at_millis.max(now)),
                )?;
            }
            Ok(Ok(()))
        })
    }

    fn read(&self, name: &LeaseName) -> PortResult<Option<Lease>> {
        self.0
            .read(|trx| {
                trx.coordination_lease()
                    .find_unique(coordination_lease::by_key(name.as_str()))
            })?
            .map(|row| lease(name, &row))
            .transpose()
    }

    fn now_millis(&self) -> PortResult<i64> {
        self.0.now_millis()
    }
}

impl FencedDestination for StorageCoordination {
    fn last_accepted(&self, name: &LeaseName) -> PortResult<Option<FencingToken>> {
        self.0
            .read(|trx| {
                trx.coordination_fence()
                    .find_unique(coordination_fence::by_key(name.as_str()))
            })?
            .map(|row| token(row.token))
            .transpose()
    }

    fn accept(&self, name: &LeaseName, accepted: FencingToken) -> PortResult<()> {
        let accepted = stored(accepted)?;
        self.0.decide(|trx| {
            match trx
                .coordination_fence()
                .find_unique(coordination_fence::by_key(name.as_str()))?
            {
                // Moving the fence backwards is refused, and the effect with it.
                Some(row) if row.token > accepted => return Ok(Err(PortError::Conflict)),
                Some(_) => {
                    trx.coordination_fence().update(
                        coordination_fence::by_key(name.as_str()),
                        coordination_fence::update().token(accepted),
                    )?;
                }
                None => {
                    trx.coordination_fence()
                        .create(coordination_fence::Create {
                            key: name.as_str().to_owned(),
                            token: accepted,
                        })?;
                }
            }
            Ok(Ok(()))
        })
    }
}
