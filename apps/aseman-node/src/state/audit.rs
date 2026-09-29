//! Policy decision audit (plan "Keys and audit"). Every enforcement decision is
//! appended to its actor's `audit.event` stream by a writer in its own transactions,
//! so a refused action rolling back never erases the record of its refusal. The
//! channel is bounded and applies backpressure: decisions are never dropped to keep
//! up.

use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, sync_channel};

use aseman_capsule::audit::CapsuleDecisionAudit;
use aseman_domain::authority::AuditRecord;
use aseman_ports::DecisionAudit;

const QUEUE: usize = 10_000;

/// The node's decision audit: a queue to the writer started over its storage.
#[derive(Default)]
pub(crate) struct AuditLog {
    sender: OnceLock<SyncSender<AuditRecord>>,
}

impl AuditLog {
    /// Start the audit writer (once, over the node's storage).
    pub(crate) fn start(&self, storage: aseman_storage::Storage) -> anyhow::Result<()> {
        let (sender, receiver) = sync_channel::<AuditRecord>(QUEUE);
        self.sender
            .set(sender)
            .map_err(|_| anyhow::anyhow!("decision audit is already started"))?;
        std::thread::Builder::new()
            .name("aseman-decision-audit".to_owned())
            .spawn(move || {
                let append = |record: &AuditRecord| -> anyhow::Result<()> {
                    let trx = storage.begin(aseman_storage::Mode::ReadWrite)?;
                    CapsuleDecisionAudit { repository: &trx }.record(record)?;
                    trx.commit()?;
                    Ok(())
                };
                for record in receiver {
                    let mut attempts: u32 = 0;
                    while let Err(error) = append(&record) {
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

    /// Record one decision (a no-op until the writer starts).
    pub(crate) fn record(&self, record: AuditRecord) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(record);
        }
    }
}
