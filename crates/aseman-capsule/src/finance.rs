//! The finance ledger on the storage module (ADR 0036).
//!
//! Each finance family is a model whose payload stays a document (the use cases edit
//! it as a JSON object) and whose queried fields are real columns, derived from the
//! document on every write: a hold's payer and creation time, a payout's user, a
//! pool's payer, a reservation's pool. Listings (`hold_ids_by_payer`, ...) are indexed
//! queries on those columns, newest first. Wallet counters are the integer fields of
//! one `core.finance_account` per user; idempotency markers are `core.marker` rows.

use aseman_contracts::documents::merge_objects;
use aseman_ports::finance_ledger::{FinanceDoc, FinanceLedger, FinanceMarker, WalletCounter};
use aseman_ports::{PortError, PortResult};
use aseman_storage::client::core::{
    billing_catalog, billing_quote, finance_account, finance_hold, finance_journal,
    finance_journal_participant, finance_live_debit, finance_payout, finance_pool,
    finance_pool_reservation, finance_project_budget, legacy_identity, marker, namespace_document,
    user,
};
use aseman_storage::{FindMany, Models, StorageError, Trx, Where};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// Rows one listing step reads.
const PAGE: u64 = 1_000;

fn storage(error: StorageError) -> PortError {
    match error {
        StorageError::Conflict(_) => PortError::Conflict,
        StorageError::NotFound(_) => PortError::NotFound,
        other => PortError::Failed(other.to_string()),
    }
}

fn text(document: &Map<String, Value>, field: &str) -> Option<String> {
    document
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn millis(document: &Map<String, Value>) -> i64 {
    document
        .get("createdAt")
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn object(value: &Value) -> PortResult<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map.clone()),
        _ => Err(PortError::failed(
            "a finance document must be a JSON object",
        )),
    }
}

/// Where a shared (path-addressed) document lives: its namespace key and path.
fn shared(family: FinanceDoc, id: &str) -> Option<(String, String)> {
    match family {
        FinanceDoc::BillingCurrent => Some(("billing".to_owned(), "current".to_owned())),
        FinanceDoc::BillingNodes => Some(("billing".to_owned(), "nodes".to_owned())),
        FinanceDoc::Market => Some(("market".to_owned(), id.to_owned())),
        _ => None,
    }
}

/// The lock a `Json::Creature` path names: `lockedTokens.{lock}` (a `core.token_lock`).
fn lock_of(path: &str) -> PortResult<&str> {
    path.strip_prefix(crate::token_lock::LOCKED_TOKENS)
        .and_then(|rest| rest.strip_prefix('.'))
        .filter(|lock| !lock.is_empty() && !lock.contains('.'))
        .ok_or_else(|| PortError::failed("a creature finance document is a token lock"))
}

fn account_field(kind: WalletCounter) -> &'static str {
    match kind {
        WalletCounter::Held => "held_minor",
        WalletCounter::Debt => "debt_minor",
        WalletCounter::Withdrawable => "withdrawable_minor",
        WalletCounter::PayoutHeld => "payout_held_minor",
        WalletCounter::Earned => "earned_minor",
        WalletCounter::Spent => "spent_minor",
    }
}

fn account_value(account: &finance_account::FinanceAccount, kind: WalletCounter) -> Option<i64> {
    match kind {
        WalletCounter::Held => account.held_minor,
        WalletCounter::Debt => account.debt_minor,
        WalletCounter::Withdrawable => account.withdrawable_minor,
        WalletCounter::PayoutHeld => account.payout_held_minor,
        WalletCounter::Earned => account.earned_minor,
        WalletCounter::Spent => account.spent_minor,
    }
}

fn marker_key(marker: &FinanceMarker) -> String {
    match marker {
        FinanceMarker::HoldRequest { payer, key } => format!("FinanceHoldRequest::{payer}::{key}"),
        FinanceMarker::Run { authority, run_id } => format!("FinanceRun::{authority}::{run_id}"),
        FinanceMarker::Settlement {
            authority,
            settlement_id,
        } => format!("FinanceSettlement::{authority}::{settlement_id}"),
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
        } => format!("FinancePoolSettlement::{authority}::{settlement_id}"),
        FinanceMarker::PoolDebit {
            authority,
            debit_id,
        } => format!("FinancePoolDebit::{authority}::{debit_id}"),
        FinanceMarker::PaymentAdjustment { key } => format!("PaymentAdjustment::{key}"),
        FinanceMarker::MintApplied { key } => format!("MintApplied::{key}"),
    }
}

