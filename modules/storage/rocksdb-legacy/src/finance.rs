//! ADR 0017 legacy finance epoch: reconciled, immutable `finance.legacy_record` capsules.

use super::*;

pub const LEGACY_FINANCE_KIND: &str = "finance.legacy_record";

/// How the records under one legacy finance key divide into document roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinanceRoots {
    /// Each listed path is one root; the legacy key identifies the record.
    Fixed(&'static [&'static str]),
    /// Every `prefix.{id}` path is one root (for example `lockedTokens.{lock}`).
    PerId(&'static str),
}

#[derive(Clone, Copy, Debug)]
struct FinanceDocumentFamily {
    key: &'static str,
    /// `true` when `key` is a complete legacy key rather than an ID prefix.
    exact: bool,
    roots: FinanceRoots,
    record_family: &'static str,
}

const FINANCE_DOCUMENT_FAMILIES: [FinanceDocumentFamily; 13] = [
    FinanceDocumentFamily {
        key: "Json::FinanceHold::",
        exact: false,
        roots: FinanceRoots::Fixed(&["hold"]),
        record_family: "hold",
    },
    FinanceDocumentFamily {
        key: "Json::FinancePool::",
        exact: false,
        roots: FinanceRoots::Fixed(&["pool"]),
        record_family: "pool",
    },
    FinanceDocumentFamily {
        key: "Json::FinancePoolReservation::",
        exact: false,
        roots: FinanceRoots::Fixed(&["reservation"]),
        record_family: "pool_reservation",
    },
    FinanceDocumentFamily {
        key: "Json::FinanceLiveDebit::",
        exact: false,
        roots: FinanceRoots::Fixed(&["debit"]),
        record_family: "live_debit",
    },
    FinanceDocumentFamily {
        key: "Json::FinancePayout::",
        exact: false,
        roots: FinanceRoots::Fixed(&["payout"]),
        record_family: "payout",
    },
    FinanceDocumentFamily {
        key: "Json::FinanceJournal::",
        exact: false,
        roots: FinanceRoots::Fixed(&["entry"]),
        record_family: "journal_entry",
    },
    FinanceDocumentFamily {
        key: "Json::FinanceProjectBudget::",
        exact: false,
        roots: FinanceRoots::Fixed(&["budget"]),
        record_family: "project_budget",
    },
    FinanceDocumentFamily {
        key: "Json::BillingCatalog::",
        exact: false,
        roots: FinanceRoots::Fixed(&["catalog"]),
        record_family: "billing_catalog",
    },
    FinanceDocumentFamily {
        key: "Json::BillingQuote::",
        exact: false,
        roots: FinanceRoots::Fixed(&["quote"]),
        record_family: "billing_quote",
    },
    FinanceDocumentFamily {
        key: "Json::VmBilling::",
        exact: false,
        roots: FinanceRoots::Fixed(&["payment"]),
        record_family: "vm_billing",
    },
    FinanceDocumentFamily {
        key: "Json::Creature::",
        exact: false,
        roots: FinanceRoots::PerId("lockedTokens"),
        record_family: "token_lock",
    },
    FinanceDocumentFamily {
        key: "Json::CreatureNamespace::billing",
        exact: true,
        roots: FinanceRoots::Fixed(&["current", "nodes"]),
        record_family: "billing_namespace",
    },
    FinanceDocumentFamily {
        key: "Json::CreatureNamespace::market",
        exact: true,
        roots: FinanceRoots::Fixed(&["agents", "tools", "frontends"]),
        record_family: "market_namespace",
    },
];

