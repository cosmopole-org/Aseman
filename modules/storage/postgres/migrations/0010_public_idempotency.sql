-- Public action idempotency (A701, P7-06): one durable claim per (subject, key).
-- The digest is what the first request signed; a retry under the same key with the
-- same digest replays the recorded response, a different digest is a mismatch, and an
-- unfinished claim is in progress. Claims live in the node's core schema because the
-- composed public action service is node state.
CREATE TABLE IF NOT EXISTS aseman_core.public_idempotency (
    subject text NOT NULL,
    key text NOT NULL,
    digest bytea NOT NULL CHECK (octet_length(digest) = 32),
    claimed_at_millis bigint NOT NULL,
    response bytea,
    completed boolean NOT NULL DEFAULT false,
    PRIMARY KEY (subject, key)
);
