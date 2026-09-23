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