/// Legacy idempotency marker link families and the record their value must name.
const FINANCE_MARKERS: [(&str, &str, MarkerTarget); 13] = [
    (
        "FinanceHoldRequest",
        "hold_request",
        MarkerTarget::ValueHead("hold"),
    ),
    ("FinanceRun", "hold_run", MarkerTarget::Value("hold")),
    (
        "FinanceSettlement",
        "hold_settlement",
        MarkerTarget::Value("hold"),
    ),
    (
        "FinanceRelease",
        "hold_release",
        MarkerTarget::Value("hold"),
    ),
    (
        "FinancePayoutRequest",
        "payout_request",
        MarkerTarget::ValueHead("payout"),
    ),
    (
        "FinancePayoutResolution",
        "payout_resolution",
        MarkerTarget::ValueHead("payout"),
    ),
    ("FinancePoolOpen", "pool_open", MarkerTarget::Value("pool")),
    (
        "FinancePoolRefresh",
        "pool_refresh",
        MarkerTarget::Value("pool"),
    ),
    ("FinancePoolClose", "pool_close", MarkerTarget::Key("pool")),
    (
        "FinancePoolSettlement",
        "pool_settlement",
        MarkerTarget::Value("pool_reservation"),
    ),
    (
        "FinancePoolDebit",
        "pool_debit",
        MarkerTarget::Value("live_debit"),
    ),
    (
        "PaymentAdjustment",
        "payment_adjustment",
        MarkerTarget::ValueTail("journal_entry"),
    ),
    // `/creatures/mint`: `{target}:{amount}:{journalId}`; losing it allows a double credit.
    (
        "MintApplied",
        "mint_applied",
        MarkerTarget::LastColonSegment("journal_entry"),
    ),
];