/// The finance ledger of one storage transaction.
pub struct StorageFinanceLedger<'a> {
    pub trx: &'a Trx,
}

macro_rules! document_family {
    ($self:ident, $module:ident, $accessor:ident, $id:expr) => {
        $self
            .trx
            .$accessor()
            .find_unique($module::by_key($id))
            .map_err(storage)?
            .map(|record| record.document)
    };
}

impl StorageFinanceLedger<'_> {
    /// The stored document of an id-scoped family.
    fn document(&self, family: FinanceDoc, id: &str) -> PortResult<Option<Value>> {
        Ok(match family {
            FinanceDoc::Hold => document_family!(self, finance_hold, finance_hold, id),
            FinanceDoc::Pool => document_family!(self, finance_pool, finance_pool, id),
            FinanceDoc::PoolReservation => {
                document_family!(self, finance_pool_reservation, finance_pool_reservation, id)
            }
            FinanceDoc::LiveDebit => {
                document_family!(self, finance_live_debit, finance_live_debit, id)
            }
            FinanceDoc::ProjectBudget => {
                document_family!(self, finance_project_budget, finance_project_budget, id)
            }
            FinanceDoc::Journal => document_family!(self, finance_journal, finance_journal, id),
            FinanceDoc::Payout => document_family!(self, finance_payout, finance_payout, id),
            FinanceDoc::BillingCatalog => {
                document_family!(self, billing_catalog, billing_catalog, id)
            }
            FinanceDoc::BillingQuote => document_family!(self, billing_quote, billing_quote, id),
            FinanceDoc::BillingCurrent
            | FinanceDoc::BillingNodes
            | FinanceDoc::Market
            | FinanceDoc::Creature => None,
        })
    }

    /// Store the whole document of an id-scoped family, with its derived columns.
    fn store(&self, family: FinanceDoc, id: &str, document: Map<String, Value>) -> PortResult<()> {
        let trx = self.trx;
        let value = Value::Object(document.clone());
        match family {
            FinanceDoc::Hold => {
                let payer = text(&document, "payerUserId").unwrap_or_default();
                let created = millis(&document);
                trx.finance_hold()
                    .upsert(
                        finance_hold::by_key(id),
                        finance_hold::Create {
                            key: id.to_owned(),
                            payer_ref: payer.clone(),
                            created_millis: created,
                            document: value.clone(),
                        },
                        finance_hold::update()
                            .payer_ref(payer)
                            .created_millis(created)
                            .document(value),
                    )
                    .map(drop)
            }
            FinanceDoc::Pool => {
                let payer = text(&document, "payerUserId");
                trx.finance_pool()
                    .upsert(
                        finance_pool::by_key(id),
                        finance_pool::Create {
                            key: id.to_owned(),
                            payer_ref: payer.clone(),
                            document: value.clone(),
                        },
                        finance_pool::update().payer_ref(payer).document(value),
                    )
                    .map(drop)
            }
            FinanceDoc::PoolReservation => {
                let pool = text(&document, "poolId");
                trx.finance_pool_reservation()
                    .upsert(
                        finance_pool_reservation::by_key(id),
                        finance_pool_reservation::Create {
                            key: id.to_owned(),
                            pool_ref: pool.clone(),
                            document: value.clone(),
                        },
                        finance_pool_reservation::update()
                            .pool_ref(pool)
                            .document(value),
                    )
                    .map(drop)
            }
            FinanceDoc::LiveDebit => trx
                .finance_live_debit()
                .upsert(
                    finance_live_debit::by_key(id),
                    finance_live_debit::Create {
                        key: id.to_owned(),
                        document: value.clone(),
                    },
                    finance_live_debit::update().document(value),
                )
                .map(drop),
            FinanceDoc::ProjectBudget => trx
                .finance_project_budget()
                .upsert(
                    finance_project_budget::by_key(id),
                    finance_project_budget::Create {
                        key: id.to_owned(),
                        document: value.clone(),
                    },
                    finance_project_budget::update().document(value),
                )
                .map(drop),
            FinanceDoc::Journal => {
                let kind = text(&document, "kind").unwrap_or_default();
                let hold = text(&document, "holdId");
                let payer = text(&document, "payerUserId");
                let created = millis(&document);
                trx.finance_journal()
                    .upsert(
                        finance_journal::by_key(id),
                        finance_journal::Create {
                            key: id.to_owned(),
                            entry_kind: kind.clone(),
                            hold_ref: hold.clone(),
                            payer_ref: payer.clone(),
                            created_millis: created,
                            document: value.clone(),
                        },
                        finance_journal::update()
                            .entry_kind(kind)
                            .hold_ref(hold)
                            .payer_ref(payer)
                            .created_millis(created)
                            .document(value),
                    )
                    .map(drop)
            }
            FinanceDoc::Payout => {
                let user = text(&document, "userId").unwrap_or_default();
                let created = millis(&document);
                trx.finance_payout()
                    .upsert(
                        finance_payout::by_key(id),
                        finance_payout::Create {
                            key: id.to_owned(),
                            user_ref: user.clone(),
                            created_millis: created,
                            document: value.clone(),
                        },
                        finance_payout::update()
                            .user_ref(user)
                            .created_millis(created)
                            .document(value),
                    )
                    .map(drop)
            }
            FinanceDoc::BillingCatalog => trx
                .billing_catalog()
                .upsert(
                    billing_catalog::by_key(id),
                    billing_catalog::Create {
                        key: id.to_owned(),
                        document: value.clone(),
                    },
                    billing_catalog::update().document(value),
                )
                .map(drop),
            FinanceDoc::BillingQuote => trx
                .billing_quote()
                .upsert(
                    billing_quote::by_key(id),
                    billing_quote::Create {
                        key: id.to_owned(),
                        document: value.clone(),
                    },
                    billing_quote::update().document(value),
                )
                .map(drop),
            FinanceDoc::BillingCurrent
            | FinanceDoc::BillingNodes
            | FinanceDoc::Market
            | FinanceDoc::Creature => {
                return Err(PortError::failed("a shared document has a path"));
            }
        }
        .map_err(storage)
    }

    fn namespace(&self, key: &str) -> PortResult<Map<String, Value>> {
        Ok(self
            .trx
            .namespace_document()
            .find_unique(namespace_document::by_key(key))
            .map_err(storage)?
            .and_then(|record| record.document.as_object().cloned())
            .unwrap_or_default())
    }

    fn put_namespace(&self, key: &str, document: Map<String, Value>) -> PortResult<()> {
        let value = Value::Object(document);
        self.trx
            .namespace_document()
            .upsert(
                namespace_document::by_key(key),
                namespace_document::Create {
                    key: key.to_owned(),
                    document: value.clone(),
                },
                namespace_document::update().document(value),
            )
            .map(drop)
            .map_err(storage)
    }

    /// The `core.user` of the human creature `user_id`, if it has one.
    fn user_of(&self, user_id: &str) -> PortResult<Option<aseman_storage::Id>> {
        Ok(self
            .trx
            .legacy_identity()
            .find_unique(legacy_identity::by_family_and_legacy_id("User", user_id))
            .map_err(storage)?
            .map(|identity| identity.target_id))
    }

    fn account(&self, user: &str) -> PortResult<Option<finance_account::FinanceAccount>> {
        self.trx
            .finance_account()
            .find_unique(finance_account::by_key(user))
            .map_err(storage)
    }

    fn put_account_field(
        &self,
        user: &str,
        field: &str,
        value: aseman_storage::Value,
    ) -> PortResult<()> {
        let update = aseman_storage::Data::from([(field.to_owned(), value)]);
        let mut create = update.clone();
        create.insert("key".to_owned(), aseman_storage::Value::from(user));
        self.trx
            .upsert(
                finance_account::NAME,
                &finance_account::by_key(user),
                create,
                update,
            )
            .map(drop)
            .map_err(storage)
    }

    /// Record keys of a keyed listing, newest first.
    fn newest_keys(
        &self,
        model: &str,
        user_field: &str,
        user: &str,
        limit: usize,
    ) -> PortResult<Vec<String>> {
        let rows = self
            .trx
            .find_many(
                model,
                &FindMany::filter(Where::eq(user_field, user))
                    .order_by(aseman_storage::Order::desc("created_millis"))
                    .order_by(aseman_storage::Order::desc("key"))
                    .take(limit as u64),
            )
            .map_err(storage)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| row.text("key").map(str::to_owned))
            .collect())
    }

    /// Every record key of a model, in key order.
    fn all_keys(&self, model: &str) -> PortResult<Vec<String>> {
        let mut keys = Vec::new();
        loop {
            let page = self
                .trx
                .find_many(
                    model,
                    &FindMany::default()
                        .order_by(aseman_storage::Order::asc("key"))
                        .skip(keys.len() as u64)
                        .take(PAGE),
                )
                .map_err(storage)?;
            let done = (page.len() as u64) < PAGE;
            keys.extend(
                page.into_iter()
                    .filter_map(|row| row.text("key").map(str::to_owned)),
            );
            if done {
                return Ok(keys);
            }
        }
    }
}

