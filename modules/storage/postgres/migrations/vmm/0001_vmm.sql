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
