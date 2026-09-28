-- ADR 0035: consensus logs (one ordered byte key/value space per log) on PostgreSQL.
CREATE SCHEMA IF NOT EXISTS aseman_consensus;
REVOKE ALL ON SCHEMA aseman_consensus FROM PUBLIC;
CREATE TABLE IF NOT EXISTS aseman_consensus.log_entries (
  log TEXT NOT NULL,
  key BYTEA NOT NULL,
  value BYTEA NOT NULL,
  PRIMARY KEY (log, key)
);
