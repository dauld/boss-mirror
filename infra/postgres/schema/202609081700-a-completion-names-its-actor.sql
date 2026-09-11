-- A completion names its actor (backlog c17871fe).
--
-- On 2026-09-08 a human approved two design-review steps in the UI
-- and the completed rows carried `answer` and `assignee_id` and
-- nothing else — no who, no when. The operator session could not
-- tell a human acceptance from a rule copying `proposed` into
-- `answer` after the fact, and had to ask. The audit log held the
-- actor on the `step.done` event all along; the step did not.
--
-- Two server-owned stamps, written ONCE at the flip to `completed`
-- and frozen with the row afterwards (the same CASE freeze `status`
-- and `completed_on` already take in the UPDATE):
--
--   completed_by  the actor the API signed the completing write
--                 with, in its wire form — a bare employee id for a
--                 person, `automation:<slug>` for a process,
--                 `<mode>:<model>` for an agent session. A body that
--                 supplies its own value is overwritten.
--   completed_at  the instant, from the clock that dates
--                 `completed_on`.
--
-- Nullable and deliberately not backfilled: rows that predate the
-- stamp keep NULL, and a full projection rebuild recovers them from
-- the `jobs.step.updated` events, which carry the whole Step.
ALTER TABLE steps ADD COLUMN IF NOT EXISTS completed_by TEXT;
ALTER TABLE steps ADD COLUMN IF NOT EXISTS completed_at TIMESTAMPTZ;

-- The per-packet audit read (`GET /api/jobs/{id}/events`) asks the
-- log "everything about this job". Step events carry the job under
-- `job_id`; the job's own lifecycle events carry it as `id`. Two
-- expression indexes so the read is an index walk, not a scan of a
-- log that held 370k rows when this was written.
CREATE INDEX IF NOT EXISTS audit_log_payload_job_id
    ON audit_log ((payload->>'job_id'));
CREATE INDEX IF NOT EXISTS audit_log_payload_id
    ON audit_log ((payload->>'id'));
