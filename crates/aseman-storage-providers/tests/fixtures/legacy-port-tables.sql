-- modules/realtime/durable/migrations/0001_realtime.sql
-- Durable realtime (A707, ADR 0014): the authoritative event log, consumer
-- checkpoints, and the transactional outbox.
--
-- The log is authoritative, not a cache of a broker. An event is inserted in the same
-- transaction as the application change that caused it, so the two cannot disagree.
CREATE SCHEMA IF NOT EXISTS aseman_core;

CREATE TABLE IF NOT EXISTS aseman_core.realtime_event (
    id uuid PRIMARY KEY,
    stream text NOT NULL,
    -- Sequences are dense and monotonic within a stream: a gap would make a consumer
    -- wait for an event that never comes, and a repeat would make replay ambiguous.
    sequence bigint NOT NULL CHECK (sequence >= 1),
    creature_id uuid NOT NULL,
    kind text NOT NULL,
    producer text NOT NULL,
    at_millis bigint NOT NULL,
    payload_digest text NOT NULL,
    retention text NOT NULL,
    version text NOT NULL,
    idempotency_key text,
    payload bytea NOT NULL,
    UNIQUE (stream, sequence)
);
CREATE INDEX IF NOT EXISTS realtime_event_stream
    ON aseman_core.realtime_event (stream, sequence);
CREATE INDEX IF NOT EXISTS realtime_event_retention
    ON aseman_core.realtime_event (at_millis);

CREATE TABLE IF NOT EXISTS aseman_core.realtime_checkpoint (
    consumer text NOT NULL,
    stream text NOT NULL,
    sequence bigint NOT NULL CHECK (sequence >= 0),
    at_millis bigint NOT NULL,
    PRIMARY KEY (consumer, stream)
);

