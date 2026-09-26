//! The finance ledger port of one state action, implemented over the legacy
//! transaction (RL-004 finance strangler slice).
//!
//! Key encodings are exactly the legacy finance module's: JSON documents under
//! `Json::*` paths, integer counters and idempotency markers as `Finance*` links, and
//! the `Finance*ByUser`/`Finance*ByPayer` index links. Finance stays on legacy per
//! ADR 0026 until Phase 8, so there is no capsule branch here.

use std::collections::HashMap;

use aseman_ports::finance_ledger::{FinanceDoc, FinanceLedger, FinanceMarker, WalletCounter};
use aseman_ports::{PortError, PortResult};
use serde_json::{Map, Value, json};

use crate::api::utils::crypto::secure_unique_string;
use crate::models::transaction::ITrx;

/// The finance ledger adapter over one legacy transaction.
pub(crate) struct FinanceLedgerPorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

/// The legacy JSON document key + path for a finance family.
fn doc_key_path(family: FinanceDoc, id: &str, path: &str) -> (String, String) {
    match family {
        FinanceDoc::Hold => (format!("Json::FinanceHold::{id}"), path.to_string()),
        FinanceDoc::Pool => (format!("Json::FinancePool::{id}"), path.to_string()),
        FinanceDoc::PoolReservation => (
            format!("Json::FinancePoolReservation::{id}"),
            path.to_string(),
        ),
        FinanceDoc::LiveDebit => (format!("Json::FinanceLiveDebit::{id}"), path.to_string()),
        FinanceDoc::ProjectBudget => (
            format!("Json::FinanceProjectBudget::{id}"),
            path.to_string(),
        ),
        FinanceDoc::Journal => (format!("Json::FinanceJournal::{id}"), path.to_string()),
        FinanceDoc::Payout => (format!("Json::FinancePayout::{id}"), path.to_string()),
        FinanceDoc::BillingCatalog => (format!("Json::BillingCatalog::{id}"), path.to_string()),
        FinanceDoc::BillingQuote => (format!("Json::BillingQuote::{id}"), path.to_string()),
        FinanceDoc::BillingCurrent => (
            "Json::CreatureNamespace::billing".to_string(),
            "current".to_string(),
        ),
        FinanceDoc::BillingNodes => (
            "Json::CreatureNamespace::billing".to_string(),
            "nodes".to_string(),
        ),
        // The market bucket is the path of the shared market document.
        FinanceDoc::Market => (
            "Json::CreatureNamespace::market".to_string(),
            id.to_string(),
        ),
        FinanceDoc::Creature => (format!("Json::Creature::{id}"), path.to_string()),
    }
}

/// The stored key prefix and path suffix used by the legacy `get_by_prefix` scans.
fn doc_scan(family: FinanceDoc) -> Option<(String, String)> {
    match family {
        FinanceDoc::Hold => Some((
            "json::Json::FinanceHold::".to_string(),
            "::hold".to_string(),
        )),
        FinanceDoc::Pool => Some((
            "json::Json::FinancePool::".to_string(),
            "::pool".to_string(),
        )),
        FinanceDoc::PoolReservation => Some((
            "json::Json::FinancePoolReservation::".to_string(),
            "::reservation".to_string(),
        )),
        FinanceDoc::LiveDebit => Some((
            "json::Json::FinanceLiveDebit::".to_string(),
            "::debit".to_string(),
        )),
        FinanceDoc::ProjectBudget => Some((
            "json::Json::FinanceProjectBudget::".to_string(),
            "::budget".to_string(),
        )),
        FinanceDoc::Journal => Some((
            "json::Json::FinanceJournal::".to_string(),
            "::entry".to_string(),
        )),
        FinanceDoc::Payout => Some((
            "json::Json::FinancePayout::".to_string(),
            "::payout".to_string(),
        )),
        FinanceDoc::BillingCatalog => Some((
            "json::Json::BillingCatalog::".to_string(),
            "::catalog".to_string(),
        )),
        FinanceDoc::BillingQuote => Some((
            "json::Json::BillingQuote::".to_string(),
            "::quote".to_string(),
        )),
        _ => None,
    }
}