impl FinanceLedger for StorageFinanceLedger<'_> {
    fn get_doc(&self, family: FinanceDoc, id: &str, path: &str) -> PortResult<Map<String, Value>> {
        if family == FinanceDoc::Creature {
            return crate::token_lock::lock(self.trx, id, lock_of(path)?)
                .map_err(storage)?
                .ok_or(PortError::NotFound);
        }
        if let Some((key, path)) = shared(family, id) {
            return match self.namespace(&key)?.get(&path) {
                Some(Value::Object(object)) => Ok(object.clone()),
                _ => Err(PortError::NotFound),
            };
        }
        match self.document(family, id)? {
            Some(Value::Object(object)) => Ok(object),
            _ => Err(PortError::NotFound),
        }
    }

    fn put_doc(
        &self,
        family: FinanceDoc,
        id: &str,
        path: &str,
        value: &Value,
        merge: bool,
    ) -> PortResult<()> {
        let incoming = object(value)?;
        if family == FinanceDoc::Creature {
            return crate::token_lock::put_lock(self.trx, id, lock_of(path)?, &incoming, merge)
                .map_err(storage);
        }
        if let Some((key, path)) = shared(family, id) {
            let mut namespace = self.namespace(&key)?;
            let next = match (merge, namespace.get(&path)) {
                (true, Some(Value::Object(existing))) => {
                    let mut merged = existing.clone();
                    merge_objects(&mut merged, &incoming);
                    merged
                }
                _ => incoming,
            };
            namespace.insert(path, Value::Object(next));
            return self.put_namespace(&key, namespace);
        }
        let next = match (merge, self.document(family, id)?) {
            (true, Some(Value::Object(mut existing))) => {
                merge_objects(&mut existing, &incoming);
                existing
            }
            _ => incoming,
        };
        self.store(family, id, next)
    }

    fn doc_ids(&self, family: FinanceDoc) -> PortResult<Vec<String>> {
        let model = match family {
            FinanceDoc::Hold => finance_hold::NAME,
            FinanceDoc::Pool => finance_pool::NAME,
            FinanceDoc::PoolReservation => finance_pool_reservation::NAME,
            FinanceDoc::LiveDebit => finance_live_debit::NAME,
            FinanceDoc::ProjectBudget => finance_project_budget::NAME,
            FinanceDoc::Journal => finance_journal::NAME,
            FinanceDoc::Payout => finance_payout::NAME,
            FinanceDoc::BillingCatalog => billing_catalog::NAME,
            FinanceDoc::BillingQuote => billing_quote::NAME,
            _ => return Ok(Vec::new()),
        };
        self.all_keys(model)
    }

    fn counter(&self, kind: WalletCounter, user: &str) -> PortResult<i64> {
        let value = self
            .account(user)?
            .and_then(|account| account_value(&account, kind))
            .unwrap_or(0);
        if value < 0 {
            return Err(PortError::failed("invalid finance counter"));
        }
        Ok(value)
    }

    fn set_counter(&self, kind: WalletCounter, user: &str, amount: i64) -> PortResult<()> {
        if amount < 0 {
            return Err(PortError::Denied("finance counter underflow"));
        }
        self.put_account_field(
            user,
            account_field(kind),
            aseman_storage::Value::Int(amount),
        )
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
            .ok_or_else(|| PortError::failed("finance counter overflow"))?;
        self.set_counter(kind, user, next)?;
        Ok(next)
    }

    fn counter_links(&self, kind: WalletCounter) -> PortResult<Vec<(String, String)>> {
        let field = account_field(kind);
        let mut out = Vec::new();
        loop {
            let page = self
                .trx
                .finance_account()
                .find_many(
                    FindMany::filter(Where::field(field, aseman_storage::Cond::IsNull(false)))
                        .order_by(finance_account::key().asc())
                        .skip(out.len() as u64)
                        .take(PAGE),
                )
                .map_err(storage)?;
            let done = (page.len() as u64) < PAGE;
            for account in page {
                if let Some(value) = account_value(&account, kind) {
                    out.push((account.key.clone(), value.to_string()));
                }
            }
            if done {
                return Ok(out);
            }
        }
    }

    fn marker(&self, marker: &FinanceMarker) -> PortResult<String> {
        Ok(self
            .trx
            .marker()
            .find_unique(marker::by_key(marker_key(marker)))
            .map_err(storage)?
            .map(|record| record.value)
            .unwrap_or_default())
    }

    fn put_marker(&self, marker: &FinanceMarker, value: &str) -> PortResult<()> {
        let key = marker_key(marker);
        self.trx
            .marker()
            .upsert(
                marker::by_key(key.clone()),
                marker::Create {
                    key,
                    value: value.to_owned(),
                },
                marker::update().value(value),
            )
            .map(drop)
            .map_err(storage)
    }

    fn hold_ids_by_payer(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        self.newest_keys(finance_hold::NAME, "payer_ref", user, limit)
    }

    fn journal_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        let rows = self
            .trx
            .finance_journal_participant()
            .find_many(
                FindMany::filter(finance_journal_participant::participant_ref().eq(user))
                    .order_by(finance_journal_participant::created_millis().desc())
                    .order_by(finance_journal_participant::journal_ref().desc())
                    .take(limit as u64),
            )
            .map_err(storage)?;
        Ok(rows.into_iter().map(|row| row.journal_ref).collect())
    }

    fn payout_ids_by_user(&self, user: &str, limit: usize) -> PortResult<Vec<String>> {
        self.newest_keys(finance_payout::NAME, "user_ref", user, limit)
    }

    fn pool_of_user(&self, user: &str) -> PortResult<String> {
        Ok(self
            .account(user)?
            .and_then(|account| account.pool_ref)
            .unwrap_or_default())
    }

    fn put_pool_of_user(&self, user: &str, pool_id: &str) -> PortResult<()> {
        self.put_account_field(
            user,
            "pool_ref",
            aseman_storage::Value::Text(pool_id.to_owned()),
        )
    }

    fn email_to_id(&self, email: &str) -> PortResult<String> {
        let Some(found) = self
            .trx
            .user()
            .find_unique(user::by_email(email))
            .map_err(storage)?
        else {
            return Ok(String::new());
        };
        Ok(self
            .trx
            .legacy_identity()
            .find_unique(legacy_identity::by_target_kind_and_target_id(
                user::NAME,
                found.id,
            ))
            .map_err(storage)?
            .map(|identity| identity.legacy_id)
            .unwrap_or_default())
    }

    fn put_email_to_id(&self, email: &str, user_id: &str) -> PortResult<()> {
        // The address is the user's own `core.user` field: one address per user, and
        // a unique index keeps an address with one user.
        let target = self.user_of(user_id)?.ok_or(PortError::NotFound)?;
        self.trx
            .user()
            .update(
                user::by_id(target),
                user::update().email(Some(email.to_owned())),
            )
            .map(drop)
            .map_err(storage)
    }

    fn id_to_email(&self, user_id: &str) -> PortResult<String> {
        let Some(target) = self.user_of(user_id)? else {
            return Ok(String::new());
        };
        Ok(self
            .trx
            .user()
            .find_unique(user::by_id(target))
            .map_err(storage)?
            .and_then(|found| found.email)
            .unwrap_or_default())
    }

    fn put_id_to_email(&self, user_id: &str, email: &str) -> PortResult<()> {
        self.put_email_to_id(email, user_id)
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
        let journal_id = self.gen_id();
        let entry = json!({
            "journalId": journal_id,
            "kind": kind,
            "holdId": hold_id,
            "payerUserId": payer_id,
            "createdAt": now,
            "payload": payload,
        });
        self.store(FinanceDoc::Journal, &journal_id, object(&entry)?)?;
        let mut seen = BTreeSet::new();
        for participant in participants {
            if participant.is_empty() || !seen.insert(participant.as_str()) {
                continue;
            }
            self.trx
                .finance_journal_participant()
                .create(finance_journal_participant::Create {
                    journal_ref: journal_id.clone(),
                    participant_ref: participant.clone(),
                    created_millis: now,
                })
                .map_err(storage)?;
        }
        Ok(journal_id)
    }

    fn gen_id(&self) -> String {
        format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_storage::{Mode, Storage};

    fn storage() -> Storage {
        Storage::new(
            aseman_storage::memory::MemoryProvider::new(),
            aseman_storage::schema::Schema::catalog().unwrap(),
        )
    }

    #[test]
    fn documents_counters_markers_and_listings_round_trip() {
        let storage = storage();
        let trx = storage.begin(Mode::ReadWrite).unwrap();
        let ledger = StorageFinanceLedger { trx: &trx };
        for (id, at) in [("h1", 10), ("h2", 30), ("h3", 20)] {
            ledger
                .put_doc(
                    FinanceDoc::Hold,
                    id,
                    "hold",
                    &json!({"holdId": id, "payerUserId": "u1", "createdAt": at, "amount": 5}),
                    false,
                )
                .unwrap();
        }
        ledger
            .put_doc(
                FinanceDoc::Hold,
                "h1",
                "hold",
                &json!({"state": "held"}),
                true,
            )
            .unwrap();
        let h1 = ledger.get_doc(FinanceDoc::Hold, "h1", "hold").unwrap();
        assert_eq!(
            (h1["amount"].clone(), h1["state"].clone()),
            (json!(5), json!("held"))
        );
        assert_eq!(ledger.hold_ids_by_payer("u1", 2).unwrap(), ["h2", "h3"]);
        assert_eq!(
            ledger.doc_ids(FinanceDoc::Hold).unwrap(),
            ["h1", "h2", "h3"]
        );
        assert!(matches!(
            ledger.get_doc(FinanceDoc::Pool, "missing", "pool"),
            Err(PortError::NotFound)
        ));

        assert_eq!(ledger.counter(WalletCounter::Held, "u1").unwrap(), 0);
        assert_eq!(ledger.add_counter(WalletCounter::Held, "u1", 7).unwrap(), 7);
        ledger.set_counter(WalletCounter::Debt, "u1", 3).unwrap();
        ledger.add_counter(WalletCounter::Held, "u2", 1).unwrap();
        assert_eq!(
            ledger.counter_links(WalletCounter::Held).unwrap(),
            [
                ("u1".to_owned(), "7".to_owned()),
                ("u2".to_owned(), "1".to_owned())
            ]
        );
        assert_eq!(ledger.counter_links(WalletCounter::Debt).unwrap().len(), 1);

        let marker = FinanceMarker::PoolClose {
            pool_id: "p1".to_owned(),
        };
        assert_eq!(ledger.marker(&marker).unwrap(), "");
        ledger.put_marker(&marker, "c1").unwrap();
        assert_eq!(ledger.marker(&marker).unwrap(), "c1");

        ledger
            .put_doc(FinanceDoc::BillingNodes, "", "", &json!({"n1": {}}), false)
            .unwrap();
        ledger
            .put_doc(FinanceDoc::BillingCurrent, "", "", &json!({"v": 1}), false)
            .unwrap();
        assert_eq!(
            ledger.get_doc(FinanceDoc::BillingNodes, "", "").unwrap(),
            json!({"n1": {}}).as_object().unwrap().clone()
        );

        let journal = ledger
            .write_journal(
                "hold",
                "h1",
                "u1",
                json!({}),
                &["u1".into(), "u2".into(), "u1".into()],
                5,
            )
            .unwrap();
        assert_eq!(
            ledger.journal_ids_by_user("u2", 10).unwrap(),
            std::slice::from_ref(&journal)
        );
        assert_eq!(
            ledger
                .get_doc(FinanceDoc::Journal, &journal, "entry")
                .unwrap()["kind"],
            json!("hold")
        );

        // Emails are the human user's own `core.user` address.
        assert!(ledger.put_email_to_id("a@x.io", "nobody").is_err());
        let seeded = trx
            .user()
            .create(user::Create {
                username: "u1-name".to_owned(),
                email: None,
                public_key: vec![1],
                status: "active".to_owned(),
            })
            .unwrap();
        trx.legacy_identity()
            .create(legacy_identity::Create {
                family: "User".to_owned(),
                legacy_id: "u1".to_owned(),
                target_kind: user::NAME.to_owned(),
                target_id: seeded.id,
            })
            .unwrap();
        ledger.put_email_to_id("a@x.io", "u1").unwrap();
        ledger.put_email_to_id("b@x.io", "u1").unwrap();
        assert_eq!(ledger.email_to_id("a@x.io").unwrap(), "");
        assert_eq!(ledger.email_to_id("b@x.io").unwrap(), "u1");
        assert_eq!(ledger.id_to_email("u1").unwrap(), "b@x.io");
        ledger.put_pool_of_user("u1", "p9").unwrap();
        assert_eq!(ledger.pool_of_user("u1").unwrap(), "p9");
        assert_eq!(ledger.counter(WalletCounter::Held, "u1").unwrap(), 7);
        trx.commit().unwrap();
    }
}
