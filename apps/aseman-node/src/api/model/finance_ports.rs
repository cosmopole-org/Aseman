//! The finance ledger of one state action (ADR 0036): the finance models through the
//! typed ledger over the action's transaction.

/// The finance ledger over an action's transaction (`FinanceLedgerPorts { trx }`).
pub(crate) type FinanceLedgerPorts<'a> = aseman_capsule::finance::StorageFinanceLedger<'a>;