fn wallet_counter_key(kind: WalletCounter, user: &str) -> String {
    let prefix = match kind {
        WalletCounter::Held => "FinanceHeld",
        WalletCounter::Debt => "FinanceDebt",
        WalletCounter::Withdrawable => "FinanceWithdrawable",
        WalletCounter::PayoutHeld => "FinancePayoutHeld",
        WalletCounter::Earned => "FinanceEarned",
        WalletCounter::Spent => "FinanceSpent",
    };
    format!("{prefix}::{user}")
}

fn counter_scan_prefix(kind: WalletCounter) -> &'static str {
    match kind {
        WalletCounter::Held => "FinanceHeld::",
        WalletCounter::Debt => "FinanceDebt::",
        WalletCounter::Withdrawable => "FinanceWithdrawable::",
        WalletCounter::PayoutHeld => "FinancePayoutHeld::",
        WalletCounter::Earned => "FinanceEarned::",
        WalletCounter::Spent => "FinanceSpent::",
    }
}

fn marker_key(marker: &FinanceMarker) -> String {
    match marker {
        FinanceMarker::HoldRequest { payer, key } => format!("FinanceHoldRequest::{payer}::{key}"),
        FinanceMarker::Run { authority, run_id } => format!("FinanceRun::{authority}::{run_id}"),
        FinanceMarker::Settlement {
            authority,
            settlement_id,
        } => {
            format!("FinanceSettlement::{authority}::{settlement_id}")
        }
        FinanceMarker::Release { caller, release_id } => {
            format!("FinanceRelease::{caller}::{release_id}")
        }
        FinanceMarker::PayoutRequest { user, request_id } => {
            format!("FinancePayoutRequest::{user}::{request_id}")
        }
        FinanceMarker::PayoutResolution { resolution_id } => {
            format!("FinancePayoutResolution::{resolution_id}")
        }
        FinanceMarker::PoolOpen { payer, key } => format!("FinancePoolOpen::{payer}::{key}"),
        FinanceMarker::PoolRefresh { payer, refresh_id } => {
            format!("FinancePoolRefresh::{payer}::{refresh_id}")
        }
        FinanceMarker::PoolClose { pool_id } => format!("FinancePoolClose::{pool_id}"),
        FinanceMarker::PoolSettlement {
            authority,
            settlement_id,
        } => {
            format!("FinancePoolSettlement::{authority}::{settlement_id}")
        }
        FinanceMarker::PoolDebit {
            authority,
            debit_id,
        } => {
            format!("FinancePoolDebit::{authority}::{debit_id}")
        }
        FinanceMarker::PaymentAdjustment { key } => format!("PaymentAdjustment::{key}"),
        FinanceMarker::MintApplied { key } => format!("MintApplied::{key}"),
    }
}

/// The value stored in a link index, newest first (the `::{now:020}::{id}` keys sort
/// lexicographically, so reverse order is newest-first).
fn latest_index_values(trx: &dyn ITrx, prefix: &str, limit: usize) -> Vec<String> {
    let mut keys = trx.get_links_list(prefix, -1, -1, &[]).unwrap_or_default();
    keys.sort();
    keys.reverse();
    keys.into_iter()
        .take(limit)
        .map(|key| trx.get_link(&key))
        .filter(|value| !value.is_empty())
        .collect()
}

