-- A401 (P4-01) identity security state. Every identity key is a `core.identity_key`
-- capsule (created by 0001; 0000 retires the old `node_keys`). Idempotent.

-- Used (key ID, nonce) pairs, remembered until their retention ends (A401 "Replay").
-- Security state, not capsules: nothing else reads it and it is never migrated.
CREATE TABLE IF NOT EXISTS aseman_core."replay_nonces" (
  "key_id" TEXT NOT NULL,
  "nonce" BYTEA NOT NULL,
  "retain_until_millis" BIGINT NOT NULL,
  PRIMARY KEY ("key_id", "nonce")
);
CREATE INDEX IF NOT EXISTS ix_replay_nonces_retain_until
  ON aseman_core."replay_nonces" ("retain_until_millis");

-- One-time server challenges (A401 "Replay"): consumed by deleting the row.
CREATE TABLE IF NOT EXISTS aseman_core."identity_challenges" (
  "nonce" BYTEA PRIMARY KEY,
  "subject" TEXT NOT NULL,
  "audience" TEXT NOT NULL,
  "expires_at_millis" BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_identity_challenges_expires
  ON aseman_core."identity_challenges" ("expires_at_millis");
