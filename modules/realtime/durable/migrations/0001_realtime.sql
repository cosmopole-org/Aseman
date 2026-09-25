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
CREATE INDEX IF NOT EXISTS realtime_outbox_pending
    ON aseman_core.realtime_outbox (event_id)
    WHERE NOT published;
