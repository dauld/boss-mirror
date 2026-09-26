-- 20260926145156-a-dead-letter-that-names-no-packet-is-recorded.sql —
-- the dispatcher's own record holds what it did NOT deliver, too.
--
-- Origin: backlog 4b175523, found by the builder of 43c4451a. The rules
-- list at /it/registry/rules reads two records per rule: its newest row
-- here, and the `dead_letter` annotation a handler that failed past its
-- budget lands on the packet it owed (a9c498eb). A dead-letter on a
-- topic whose subject is NOT a packet — commerce.invoice.*, inventory.*,
-- ledger.*, jobs.estate.* — had nowhere to land: measured on origin/main
-- 564d044c, `annotation_target` answers None for every such topic, and
-- the runner kept only a log line and an in-process counter
-- (`dead_letters_unrecorded` on /api/dispatcher/readyz) that resets with
-- the pod. So `GET /api/jobs?metadata_has=dead_letter` answering total 0
-- on 2026-09-24 could not be told apart from loss.
--
-- WHY A COLUMN AND NOT A TABLE. A dead-letter IS a rule's outcome on an
-- event — the same rule, the same topic, the same instant, the same
-- evidence in `detail` — and this table already has the writer (the
-- dispatcher's own pool, best-effort after the settle), the thirty-day
-- prune and the per-rule recency index. A second table would copy all
-- three to hold one more word. The id keeps the two apart:
-- `dispatcher:<rule>:<event-id>` for a firing and
-- `dead-letter:<rule>:<event-id>` for a dead-letter, so a log-tail
-- restart that later DELIVERS an event it once dead-lettered records the
-- firing rather than having the primary key swallow it.
--
-- EVERY READER OF A FIRING NOW SAYS SO. The two reads that answer "when
-- did this rule last fire" (boss_jobs::dispatcher_firings, the borders
-- and the rules list) filter `outcome = 'fired'`; the dead-letter read
-- filters the other way. The default keeps every row written before
-- this file what it was: a firing.
--
-- AND THE SCHEDULE RUNNER WRITES HERE NOW (same car). The first
-- migration's note said its rows would carry `schedule:<cadence>` in
-- `fired_on`; they carry the topic the runner dispatches under instead —
-- `clock.day` for a day rule, keyed `dispatcher:<rule>:clock-day:<day>`
-- so a crash that re-fires a sim-day records it once, and `clock.tick`
-- for a sub-day rule, keyed by the tick's instant.

ALTER TABLE dispatcher_firings
    ADD COLUMN IF NOT EXISTS outcome TEXT NOT NULL DEFAULT 'fired'
        CHECK (outcome IN ('fired', 'dead-letter'));

COMMENT ON COLUMN dispatcher_firings.outcome IS
  '''fired'' when every handler of the rule succeeded on the event; '
  '''dead-letter'' when they failed past the redelivery budget (or '
  'permanently) on a topic that names no packet, so the failure has no '
  'packet to be annotated on (4b175523).';