impl FinanceLedger for FinanceLedgerPorts<'_> {
    fn get_doc(&self, family: FinanceDoc, id: &str, path: &str) -> PortResult<Map<String, Value>> {
        let (key, path) = doc_key_path(family, id, path);
        self.trx
            .get_json(&key, &path)
            .map_err(|_| PortError::NotFound)
    }

    fn put_doc(
        &self,
        family: FinanceDoc,
        id: &str,
        path: &str,
        value: &Value,
        merge: bool,
    ) -> PortResult<()> {
        let (key, path) = doc_key_path(family, id, path);
        self.trx
            .put_json(&key, &path, value, merge)
            .map_err(|error| failed(error.to_string()))
    }

    fn doc_ids(&self, family: FinanceDoc) -> PortResult<Vec<String>> {
        let Some((prefix, suffix)) = doc_scan(family) else {
            return Ok(Vec::new());
        };
        let mut ids = self
            .trx
            .get_by_prefix(&prefix)
            .into_iter()
            .filter_map(|key| {
                key.strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(&suffix))
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        ids.sort();
        Ok(ids)
    }

    fn counter(&self, kind: WalletCounter, user: &str) -> PortResult<i64> {
        let raw = self.trx.get_link(&wallet_counter_key(kind, user));
        if raw.is_empty() {
            return Ok(0);
        }
        let value = raw
            .parse::<i64>()
            .map_err(|_| failed("invalid finance counter"))?;
        if value < 0 {
            return Err(failed("invalid finance counter"));
        }
        Ok(value)
    }

    fn set_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<()> {
        if amount < 0 {
            return Err(PortError::Denied("finance counter underflow"));
        }
        self.trx
            .put_link(&wallet_counter_key(kind, user), &amount.to_string());
        Ok(())
    }

    fn add_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<i64> {
        if amount < 0 {
            return Err(PortError::Denied(
                "finance counter amount must be nonnegative",
            ));
        }
        let next = self
            .counter(kind, user)?
            .checked_add(amount)
            .ok_or_else(|| failed("finance counter overflow"))?;
        self.set_counter(kind, user, next)?;
        Ok(next)
    }

    fn counter_links(&self, kind: WalletCounter) -> PortResult<Vec<(String, String)>> {
        let prefix = counter_scan_prefix(kind);
        let mut out = Vec::new();
        for key in self
            .trx
            .get_links_list(prefix, -1, -1, &[])
            .unwrap_or_default()
        {
            if let Some(user) = key.strip_prefix(prefix).filter(|user| !user.is_empty()) {
                out.push((user.to_string(), self.trx.get_link(&key)));
            }
        }
        out.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(out)
    }

    fn marker(&self, marker: &FinanceMarker) -> PortResult<String> {
        Ok(self.trx.get_link(&marker_key(marker)))
    }

    fn put_marker(&self, marker: &FinanceMarker, value: &str) -> PortResult<()> {
        self.trx.put_link(&marker_key(marker), value);
        Ok(())
    }

    fn hold_ids_by_payer(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        Ok(latest_index_values(
            self.trx,
            &format!("FinanceHoldByPayer::{user}::"),
            limit,
        ))
    }

    fn journal_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        Ok(latest_index_values(
            self.trx,
            &format!("FinanceJournalByUser::{user}::"),
            limit,
        ))
    }

    fn payout_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        Ok(latest_index_values(
            self.trx,
            &format!("FinancePayoutByUser::{user}::"),
            limit,
        ))
    }

    fn pool_of_user(&self, user: &str) -> PortResult<String> {
        Ok(self.trx.get_link(&format!("FinancePoolByUser::{user}")))
    }

    fn put_pool_of_user(&self, user: &str, pool_id: &str) -> PortResult<()> {
        self.trx
            .put_link(&format!("FinancePoolByUser::{user}"), pool_id);
        Ok(())
    }

    fn email_to_id(&self, email: &str) -> PortResult<String> {
        Ok(self.trx.get_link(&format!("UserEmailToId::{email}")))
    }

    fn put_email_to_id(&self, email: &str, user_id: &str) -> PortResult<()> {
        self.trx
            .put_link(&format!("UserEmailToId::{email}"), user_id);
        Ok(())
    }

    fn id_to_email(&self, user_id: &str) -> PortResult<String> {
        Ok(self.trx.get_link(&format!("UserIdToEmail::{user_id}")))
    }

    fn put_id_to_email(&self, user_id: &str, email: &str) -> PortResult<()> {
        self.trx
            .put_link(&format!("UserIdToEmail::{user_id}"), email);
        Ok(())
    }

    fn write_journal(
        &self,
        kind: &str,
        hold_id: &str,
        payer_id: &str,
        payload: Value,
        participants: &[String],
        now: i64,
    ) -> PortResult<String> {
        let journal_id = secure_unique_string();
        let entry = json!({
            "journalId": journal_id,
            "kind": kind,
            "holdId": hold_id,
            "payerUserId": payer_id,
            "createdAt": now,
            "payload": payload,
        });
        self.trx
            .put_json(
                &format!("Json::FinanceJournal::{journal_id}"),
                "entry",
                &entry,
                false,
            )
            .map_err(|error| failed(error.to_string()))?;
        let mut indexed: HashMap<&str, bool> = HashMap::new();
        for participant in participants {
            if participant.is_empty() || indexed.insert(participant.as_str(), true).is_some() {
                continue;
            }
            self.trx.put_link(
                &format!("FinanceJournalByUser::{participant}::{now:020}::{journal_id}"),
                &journal_id,
            );
        }
        Ok(journal_id)
    }

    fn gen_id(&self) -> String {
        secure_unique_string()
    }
}

