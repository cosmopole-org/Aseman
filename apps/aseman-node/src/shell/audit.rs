//! Policy decision audit (plan "Keys and audit"). On PostgreSQL every enforcement
//! decision is appended to its actor's `audit.event` stream by a writer on its own
//! connection, so a refused action rolling back never erases the record of its
//! refusal. The channel is bounded and applies backpressure: decisions are never
//! dropped to keep up. The legacy provider has no audit store.

use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};

use aseman_capsule::audit::CapsuleDecisionAudit;
use aseman_domain::authority::AuditRecord;
use aseman_ports::DecisionAudit;
use aseman_storage_postgres::PostgresCapsuleRepository;

const QUEUE: usize = 10_000;

static SENDER: OnceLock<SyncSender<AuditRecord>> = OnceLock::new();

/// Start the audit writer (once, when the node runs on PostgreSQL).
pub(crate) fn install_postgres(repository: PostgresCapsuleRepository) -> anyhow::Result<()> {
    let (sender, receiver) = sync_channel::<AuditRecord>(QUEUE);
    SENDER
        .set(sender)
        .map_err(|_| anyhow::anyhow!("decision audit is already installed"))?;
    std::thread::Builder::new()
        .name("aseman-decision-audit".to_owned())
        .spawn(move || {
            let audit = CapsuleDecisionAudit {
                repository: &repository,
            };
            for record in receiver {
                let mut attempts: u32 = 0;
                while let Err(error) = audit.record(&record) {
                    attempts += 1;
                    eprintln!("[audit] append failed ({attempts}): {error}");
                    std::thread::sleep(std::time::Duration::from_millis(
                        100 * u64::from(attempts.min(50)),
                    ));
                }
            }
        })?;
    Ok(())
}

/// Record one decision (a no-op on the legacy provider).
pub(crate) fn record(record: AuditRecord) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(record);
    }
}
