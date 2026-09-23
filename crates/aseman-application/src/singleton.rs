//! Running singleton work under a fenced lease (A607, ADR 0013).
//!
//! A replica calls [`Singleton::run`] on every pass of its worker loop. The lease is
//! acquired when it is free, renewed while it is held, and given up the moment the
//! provider's clock says this instance is inside the safety margin. The work only
//! ever runs with a fencing token in hand, so every effect it commits can be fenced.

use aseman_domain::coordination::{
    Acquisition, FencingToken, Lease, LeaseName, LeaseStanding, SafetyMargin, standing,
};
use aseman_ports::PortResult;
use aseman_ports::coordination::CoordinationPort;

/// What happened on one pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pass {
    /// The work ran under this token.
    Ran(FencingToken),
    /// Another replica holds the lease. This is the normal state of every replica
    /// that is not the one doing the work, not an error.
    Standby { holder: String },
    /// This replica held the lease and gave it up: it is inside the safety margin, or
    /// its renewal did not land. It must not have committed anything after this.
    Yielded,
}

/// One named singleton job, as one replica sees it.
pub struct Singleton<'a> {
    coordination: &'a dyn CoordinationPort,
    name: LeaseName,
    instance: String,
    ttl_millis: i64,
    margin: SafetyMargin,
    held: Option<Lease>,
}

impl<'a> Singleton<'a> {
    /// Take part in `name` as `instance`, holding the lease for `ttl_millis` at a
    /// time and stopping `margin` before it expires.
    #[must_use]
    pub fn new(
        coordination: &'a dyn CoordinationPort,
        name: LeaseName,
        instance: impl Into<String>,
        ttl_millis: i64,
        margin: SafetyMargin,
    ) -> Self {
        Self {
            coordination,
            name,
            instance: instance.into(),
            ttl_millis,
            margin,
            held: None,
        }
    }

    /// The lease this replica holds, if it holds one.
    #[must_use]
    pub fn lease(&self) -> Option<&Lease> {
        self.held.as_ref()
    }

    /// Run `work` if and only if this replica holds the lease with room to spare.
    ///
    /// `work` receives the fencing token it must record with every effect it
    /// commits. A `work` that returns an error does not lose the lease: the failure
    /// is the caller's to report and retry on the next pass.
    ///
    /// # Errors
    ///
    /// When the coordination provider is unreachable. A replica that cannot reach the
    /// provider has given up its lease by the time this returns: it has no fresh time
    /// and may not keep working on an assumption.
    pub fn run<T>(
        &mut self,
        work: impl FnOnce(FencingToken) -> T,
    ) -> PortResult<(Pass, Option<T>)> {
        match self.standing()? {
            Pass::Ran(token) => {
                let outcome = work(token);
                Ok((Pass::Ran(token), Some(outcome)))
            }
            other => Ok((other, None)),
        }
    }

    /// Give the lease up deliberately, so another replica can take it without waiting
    /// for the expiry. Shutdown should call this.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable; the lease then expires on its own.
    pub fn resign(&mut self) -> PortResult<()> {
        if let Some(lease) = self.held.take() {
            self.coordination.release(&lease)?;
        }
        Ok(())
    }

    /// Where this replica stands, acquiring or renewing as needed.
    fn standing(&mut self) -> PortResult<Pass> {
        // A provider that cannot be reached leaves this replica with no fresh time,
        // so it drops the lease rather than assume it still holds one.
        let now = match self.coordination.now_millis() {
            Ok(now) => now,
            Err(error) => {
                self.held = None;
                return Err(error);
            }
        };
        let Some(lease) = self.held.clone() else {
            return self.acquire();
        };
        match standing(&lease, self.margin, now) {
            LeaseStanding::Held => Ok(Pass::Ran(lease.token)),
            LeaseStanding::Renew => match self.coordination.renew(&lease, self.ttl_millis)? {
                Some(renewed) => {
                    let token = renewed.token;
                    self.held = Some(renewed);
                    Ok(Pass::Ran(token))
                }
                // The lease was taken over while this replica held it. It stops here,
                // this pass, without doing the work.
                None => {
                    self.held = None;
                    Ok(Pass::Yielded)
                }
            },
            LeaseStanding::Stop => {
                self.held = None;
                // Releasing is best effort: the lease expires on its own anyway, and
                // this replica has already stopped.
                let _ = self.coordination.release(&lease);
                Ok(Pass::Yielded)
            }
        }
    }

    fn acquire(&mut self) -> PortResult<Pass> {
        match self
            .coordination
            .acquire(&self.name, &self.instance, self.ttl_millis)?
        {
            Acquisition::Granted(lease) => {
                let token = lease.token;
                self.held = Some(lease);
                Ok(Pass::Ran(token))
            }
            Acquisition::Held { holder, .. } => Ok(Pass::Standby { holder }),
        }
    }
}

#[cfg(test)]
mod tests;