/// Where a marker names the record it deduplicates.
#[derive(Clone, Copy, Debug)]
enum MarkerTarget {
    /// The whole value is the record ID.
    Value(&'static str),
    /// The value is `{record}|{hash}`.
    ValueHead(&'static str),
    /// The value is `{hash}|{record}`.
    ValueTail(&'static str),
    /// The marker key suffix is the record ID.
    Key(&'static str),
    /// The value is `{..}:{..}:{record}`.
    LastColonSegment(&'static str),
}

/// Authoritative counters migrate; derived counters are rebuilt and compared.
const AUTHORITATIVE_COUNTERS: [(&str, &str); 2] = [
    ("FinanceDebt", "debt_counter"),
    ("FinanceWithdrawable", "withdrawable_counter"),
];
const DERIVED_COUNTERS: [&str; 4] = [
    "FinanceHeld",
    "FinancePayoutHeld",
    "FinanceSpent",
    "FinanceEarned",
];
const LISTING_LINKS: [&str; 4] = [
    "FinanceHoldByPayer",
    "FinanceJournalByUser",
    "FinancePayoutByUser",
    "FinancePoolByUser",
];

/// `true` when a `json::` key belongs to the ADR 0017 finance epoch.
#[must_use]
pub fn is_legacy_finance_document_key(key: &str) -> bool {
    finance_document_family(key).is_some()
}

/// `true` when a `link::` family belongs to the ADR 0017 finance epoch.
#[must_use]
pub fn is_legacy_finance_link_family(family: &str) -> bool {
    FINANCE_MARKERS.iter().any(|(name, _, _)| *name == family)
        || AUTHORITATIVE_COUNTERS
            .iter()
            .any(|(name, _)| *name == family)
        || DERIVED_COUNTERS.contains(&family)
        || LISTING_LINKS.contains(&family)
}

fn finance_document_family(key: &str) -> Option<(FinanceDocumentFamily, &str)> {
    FINANCE_DOCUMENT_FAMILIES.iter().find_map(|family| {
        if family.exact {
            (key == family.key).then_some((*family, ""))
        } else {
            key.strip_prefix(family.key)
                .filter(|id| !id.is_empty())
                .map(|id| (*family, id))
        }
    })
}

type FinanceRecords = BTreeMap<(String, String), Map<String, Value>>;

impl LegacySnapshotGraph {
    /// Export the reconciled legacy finance epoch (ADR 0017).
    pub(crate) fn transform_legacy_finance(
        &self,
        migration_time_micros: i64,
        finance: Option<&LegacyFinanceConfig>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut records = self.finance_documents()?;
        let mut derived: BTreeMap<&str, BTreeMap<String, i64>> = DERIVED_COUNTERS
            .iter()
            .map(|family| (*family, BTreeMap::new()))
            .collect();
        let mut listings = Vec::new();
        for (key, value) in &self.links {
            let (family, rest) = key.split_once("::").unwrap_or((key.as_str(), ""));
            if !is_legacy_finance_link_family(family) {
                continue;
            }
            if rest.is_empty() {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy finance link {key} has an empty identity"
                )));
            }
            let text = String::from_utf8(value.clone()).map_err(|_| {
                LegacyMigrationError::Invalid(format!("legacy finance link {key} is not UTF-8"))
            })?;
            if let Some((_, record_family)) = AUTHORITATIVE_COUNTERS
                .iter()
                .find(|(name, _)| *name == family)
            {
                let amount = parse_legacy_counter(key, &text)?;
                insert_finance_record(
                    &mut records,
                    record_family,
                    rest,
                    Map::from_iter([("amount".to_owned(), Value::from(amount))]),
                )?;
            } else if let Some(counters) = derived.get_mut(family) {
                counters.insert(rest.to_owned(), parse_legacy_counter(key, &text)?);
            } else if let Some((_, record_family, target)) =
                FINANCE_MARKERS.iter().find(|(name, _, _)| *name == family)
            {
                if text.is_empty() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy finance marker {key} is empty"
                    )));
                }
                insert_finance_record(
                    &mut records,
                    record_family,
                    rest,
                    Map::from_iter([
                        ("value".to_owned(), Value::from(text.clone())),
                        (
                            "target".to_owned(),
                            Value::from(marker_target_label(*target)),
                        ),
                    ]),
                )?;
            } else {
                listings.push((family, rest, text));
            }
        }
        for (family, rest, value) in &listings {
            verify_finance_listing(&records, family, rest, value)?;
        }
        for ((record_family, legacy_key), document) in &records {
            if let Some((_, _, target)) = FINANCE_MARKERS
                .iter()
                .find(|(_, name, _)| name == record_family)
            {
                verify_finance_marker(&records, record_family, legacy_key, document, *target)?;
            }
        }
        self.reconcile_legacy_finance(&records, &derived)?;
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let finance = finance.ok_or_else(|| LegacyMigrationError::Unmapped {
            family: "finance.legacy_record.finance_config".to_owned(),
            key: "legacy finance epoch".to_owned(),
        })?;
        validate_finance_config(finance)?;
        records
            .into_iter()
            .map(|((record_family, legacy_key), document)| {
                seal_legacy_finance_record(
                    &record_family,
                    &legacy_key,
                    document,
                    finance,
                    migration_time_micros,
                )
            })
            .collect()
    }

    fn finance_documents(&self) -> LegacyMigrationResult<FinanceRecords> {
        let mut records = FinanceRecords::new();
        for (key, paths) in &self.documents {
            let Some((family, id)) = finance_document_family(key) else {
                continue;
            };
            let mut roots: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
            for (path, value) in paths {
                let root = finance_document_root(family.roots, path).ok_or_else(|| {
                    LegacyMigrationError::Unmapped {
                        family: "json-document-path".to_owned(),
                        key: format!("json::{key}::{path}"),
                    }
                })?;
                roots
                    .entry(root)
                    .or_default()
                    .insert(path.clone(), value.clone());
            }
            for (root, root_records) in roots {
                let document = verified_legacy_document(key, &root, &root_records)?;
                let legacy_key = match family.roots {
                    FinanceRoots::PerId(prefix) => {
                        format!("{id}::{}", &root[prefix.len() + 1..])
                    }
                    FinanceRoots::Fixed(_) if family.exact => root.clone(),
                    FinanceRoots::Fixed(_) => id.to_owned(),
                };
                insert_finance_record(&mut records, family.record_family, &legacy_key, document)?;
            }
        }
        Ok(records)
    }

    /// Enforce every invariant of the legacy `/creatures/reconcileFinancialSystem`
    /// action; any reported issue fails the export instead of being migrated.
    fn reconcile_legacy_finance(
        &self,
        records: &FinanceRecords,
        derived: &BTreeMap<&str, BTreeMap<String, i64>>,
    ) -> LegacyMigrationResult<()> {
        let mut issues = FinanceIssues::default();
        let mut held = BTreeMap::new();
        let mut spent = BTreeMap::new();
        let mut earned = BTreeMap::new();
        let mut payout_held = BTreeMap::new();
        let mut project_reserved = BTreeMap::new();
        let mut project_spent = BTreeMap::new();
        let mut pool_reserved = BTreeMap::new();

        for (id, hold) in family_records(records, "hold") {
            let payer = text_field(hold, "payerUserId");
            let project = text_field(hold, "projectId");
            let max = money(&mut issues, id, hold, "maxAmount");
            let remaining = money(&mut issues, id, hold, "remainingAmount");
            if payer.is_empty() || max <= 0 {
                issues.push("hold.invalid", id, "payer or maxAmount is invalid");
                continue;
            }
            match text_field(hold, "status") {
                "open" | "running" => {
                    if remaining != max {
                        issues.push("hold.remaining_mismatch", id, "remaining differs from max");
                    }
                    add(&mut issues, &mut held, payer, max);
                    if !project.is_empty() {
                        add(&mut issues, &mut project_reserved, project, max);
                    }
                }
                "settled" => {
                    let actual = money(&mut issues, id, hold, "actualAmount");
                    let refunded = money(&mut issues, id, hold, "refundedAmount");
                    if remaining != 0
                        || actual < 0
                        || refunded < 0
                        || actual.checked_add(refunded) != Some(max)
                    {
                        issues.push("hold.settlement_mismatch", id, "amounts do not balance");
                        continue;
                    }
                    let lines = settlement_lines(&mut issues, id, hold, &mut earned);
                    if lines != actual {
                        issues.push("settlement.lines_mismatch", id, "lines differ from actual");
                    }
                    add(&mut issues, &mut spent, payer, actual);
                    if !project.is_empty() {
                        add(&mut issues, &mut project_spent, project, actual);
                    }
                }
                "released" | "expired" => {
                    let refunded = money(&mut issues, id, hold, "refundedAmount");
                    if remaining != 0 || refunded != max {
                        issues.push("hold.release_mismatch", id, "refund differs from max");
                    }
                }
                _ => issues.push("hold.status_invalid", id, "unknown status"),
            }
        }

        for (id, pool) in family_records(records, "pool") {
            let payer = text_field(pool, "payerUserId");
            let amounts = ["maxAmount", "remaining", "reserved", "spent", "refunded"]
                .map(|field| money(&mut issues, id, pool, field));
            let [max, remaining, reserved, used, refunded] = amounts;
            if payer.is_empty() || amounts.iter().any(|amount| *amount < 0) {
                issues.push("pool.invalid", id, "payer or pool amounts are invalid");
                continue;
            }
            let sum = remaining
                .checked_add(reserved)
                .and_then(|value| value.checked_add(used))
                .and_then(|value| value.checked_add(refunded));
            if sum != Some(max) {
                issues.push("pool.balance_mismatch", id, "amounts do not sum to max");
            }
            if text_field(pool, "status") == "open" {
                add(
                    &mut issues,
                    &mut held,
                    payer,
                    remaining.saturating_add(reserved),
                );
            }
        }

        for (id, reservation) in family_records(records, "pool_reservation") {
            let payer = text_field(reservation, "payerUserId");
            let pool = text_field(reservation, "poolId");
            let amount = money(&mut issues, id, reservation, "amount");
            if payer.is_empty() || pool.is_empty() || amount < 0 {
                issues.push("reservation.invalid", id, "reservation fields are invalid");
                continue;
            }
            match text_field(reservation, "status") {
                "reserved" => add(&mut issues, &mut pool_reserved, pool, amount),
                "settled" => {
                    let actual = money(&mut issues, id, reservation, "actualAmount");
                    if actual < 0 {
                        issues.push("reservation.settlement_invalid", id, "missing actual");
                        continue;
                    }
                    let lines = settlement_lines(&mut issues, id, reservation, &mut earned);
                    if lines != actual {
                        issues.push("reservation.lines_mismatch", id, "lines differ from actual");
                    }
                    add(&mut issues, &mut spent, payer, actual);
                }
                "released" => {}
                _ => issues.push("reservation.status_invalid", id, "unknown status"),
            }
        }

        for (id, debit) in family_records(records, "live_debit") {
            let payer = text_field(debit, "payerUserId");
            let charged = money(&mut issues, id, debit, "chargedTotal");
            if payer.is_empty() || charged < 0 {
                issues.push("livedebit.invalid", id, "live debit fields are invalid");
                continue;
            }
            let mut credited = 0_i64;
            if let Some(credits) = debit.get("credits").and_then(Value::as_object) {
                for (cap_key, value) in credits {
                    let user = cap_key.split('|').next().unwrap_or("");
                    let amount = exact_money(value).unwrap_or(-1);
                    if user.is_empty() || amount <= 0 {
                        issues.push("livedebit.credit_invalid", id, "invalid credit");
                        continue;
                    }
                    add(&mut issues, &mut earned, user, amount);
                    credited = credited.saturating_add(amount);
                }
            }
            if credited != charged {
                issues.push(
                    "livedebit.credit_mismatch",
                    id,
                    "credits differ from charged",
                );
            }
            add(&mut issues, &mut spent, payer, charged);
        }

        for (id, pool) in family_records(records, "pool") {
            let stored = exact_money(pool.get("reserved").unwrap_or(&Value::from(0))).unwrap_or(-1);
            if stored != pool_reserved.get(id).copied().unwrap_or(0) {
                issues.push(
                    "pool.reserved_mismatch",
                    id,
                    "reserved differs from reservations",
                );
            }
        }

        for (id, payout) in family_records(records, "payout") {
            if text_field(payout, "status") == "pending" {
                let user = text_field(payout, "userId");
                let amount = money(&mut issues, id, payout, "amount");
                if user.is_empty() || amount <= 0 {
                    issues.push(
                        "payout.invalid",
                        id,
                        "pending payout owner or amount is invalid",
                    );
                    continue;
                }
                add(&mut issues, &mut payout_held, user, amount);
            }
        }

        for (family, expected, code) in [
            ("FinanceHeld", &held, "held.mismatch"),
            ("FinancePayoutHeld", &payout_held, "payout.held_mismatch"),
            ("FinanceSpent", &spent, "spent.mismatch"),
            ("FinanceEarned", &earned, "earned.mismatch"),
        ] {
            let actual = &derived[family];
            for user in expected.keys().chain(actual.keys()) {
                if expected.get(user).copied().unwrap_or(0)
                    != actual.get(user).copied().unwrap_or(0)
                {
                    issues.push(code, user, "stored counter differs from its reconstruction");
                }
            }
        }

        let mut withdrawable_total = 0_i64;
        for (user, counter) in family_records(records, "withdrawable_counter") {
            let withdrawable = exact_money(&counter["amount"]).unwrap_or(-1);
            let balance = self
                .objects
                .get(&("Creature".to_owned(), user.to_owned()))
                .map(|columns| required_i64_le_column("Creature", columns, "balance"))
                .transpose()?;
            match balance {
                Some(balance) if (0..=balance).contains(&withdrawable) => {}
                _ => issues.push(
                    "withdrawable.invalid",
                    user,
                    "exceeds balance or has no creature",
                ),
            }
            withdrawable_total = withdrawable_total.saturating_add(withdrawable.max(0));
        }
        let earned_total = earned
            .values()
            .fold(0_i64, |sum, value| sum.saturating_add(*value));
        if withdrawable_total > earned_total {
            issues.push(
                "withdrawable.unbacked_total",
                "",
                "withdrawable exceeds lifetime earnings",
            );
        }

        let mut projects: BTreeSet<&str> = project_reserved
            .keys()
            .chain(project_spent.keys())
            .map(String::as_str)
            .collect();
        projects.extend(family_records(records, "project_budget").map(|(id, _)| id));
        for project in projects {
            let budget = records.get(&("project_budget".to_owned(), project.to_owned()));
            let stored = |field: &str| {
                budget
                    .and_then(|budget| budget.get(field))
                    .map_or(Some(0), exact_money)
                    .unwrap_or(-1)
            };
            if stored("reservedMinor") != project_reserved.get(project).copied().unwrap_or(0) {
                issues.push("project.reserved_mismatch", project, "reserved differs");
            }
            if stored("spentMinor") < project_spent.get(project).copied().unwrap_or(0) {
                issues.push("project.spent_undercount", project, "spent below minimum");
            }
        }
        issues.into_result()
    }
}

