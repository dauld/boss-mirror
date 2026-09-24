-- 20260924123307-a-job-status-is-one-of-four-words.sql — `blocked`
-- and `pending-sign-off` are retired from `jobs.status` (backlog
-- 3c3dc8f3), with the two `JobStatus` variants that spelled them.
--
-- Nothing ever set either: the v2 engine has no Blocked state (a step
-- waiting on a dependency is a `pending` step on an `open` Job) and
-- step completion refuses an unsigned step, so the PendingSignOff arm
-- of `compute_job_status` was unreachable. Measured live before this
-- car, 2026-09-24 12:2xZ: `GET /api/jobs` answered 0 for both status
-- filters against a total of 16,963 (335 open + 16,628 closed), and
-- a scan of every `jobs.job.*` audit event — 97,909 rows, the whole
-- log from its oldest row on 2026-09-16 — found neither word at any
-- key. What the two words DID do was render as two /ux/jobs status
-- filters that could only answer "No jobs match."
--
-- Idempotent: drop-if-exists then add, the constraint idiom of
-- 20260917195929. The column's CHECK is the auto-named one from
-- 03-jobs.sql. ADD CONSTRAINT validates every existing row, so a row
-- holding a retired word stops this migration loudly rather than
-- being rewritten into some other status.

ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_status_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_status_check
    CHECK (status IN ('draft', 'open', 'closed', 'cancelled'));
