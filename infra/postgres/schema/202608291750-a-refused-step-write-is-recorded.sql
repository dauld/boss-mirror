-- =========================================================================
-- 202608291750-a-refused-step-write-is-recorded.sql
--
-- The denominator the reliability metric was missing.
--
-- Step conformance measures 100% across 3,517 completed steps, and it
-- cannot measure anything else: required-at-done validators run on the
-- write path, so a step physically cannot reach `completed` without its
-- required fields. The number certifies the validator, not the work.
-- Every real failure lives in what the record never held — attempts
-- that did not become completions.
--
-- A refused write is exactly that evidence, and today it leaves no
-- trace at all: boss-jobs/src/http/steps.rs returns its 4xx from an
-- early `return`, above the OUTBOX block that emits the step events,
-- so nothing is written and nothing is emitted. `policy_rule_audit`
-- does not cover it either — that table records edits to the RULEBOOK
-- (rule.upsert, override.deactivate), not decisions made against it.
--
-- WHY A SIBLING TABLE AND NOT audit_log. The event log records state
-- changes — facts about what happened — and its meaning is load-bearing
-- across the five-property correctness protocol. A refusal changes
-- nothing, so widening the log to hold attempts would buy one metric at
-- the cost of what "the log is the system of record" means. David chose
-- the sibling table on 2026-08-29 when the fork was put to him.
--
-- WHAT THIS IS NOT FOR. The count is not the metric, and driving it to
-- zero is not the goal — the cheapest way to do that is to loosen
-- validation, which would destroy the one mechanism keeping conformance
-- at 100%. A refusal is usually the protocol working. The two derived
-- readings are what matter, and the indexes below serve them:
--   1. UNRECOVERED refusals — a refusal never followed by a success on
--      that step. That is an obligation that went undischarged.
--   2. DISTINCT ACTORS per (step, error_class) — several independent
--      actors hitting the same refusal means the PROTOCOL is hard to
--      comply with. One actor hitting many different refusals is the
--      actor. This is what "regardless of which actor attempted the
--      step" means operationally.
-- =========================================================================

CREATE TABLE IF NOT EXISTS step_write_refusals (
    id            BIGSERIAL PRIMARY KEY,
    refused_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Both nullable on purpose: a refusal can happen BECAUSE the id did
    -- not parse ("invalid job id"), and that refusal is still evidence.
    -- No FK for the same reason, and because a refusal must outlive the
    -- packet it was about — retention trims jobs, and deleting the
    -- history of what actors got wrong would defeat the point.
    job_id        UUID,
    step_id       UUID,
    actor_id      TEXT NOT NULL,
    method        TEXT NOT NULL,
    path          TEXT NOT NULL,
    status_code   INT  NOT NULL,
    -- Coarse, stable, and derived from the response — never free text.
    -- See boss-jobs/src/refusals.rs::classify, which is a pure function
    -- so the vocabulary is unit-tested rather than asserted here.
    error_class   TEXT NOT NULL CHECK (error_class IN (
        'validation', 'policy', 'state', 'shape', 'missing', 'other'
    )),
    detail        TEXT NOT NULL
);

-- Reading 2: distinct actors per (step, error_class).
CREATE INDEX IF NOT EXISTS step_write_refusals_class
    ON step_write_refusals(error_class, step_id);

-- Reading 1: what was refused on this step, in order, so a later
-- success on the same step can be paired against it.
CREATE INDEX IF NOT EXISTS step_write_refusals_step
    ON step_write_refusals(step_id, refused_at DESC) WHERE step_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS step_write_refusals_recent
    ON step_write_refusals(refused_at DESC);
