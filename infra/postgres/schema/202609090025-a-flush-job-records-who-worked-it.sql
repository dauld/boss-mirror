-- 202609090025-a-flush-job-records-who-worked-it.sql — the flush job
-- keeps who MOVED it, not only who asked for it.
--
-- Origin: backlog c3cd3301. `design_flush_jobs.requested_by` records
-- the actor who queued the flush. Nothing recorded the actor who ran
-- it — `boss docs flush-pending` claimed a job (running), rewrote a
-- markdown file, committed it, and reported succeeded, and every one
-- of those transitions landed against a row that named nobody. The
-- commit has a git author; the job record had no answer at all.
--
-- The companion CLI change makes every `boss docs` call carry its
-- caller in `x-boss-user` (the one definition, boss-cli/src/identity.rs)
-- and refuses a write nobody named. A header the receiving end drops
-- changes nothing, so this is the column that receives it.
--
-- NULL is the honest value for a job that has not been worked yet,
-- and the status PUT clears it back to NULL on a requeue — a job
-- waiting to run has no worker, and carrying the previous one would
-- describe the past as the present.

ALTER TABLE design_flush_jobs
    ADD COLUMN IF NOT EXISTS worked_by TEXT;

COMMENT ON COLUMN design_flush_jobs.worked_by IS
  'The actor that last moved this job through its lifecycle, from the '
  'x-boss-user header on the status PUT. NULL while queued: a job '
  'waiting to run has no worker. Distinct from requested_by, which is '
  'who asked for the flush.';
