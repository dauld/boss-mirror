-- A step remembers when it became an obligation.
-- (Queue-age lens, packet 2a0b034e; measurement 2a77e5fc rec 1.)
--
-- `updated_at` is only an honest LOWER BOUND on how long a ready step
-- has waited: any later write bumps it — annotating a packet is
-- enough. This column is the dedicated stamp. The projection sets it
-- ONCE, at the write that first lands the step in `ready` (the INSERT
-- for steps born ready at materialization, the UPDATE for a
-- pending → ready promotion), and no later write moves it — which is
-- exactly the property `updated_at` cannot have.
--
-- Nullable, and deliberately not backfilled by migration: rows that
-- predate the column keep NULL, and the queue-age lens
-- (`GET /api/jobs/queue-age`) falls back to `updated_at` for them,
-- labelled `exact: false`. A full projection rebuild DOES recover the
-- historical stamps — `jobs.step.updated` events have carried every
-- transition with a real timestamp all along; the projection just
-- dropped it (2a77e5fc).

ALTER TABLE steps ADD COLUMN IF NOT EXISTS became_ready_at TIMESTAMPTZ;
