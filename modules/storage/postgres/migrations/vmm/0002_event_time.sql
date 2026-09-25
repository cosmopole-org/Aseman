-- Event retention is by age (the service's clock): record each event's time next to
-- its sequence. Events written before this column existed are dated 0 and are the
-- first to go.
ALTER TABLE aseman_vmm.event ADD COLUMN IF NOT EXISTS at_millis bigint NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS event_time ON aseman_vmm.event (at_millis);