#[derive(Default)]
struct FinanceIssues(Vec<String>);

impl FinanceIssues {
    fn push(&mut self, code: &str, reference: &str, detail: &str) {
        self.0.push(format!("{code} [{reference}]: {detail}"));
    }

    fn into_result(self) -> LegacyMigrationResult<()> {
        if self.0.is_empty() {
            return Ok(());
        }
        let shown = self
            .0
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        Err(LegacyMigrationError::Invalid(format!(
            "legacy finance reconciliation failed with {} issue(s): {shown}",
            self.0.len()
        )))
    }
}

fn family_records<'a>(
    records: &'a FinanceRecords,
    family: &'a str,
) -> impl Iterator<Item = (&'a str, &'a Map<String, Value>)> + 'a {
    records
        .iter()
        .filter(move |((record_family, _), _)| record_family == family)
        .map(|((_, id), document)| (id.as_str(), document))
}

fn text_field<'a>(document: &'a Map<String, Value>, field: &str) -> &'a str {
    document.get(field).and_then(Value::as_str).unwrap_or("")
}

/// Money is an exact integer. Legacy readers truncate floats, but legacy writers never
/// produce them, so a float amount is anomalous state that must not be rounded.
fn exact_money(value: &Value) -> Option<i64> {
    value.as_i64()
}

