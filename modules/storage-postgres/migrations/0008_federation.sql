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