/// The finance ledger ports of one state action.
pub(crate) struct FinancePorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

impl FinancePorts<'_> {
    /// The finance ledger adapter over this action's transaction.
    pub(crate) fn ledger(&self) -> FinanceLedgerPorts<'_> {
        FinanceLedgerPorts { trx: self.trx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::rocksdb::trx::TrxWrapper;
    use crate::adapters::rocksdb::trx::tests::{StubCore, StubStorage};
    use crate::models::ports::IStorage;
    use std::sync::Arc;

    #[test]
    fn finance_ledger_documents_round_trip_with_legacy_keys() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let ledger = FinanceLedgerPorts { trx: &*trx };
        let hold = serde_json::json!({"holdId": "h1", "status": "open"});
        ledger
            .put_doc(FinanceDoc::Hold, "h1", "hold", &hold, false)
            .unwrap();
        assert_eq!(
            trx.get_json("Json::FinanceHold::h1", "hold").unwrap(),
            hold.as_object().cloned().unwrap()
        );
        assert_eq!(
            ledger.get_doc(FinanceDoc::Hold, "h1", "hold").unwrap(),
            hold.as_object().cloned().unwrap()
        );
        assert_eq!(ledger.doc_ids(FinanceDoc::Hold).unwrap(), ["h1"]);
        assert_eq!(
            ledger.get_doc(FinanceDoc::Hold, "missing", "hold"),
            Err(PortError::NotFound)
        );
    }

    #[test]
    fn finance_ledger_counters_and_markers_use_legacy_links() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let ledger = FinanceLedgerPorts { trx: &*trx };
        assert_eq!(ledger.counter(WalletCounter::Held, "u1").unwrap(), 0);
        ledger.set_counter(WalletCounter::Held, "u1", 5).unwrap();
        assert_eq!(ledger.counter(WalletCounter::Held, "u1").unwrap(), 5);
        assert_eq!(ledger.add_counter(WalletCounter::Held, "u1", 3).unwrap(), 8);
        assert_eq!(trx.get_link("FinanceHeld::u1"), "8");
        assert_eq!(
            ledger.counter_links(WalletCounter::Held).unwrap(),
            [("u1".to_string(), "8".to_string())]
        );

        let marker = FinanceMarker::HoldRequest {
            payer: "u1".to_string(),
            key: "k1".to_string(),
        };
        assert_eq!(ledger.marker(&marker).unwrap(), "");
        ledger.put_marker(&marker, "h1|hash").unwrap();
        assert_eq!(trx.get_link("FinanceHoldRequest::u1::k1"), "h1|hash");
        assert_eq!(ledger.marker(&marker).unwrap(), "h1|hash");
    }

    #[test]
    fn finance_ledger_index_links_are_newest_first() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let ledger = FinanceLedgerPorts { trx: &*trx };
        for (now, id) in [(100_i64, "h-old"), (200, "h-new")] {
            trx.put_link(&format!("FinanceHoldByPayer::u1::{now:020}::{id}"), id);
        }
        assert_eq!(
            ledger.hold_ids_by_payer("u1", 10).unwrap(),
            ["h-new", "h-old"]
        );
        ledger.put_pool_of_user("u1", "p1").unwrap();
        assert_eq!(ledger.pool_of_user("u1").unwrap(), "p1");
    }

    #[test]
    fn finance_ledger_writes_journals_with_participant_index() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let ledger = FinanceLedgerPorts { trx: &*trx };
        let id = ledger
            .write_journal(
                "hold.created",
                "h1",
                "u1",
                serde_json::json!({"entries": []}),
                &[
                    "u1".to_string(),
                    "u2".to_string(),
                    "u1".to_string(),
                    String::new(),
                ],
                123,
            )
            .unwrap();
        let entry = ledger.get_doc(FinanceDoc::Journal, &id, "entry").unwrap();
        assert_eq!(entry["kind"], "hold.created");
        assert_eq!(entry["holdId"], "h1");
        assert_eq!(entry["payerUserId"], "u1");
        // Participants are indexed, deduplicated, empty ids skipped.
        assert_eq!(
            ledger.journal_ids_by_user("u1", 10).unwrap(),
            vec![id.clone()]
        );
        assert_eq!(ledger.journal_ids_by_user("u2", 10).unwrap(), [id]);
    }
}