/// Read a money field, reporting a present non-integer value; absent reads as -1 like legacy.
fn money(issues: &mut FinanceIssues, id: &str, document: &Map<String, Value>, field: &str) -> i64 {
    match document.get(field) {
        None => -1,
        Some(value) => exact_money(value).unwrap_or_else(|| {
            issues.push("amount.not_integer", id, field);
            -1
        }),
    }
}

fn add(issues: &mut FinanceIssues, totals: &mut BTreeMap<String, i64>, key: &str, amount: i64) {
    let total = totals.entry(key.to_owned()).or_insert(0);
    match total.checked_add(amount) {
        Some(next) => *total = next,
        None => issues.push("amount.overflow", key, "expected total overflows"),
    }
}

fn settlement_lines(
    issues: &mut FinanceIssues,
    id: &str,
    document: &Map<String, Value>,
    earned: &mut BTreeMap<String, i64>,
) -> i64 {
    let mut total = 0_i64;
    for line in document
        .get("settlementLines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let user = line.get("userId").and_then(Value::as_str).unwrap_or("");
        let amount = line.get("amount").and_then(exact_money).unwrap_or(-1);
        if amount <= 0 {
            issues.push("settlement.line_invalid", id, "invalid beneficiary line");
            continue;
        }
        add(issues, earned, user, amount);
        total = total.saturating_add(amount);
    }
    total
}

