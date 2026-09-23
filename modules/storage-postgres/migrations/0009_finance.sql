-- Metering and the ledger (Phase 8).
--
-- The settlement identity is (workload, interval start, provider sample). It is a
-- primary key here, which is what makes a duplicate collection or a duplicated message
-- unable to charge twice: the second attempt is the same row.
CREATE SCHEMA IF NOT EXISTS aseman_core;

CREATE TABLE IF NOT EXISTS aseman_core.usage_sample (
    workload_id uuid NOT NULL,
    provider_sample_id text NOT NULL,
    provider text NOT NULL,
    collected_at_millis bigint NOT NULL,
    sample text NOT NULL,
    PRIMARY KEY (workload_id, provider_sample_id)
);
CREATE INDEX IF NOT EXISTS usage_sample_time
    ON aseman_core.usage_sample (workload_id, collected_at_millis DESC);

CREATE TABLE IF NOT EXISTS aseman_core.usage_interval (
    settlement_key text PRIMARY KEY,
    workload_id uuid NOT NULL,
    interval_start_millis bigint NOT NULL,
    interval_end_millis bigint NOT NULL,
    interval text NOT NULL
);
CREATE INDEX IF NOT EXISTS usage_interval_order
    ON aseman_core.usage_interval (interval_start_millis, settlement_key);

-- Append-only. A record is its idempotency key: committing the same key twice is
-- success and changes nothing.
CREATE TABLE IF NOT EXISTS aseman_core.journal_record (
    idempotency_key text PRIMARY KEY,
    at_millis bigint NOT NULL,
    price_version text,
    record text NOT NULL
);

-- One row per entry, so a balance is a sum rather than a parsed document.
CREATE TABLE IF NOT EXISTS aseman_core.journal_entry (
    idempotency_key text NOT NULL
        REFERENCES aseman_core.journal_record (idempotency_key) ON DELETE CASCADE,
    ordinal integer NOT NULL,
    account text NOT NULL,
    amount bigint NOT NULL,
    PRIMARY KEY (idempotency_key, ordinal)
);
CREATE INDEX IF NOT EXISTS journal_entry_account
    ON aseman_core.journal_entry (account);

CREATE TABLE IF NOT EXISTS aseman_core.price_list (
    version text PRIMARY KEY,
    effective_from_millis bigint NOT NULL,
    list text NOT NULL
);