-- One row per event awaiting onward publication. A claim is a worker's name and a
-- deadline, so a worker that dies releases its work by expiry rather than by cleanup.
CREATE TABLE IF NOT EXISTS aseman_core.realtime_outbox (
    event_id uuid PRIMARY KEY REFERENCES aseman_core.realtime_event (id) ON DELETE CASCADE,
    claimed_by text,
    claimed_until_millis bigint,
    attempts integer NOT NULL DEFAULT 0,
    published boolean NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS realtime_outbox_claimable
    ON aseman_core.realtime_outbox (attempts, claimed_until_millis, event_id)
    WHERE NOT published;

-- modules/finance/ledger/migrations/0001_finance.sql
-- Metering and the ledger.
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

-- modules/federation/http/migrations/0001_federation.sql
-- Federation directory and envelope guard (A704, A705).
--
-- The home node is authoritative for its own descriptors; these tables are this node's
-- own record plus its cache of others. Sequences and revisions only move forward, so a
-- replayed older descriptor cannot un-rotate a key or un-revoke an epoch.
CREATE SCHEMA IF NOT EXISTS aseman_core;

CREATE TABLE IF NOT EXISTS aseman_core.federation_node (
    node_id uuid PRIMARY KEY,
    sequence bigint NOT NULL CHECK (sequence >= 1),
    expires_at_millis bigint NOT NULL,
    descriptor text NOT NULL
);

CREATE TABLE IF NOT EXISTS aseman_core.federation_workload (
    workload_id uuid PRIMARY KEY,
    revision bigint NOT NULL CHECK (revision >= 1),
    expires_at_millis bigint NOT NULL,
    descriptor text NOT NULL
);

-- Seen nonces. A repeated one is a replay, and is refused. Rows are dropped once the
-- envelope that carried them could no longer be accepted anyway.
CREATE TABLE IF NOT EXISTS aseman_core.federation_nonce (
    source_node uuid NOT NULL,
    nonce text NOT NULL,
    expires_at_millis bigint NOT NULL,
    PRIMARY KEY (source_node, nonce)
);
CREATE INDEX IF NOT EXISTS federation_nonce_expiry
    ON aseman_core.federation_nonce (expires_at_millis);

-- Answered requests. A retry carries the same request id and is answered from here
-- rather than executed a second time. This is a different thing from a nonce: one says
-- "already seen", the other says "already answered, here is the answer".
CREATE TABLE IF NOT EXISTS aseman_core.federation_answer (
    request_id uuid PRIMARY KEY,
    answer text NOT NULL,
    expires_at_millis bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS federation_answer_expiry
    ON aseman_core.federation_answer (expires_at_millis);

-- modules/storage/postgres/migrations/0006_coordination.sql
-- Fenced singleton coordination (A607, ADR 0013).
--
-- One row per named singleton job. The row is locked for update inside the
-- acquisition transaction, expiry is decided by database time, and `token` is
-- allocated on every acquisition and never reused: it is what a destination-side
-- guard fences a paused former holder out with. An advisory lock alone would not
-- give us the token, and so would not give us the guarantee.
-- Self-contained: the VMM service and the node both use coordination and start in
-- either order, so this migration never assumes the other one has run.
CREATE SCHEMA IF NOT EXISTS aseman_core;

CREATE TABLE IF NOT EXISTS aseman_core.coordination_lease (
    name text PRIMARY KEY,
    instance text NOT NULL,
    token bigint NOT NULL CHECK (token >= 1),
    acquired_at_millis bigint NOT NULL,
    -- A released lease has expires = acquired: a zero-length lease, already over.
    -- That is how a resignation frees the name at once while the row, and with it
    -- the token counter, stays.
    expires_at_millis bigint NOT NULL CHECK (expires_at_millis >= acquired_at_millis)
);

-- The highest fencing token each destination has accepted for a name. An effect
-- that cannot be committed in the same transaction as the lease check passes
-- through here instead, and a lower token is refused.
CREATE TABLE IF NOT EXISTS aseman_core.coordination_fence (
    name text PRIMARY KEY,
    token bigint NOT NULL CHECK (token >= 1)
);

-- modules/storage/postgres/migrations/vmm/0001_vmm.sql
-- The VMM service's own state (A501, A503): workloads with observed state,
-- operations, idempotency keys, and the event log. It is separate from the node's
-- core schema: the VMM owns observed state, the node owns desired state.
CREATE SCHEMA IF NOT EXISTS aseman_vmm;

CREATE TABLE IF NOT EXISTS aseman_vmm.workload (
    id uuid PRIMARY KEY,
    owner text NOT NULL,
    creature_id uuid NOT NULL,
    observed_state text,
    resource_version bigint NOT NULL CHECK (resource_version >= 1),
    record jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS workload_owner ON aseman_vmm.workload (owner, id);

CREATE TABLE IF NOT EXISTS aseman_vmm.operation (
    id uuid PRIMARY KEY,
    owner text NOT NULL,
    workload_id uuid,
    state text NOT NULL,
    created_at_millis bigint NOT NULL,
    record jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS operation_owner
    ON aseman_vmm.operation (owner, created_at_millis DESC, id DESC);
CREATE INDEX IF NOT EXISTS operation_unfinished
    ON aseman_vmm.operation (created_at_millis, id)
    WHERE state IN ('pending', 'running');

CREATE TABLE IF NOT EXISTS aseman_vmm.idempotency (
    owner text NOT NULL,
    key text NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest) = 32),
    claimed_at_millis bigint NOT NULL,
    response_status integer,
    response_body bytea,
    response_content_type text,
    response_location text,
    PRIMARY KEY (owner, key)
);

CREATE TABLE IF NOT EXISTS aseman_vmm.event (
    sequence bigint PRIMARY KEY,
    owner text NOT NULL,
    workload_id uuid NOT NULL,
    record jsonb NOT NULL
);
CREATE INDEX IF NOT EXISTS event_owner ON aseman_vmm.event (owner, sequence);

-- One row. Appends take it FOR UPDATE, so sequences commit in order and a reader
-- never skips an event that commits later with a lower sequence.
CREATE TABLE IF NOT EXISTS aseman_vmm.event_log (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    last_sequence bigint NOT NULL,
    truncated_through bigint NOT NULL
);
INSERT INTO aseman_vmm.event_log (singleton, last_sequence, truncated_through)
VALUES (true, 0, 0)
ON CONFLICT (singleton) DO NOTHING;

-- modules/storage/postgres/migrations/vmm/0002_event_time.sql
-- Event retention is by age (the service's clock): record each event's time next to
-- its sequence. Events written before this column existed are dated 0 and are the
-- first to go.
ALTER TABLE aseman_vmm.event ADD COLUMN IF NOT EXISTS at_millis bigint NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS event_time ON aseman_vmm.event (at_millis);