fn finance_document_root(roots: FinanceRoots, path: &str) -> Option<String> {
    match roots {
        FinanceRoots::Fixed(names) => {
            let root = path.split('.').next()?;
            names.contains(&root).then(|| root.to_owned())
        }
        FinanceRoots::PerId(prefix) => {
            let id = path.strip_prefix(prefix)?.strip_prefix('.')?;
            let id = id.split('.').next().filter(|id| !id.is_empty())?;
            Some(format!("{prefix}.{id}"))
        }
    }
}

fn insert_finance_record(
    records: &mut FinanceRecords,
    record_family: &str,
    legacy_key: &str,
    document: Map<String, Value>,
) -> LegacyMigrationResult<()> {
    if records
        .insert((record_family.to_owned(), legacy_key.to_owned()), document)
        .is_some()
    {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy finance {record_family} {legacy_key} appears twice"
        )));
    }
    Ok(())
}

fn parse_legacy_counter(key: &str, text: &str) -> LegacyMigrationResult<i64> {
    text.parse::<i64>()
        .ok()
        .filter(|amount| *amount >= 0)
        .ok_or_else(|| {
            LegacyMigrationError::Invalid(format!(
                "legacy finance counter {key} is not a nonnegative integer"
            ))
        })
}

fn marker_target_label(target: MarkerTarget) -> &'static str {
    match target {
        MarkerTarget::Value(family)
        | MarkerTarget::ValueHead(family)
        | MarkerTarget::ValueTail(family)
        | MarkerTarget::Key(family)
        | MarkerTarget::LastColonSegment(family) => family,
    }
}

