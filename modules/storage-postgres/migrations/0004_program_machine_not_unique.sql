-- A machine may own several programs, as in legacy (`machinePrograms::{machine}::*`).
-- The first core schema declared `programs.machine_id` unique, which would reject the
-- migration of any machine with more than one program. Idempotent.
DROP INDEX IF EXISTS aseman_core.uq_programs_machine_id;
CREATE INDEX IF NOT EXISTS ix_programs_machine_id ON aseman_core."programs" ("machine_id") WHERE NOT tombstone;
