//! Fenced singleton coordination (A607, ADR 0013).
//!
//! Exactly one replica may do a named piece of singleton work — reconciliation, outbox
//! publication, directory publication, migrations, scheduled settlement — and the
//! guarantee is not "it holds a lease". A lease can be believed after it has expired:
//! the holder's clock can drift, its process can be paused, its database connection
//! can stall. What makes the guarantee is the **fencing token**: a strictly increasing
//! number allocated on every acquisition, recorded with every committed effect, so a
//! paused former holder that wakes up cannot commit behind the new one.
//!
//! These are the rules, with no clock, no database, and no process in them.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The name of a singleton job. Two replicas doing work under the same name are the
/// thing this module exists to prevent.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseName(String);

impl LeaseName {
    /// # Errors
    ///
    /// When the name is empty or longer than 128 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, CoordinationError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(CoordinationError::InvalidName);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for LeaseName {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A strictly increasing number allocated on every acquisition of a lease.
///
/// It is the fence: an effect committed under token `n` is refused by any destination
/// that has already accepted token `m > n`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FencingToken(u64);

impl FencingToken {
    /// The first token a lease is ever granted.
    pub const FIRST: Self = Self(1);

    /// A token read back from a provider. Tokens start at 1; 0 is not a token, so a
    /// zero-initialized record can never pass for one.
    ///
    /// # Errors
    ///
    /// When `value` is zero.
    pub fn from_stored(value: u64) -> Result<Self, CoordinationError> {
        (value >= 1)
            .then_some(Self(value))
            .ok_or(CoordinationError::InvalidToken)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next token. A provider allocates this on every acquisition, including a
    /// re-acquisition by the same instance.
    ///
    /// # Errors
    ///
    /// When the counter would overflow, which is a provider that must stop rather
    /// than wrap into tokens it has already issued.
    pub fn next(self) -> Result<Self, CoordinationError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(CoordinationError::TokenOverflow)
    }
}

impl core::fmt::Display for FencingToken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A held lease, as the holder knows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub name: LeaseName,
    /// The instance that holds it; a replica's own identity, not its address.
    pub instance: String,
    pub token: FencingToken,
    /// Database time, never the holder's clock.
    pub acquired_at_millis: i64,
    pub expires_at_millis: i64,
}

/// How long before expiry a holder must stop working.
///
/// Expiry is decided by database time, and the holder learns it through a connection
/// that can be slow. The margin covers the round trip and the clock skew an operator
/// is willing to assume; a holder that stops this early can never still be working
/// when the provider hands the lease on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SafetyMargin {
    pub millis: i64,
}

impl SafetyMargin {
    /// # Errors
    ///
    /// When the margin is not positive: a zero margin is a claim that the database
    /// round trip and the clock skew are both zero.
    pub fn new(millis: i64) -> Result<Self, CoordinationError> {
        (millis > 0)
            .then_some(Self { millis })
            .ok_or(CoordinationError::InvalidMargin)
    }
}

/// What a holder must do right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStanding {
    /// The lease is comfortably held: keep working.
    Held,
    /// Inside the safety margin: renew now, and stop if the renewal does not land.
    Renew,
    /// Stop. The lease is within the margin of expiry, or past it, and this instance
    /// may not commit another effect under it.
    Stop,
}

/// Whether the holder may still be working, given the time the provider reports.
///
/// `now_millis` is the provider's time, not the caller's clock: a holder that cannot
/// reach the provider has no fresh time and must treat itself as [`LeaseStanding::Stop`]
/// once its last known standing runs out.
#[must_use]
pub fn standing(lease: &Lease, margin: SafetyMargin, now_millis: i64) -> LeaseStanding {
    let deadline = lease.expires_at_millis.saturating_sub(margin.millis);
    if now_millis >= deadline {
        return LeaseStanding::Stop;
    }
    // Renew once the lease is more than half spent, so a single failed renewal is
    // never the difference between working and stopping.
    let half = lease
        .acquired_at_millis
        .saturating_add((deadline.saturating_sub(lease.acquired_at_millis)) / 2);
    if now_millis >= half {
        LeaseStanding::Renew
    } else {
        LeaseStanding::Held
    }
}

/// Whether an effect carrying `token` may be committed at a destination that has
/// already accepted `last_accepted`.
///
/// This is the destination-side guard ADR 0013 requires when an effect cannot be
/// committed in the same transaction as the lease check. Equal tokens are accepted:
/// one holder commits many effects under one token.
#[must_use]
pub fn may_commit(token: FencingToken, last_accepted: Option<FencingToken>) -> bool {
    match last_accepted {
        None => true,
        Some(accepted) => token >= accepted,
    }
}

/// What a provider decided about an acquisition attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Acquisition {
    /// The lease is this instance's, at this token.
    Granted(Lease),
    /// Someone else holds it and it has not expired. `holder` is who, so an operator
    /// can see which replica is doing the work.
    Held {
        holder: String,
        expires_at_millis: i64,
    },
}

/// Whether an acquisition may be granted, and at which token.
///
/// `current` is the row as the provider read it inside the transaction that will write
/// the new one. A lease is taken over only once it has expired by the provider's own
/// clock; taking over early is exactly the double-execution this module prevents.
///
/// # Errors
///
/// When the token counter would overflow.
pub fn plan_acquisition(
    name: &LeaseName,
    current: Option<&Lease>,
    instance: &str,
    now_millis: i64,
    ttl_millis: i64,
) -> Result<Acquisition, CoordinationError> {
    if ttl_millis <= 0 {
        return Err(CoordinationError::InvalidTtl);
    }
    let token = match current {
        // An expired lease, or one this instance already holds, is re-acquired at the
        // next token — never the same one, so a renewal after a gap is distinguishable
        // from the work that came before it.
        Some(lease) if lease.expires_at_millis > now_millis && lease.instance != instance => {
            return Ok(Acquisition::Held {
                holder: lease.instance.clone(),
                expires_at_millis: lease.expires_at_millis,
            });
        }
        Some(lease) => lease.token.next()?,
        None => FencingToken::FIRST,
    };
    Ok(Acquisition::Granted(Lease {
        name: name.clone(),
        instance: instance.to_owned(),
        token,
        acquired_at_millis: now_millis,
        expires_at_millis: now_millis.saturating_add(ttl_millis),
    }))
}

/// Whether a renewal may extend the lease, keeping the same token.
///
/// A renewal never allocates a token: the holder is the same and its effects are the
/// same run of work. A renewal is refused once the lease has been taken over, because
/// the owner or the token no longer match.
#[must_use]
pub fn may_renew(current: Option<&Lease>, held: &Lease, now_millis: i64) -> bool {
    current.is_some_and(|lease| {
        lease.instance == held.instance
            && lease.token == held.token
            && lease.expires_at_millis > now_millis
    })
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CoordinationError {
    #[error("a lease name is 1 to 128 bytes")]
    InvalidName,
    #[error("a fencing token starts at 1")]
    InvalidToken,
    #[error("the fencing token counter is exhausted")]
    TokenOverflow,
    #[error("a lease time to live is positive")]
    InvalidTtl,
    #[error("a safety margin is positive")]
    InvalidMargin,
}

#[cfg(test)]
mod tests;