fn verify_finance_marker(
    records: &FinanceRecords,
    record_family: &str,
    legacy_key: &str,
    document: &Map<String, Value>,
    target: MarkerTarget,
) -> LegacyMigrationResult<()> {
    let value = text_field(document, "value");
    let (family, id) = match target {
        MarkerTarget::Value(family) => (family, value),
        MarkerTarget::ValueHead(family) => (family, value.split_once('|').map_or("", |(id, _)| id)),
        MarkerTarget::ValueTail(family) => (family, value.split_once('|').map_or("", |(_, id)| id)),
        MarkerTarget::Key(family) => (family, legacy_key),
        MarkerTarget::LastColonSegment(family) => {
            (family, value.rsplit_once(':').map_or("", |(_, id)| id))
        }
    };
    let record = records.get(&(family.to_owned(), id.to_owned()));
    let hash_matches = match target {
        // A hold request marker also binds the request hash stored on the hold.
        MarkerTarget::ValueHead("hold") => record.is_some_and(|hold| {
            value.split_once('|').map(|(_, hash)| hash) == Some(text_field(hold, "requestHash"))
        }),
        _ => record.is_some(),
    };
    if id.is_empty() || !hash_matches {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy finance marker {record_family} {legacy_key} names no matching {family}"
        )));
    }
    Ok(())
}

fn verify_finance_listing(
    records: &FinanceRecords,
    family: &str,
    rest: &str,
    value: &str,
) -> LegacyMigrationResult<()> {
    let invalid = || {
        LegacyMigrationError::Invalid(format!(
            "legacy finance listing {family}::{rest} diverges from its record"
        ))
    };
    let (record_family, party_field, party, id) = if family == "FinancePoolByUser" {
        ("pool", "payerUserId", rest, value)
    } else {
        let mut parts = rest.splitn(3, "::");
        let (Some(party), Some(timestamp), Some(id)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        if timestamp.len() != 20
            || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
            || id != value
        {
            return Err(invalid());
        }
        match family {
            "FinanceHoldByPayer" => ("hold", "payerUserId", party, id),
            "FinancePayoutByUser" => ("payout", "userId", party, id),
            // Journal participants are not stored on the entry; existence is the invariant.
            _ => ("journal_entry", "", party, id),
        }
    };
    let record = records
        .get(&(record_family.to_owned(), id.to_owned()))
        .ok_or_else(invalid)?;
    if !party_field.is_empty() && text_field(record, party_field) != party {
        return Err(invalid());
    }
    Ok(())
}

fn validate_finance_config(finance: &LegacyFinanceConfig) -> LegacyMigrationResult<()> {
    if finance.currency.is_empty()
        || finance.currency.len() > 16
        || !finance
            .currency
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        || finance.scale > 18
    {
        return Err(LegacyMigrationError::Invalid(
            "legacy finance currency/scale configuration is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn seal_legacy_finance_record(
    record_family: &str,
    legacy_key: &str,
    document: Map<String, Value>,
    finance: &LegacyFinanceConfig,
    migration_time_micros: i64,
) -> LegacyMigrationResult<CapsuleEnvelope> {
    let identity = format!("{record_family}\0{legacy_key}");
    let entry_count = i64::try_from(document.len()).map_err(|_| {
        LegacyMigrationError::Invalid(format!("legacy finance {identity} is too large"))
    })?;
    let document = legacy_json_to_capsule_value(&identity, &Value::Object(document))?;
    let content_digest = legacy_document_digest(&document)?;
    seal_legacy_capsule(
        LegacyCapsuleSpec {
            family: "FinanceLegacyRecord",
            kind: LEGACY_FINANCE_KIND,
            storage_class: StorageClass::Finance,
            owner_scope: OwnerScope::Global,
            migration_time_micros,
        },
        &identity,
        Vec::new(),
        BTreeMap::from([
            (
                "record_family".to_owned(),
                CapsuleValue::Text(record_family.to_owned()),
            ),
            (
                "legacy_key".to_owned(),
                CapsuleValue::Text(legacy_key.to_owned()),
            ),
            ("document".to_owned(), document),
            ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
            (
                "content_digest".to_owned(),
                CapsuleValue::Bytes(content_digest),
            ),
            (
                "currency".to_owned(),
                CapsuleValue::Text(finance.currency.clone()),
            ),
            (
                "scale".to_owned(),
                CapsuleValue::Integer(i64::from(finance.scale)),
            ),
        ]),
    )
}
