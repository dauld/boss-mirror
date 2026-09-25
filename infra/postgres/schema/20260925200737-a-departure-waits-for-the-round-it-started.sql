-- 20260925200737-a-departure-waits-for-the-round-it-started.sql — the
-- cadence registry learns the dock's two new facts: a `refresh` verb,
-- and a bound on how long a departure waits for re-gates.
--
-- Origin: backlog 4890165b, design 42279fb2 ("The dock refreshes between
-- departures, and a departure waits one gate for the round it started",
-- approved 2026-09-25). Measured on the item's triage over the ten trains
-- to 19:26Z that day: 22 cars boarded, 58 left-behind rows, 42 of them
-- "waiting for a gate slot to re-gate on current main". A parked car whose
-- files main moved into is replayed onto main and re-gated before it
-- boards (backlog 969a1092) — but the dock launched those re-gates only
-- inside `board`, once per departure window, and against the main the
-- train departing in the same pass was about to replace.
--
-- TWO SCHEMA FACTS, NO ROWS. The rules themselves are declared in the
-- platform bundle (`infra/platform/cadence/`), because migrations newer
-- than the cutover declare schema only
-- (infra/lint/migrations-declare-schema-only.sh):
--
--   1. `refresh` is a conductor verb (D1). `train-dock-refresh` fires it
--      every two minutes: the dock's per-car judgement, and the re-gates
--      it owes, while the track is clear — nothing assembled, nothing
--      departed. The verb list is restated whole, the way 202608282135
--      restated it, because Postgres cannot extend a CHECK.
--
--   2. `regate_hold_minutes` bounds D2: a departure holds while the dock
--      has re-gates in flight on the CURRENT main, until the oldest of
--      them is this many minutes old. NULL (every rule but the boarding
--      one) means no hold. Only a verb that departs a train may carry it
--      — the same two verbs `boss_jobs::cadence::departs_a_train` names —
--      because a hold on a reconcile or a packet-open would be a number
--      nothing reads.

ALTER TABLE cadence_rules
    ADD COLUMN IF NOT EXISTS regate_hold_minutes INT;  -- board/run: minutes a departure waits for the dock's re-gate round

ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_verb_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_verb_check CHECK (
    verb IN ('preflight', 'reconcile', 'board', 'run', 'refresh')
    OR verb ~ '^open:[a-z0-9-]+$'
);

ALTER TABLE cadence_rules DROP CONSTRAINT IF EXISTS cadence_rules_regate_hold_check;
ALTER TABLE cadence_rules ADD CONSTRAINT cadence_rules_regate_hold_check CHECK (
    regate_hold_minutes IS NULL
    OR (regate_hold_minutes >= 0 AND verb IN ('board', 'run'))
);
