-- A packet is stamped with the instant it was admitted (backlog
-- 6c2eba00, design f2cdff23).
--
-- `jobs.opened_on` is a DATE, so the finest honest answer it supports
-- is a whole day. Every surface that asks "how long has this been
-- waiting" needs the instant: the ops-runner's `oldest_wait_s` gauge,
-- dock wait and gate duration on the region map, the overdue alarms,
-- the silence sweep that decides an agent run has died. Until now that
-- instant was `metadata.opened_at`, written by whoever filed the
-- packet — the dispatcher's rules write it, `boss job file` writes it,
-- a hand-filed packet or a caller that does not know the convention
-- does not. The failure is the quiet kind: `date -u -d ''` answers
-- midnight rather than erroring, so a missing stamp reads as decades
-- old rather than as unknown.
--
-- Server-owned, like `steps.completed_at`
-- (202609081700-a-completion-names-its-actor.sql): written once at
-- admission and kept out of every UPDATE afterwards, the same way
-- `partition` is. A body that supplies its own value does not move it.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS opened_at TIMESTAMPTZ;

-- The back-fill is a PROJECTION OF THE LOG, not an invention (design
-- f2cdff23, question `backfill`): every packet's own
-- `jobs.job.created` event, which carries both the instant the log
-- recorded it and — for packets admitted after this column exists —
-- the stamp itself. Same derivation the rebuilder applies
-- (boss-jobs/src/rebuild.rs), so a full replay reproduces exactly
-- these values rather than drifting from them.
--
-- A packet whose create event is not in the log (an audit slice
-- trimmed by an epoch restart) keeps NULL. An absent stamp is the
-- honest answer there; an instant nobody observed would be worse than
-- no instant at all.
UPDATE jobs j
   SET opened_at = c.at
  FROM (
    SELECT DISTINCT ON (payload->>'id')
           payload->>'id' AS job_id,
           COALESCE(NULLIF(payload->>'opened_at', '')::timestamptz, timestamp) AS at
      FROM audit_log
     WHERE kind = 'jobs.job.created'
     ORDER BY payload->>'id', id
  ) c
 WHERE j.opened_at IS NULL
   AND j.id::text = c.job_id;
