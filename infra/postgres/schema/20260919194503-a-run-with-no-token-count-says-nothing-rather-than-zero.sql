-- 20260919194503-a-run-with-no-token-count-says-nothing-rather-than-zero.sql
-- — `total_tokens` is NULL when nobody measured it, never 0.
--
-- Origin: backlog 65c9c05a, found by the builder of 8f1de7bf and
-- verified independently against the live table on 2026-09-19: 13 of
-- 42 rows carried `total_tokens = 0`, meaning "the harness printed no
-- usage line", distinguishable from a genuine zero only by a
-- companion `detail.tokens_reported: false`. `boss dispatch --report`
-- without `--tokens` wrote that zero, because the API refuses a run
-- with neither a total nor a split and zero was the only number that
-- got past the refusal. So the column answered confidently where it
-- had nothing: an average over `total_tokens` silently included 13
-- zeros, and no query that did not already know to join on a JSONB
-- flag could tell.
--
-- THE PRECEDENT IS IN THIS TABLE. `usd_micros` has drawn exactly this
-- distinction since 20260910030644 — NULL means no rate-card row
-- covered the model: unpriced, not free, and a roll-up over a set
-- holding one reports no total at all rather than an understated one.
-- Followed here rather than a third convention invented beside it: a
-- fact that has a spelling in the table gets that spelling.
--
-- WHAT 20260910151729 SAID, AND WHY IT IS NOT BEING CONTRADICTED.
-- That file made `total_tokens` NOT NULL on the reasoning that it is
-- "the one column every run can answer". That was true of the two
-- shapes a REPORTER could be in, and it stays true: a split derives a
-- total, a total-only run measures one. The case it did not have was
-- a reporter with nothing to report, which is not a third way of
-- counting tokens — it is the absence of a count, the same absence
-- `input_tokens` has held as NULL since that same file. The two
-- CHECKs that made the split and the total agree are untouched, and
-- one is TIGHTENED below.
--
-- THE HOLE NULLABILITY OPENS, CLOSED IN THE SAME BREATH. The existing
-- `CHECK (input_tokens IS NULL OR total_tokens = input_tokens +
-- output_tokens)` was total while the column was NOT NULL. Once it
-- can be NULL, a row with a split and a NULL total evaluates the
-- comparison to NULL — and Postgres treats a NULL CHECK as SATISFIED,
-- so a measured split with no total would have been accepted
-- silently. Replaced below with one that states the presence
-- explicitly. A constraint that stops being total when a neighbouring
-- column changes is the shape to look for in this kind of migration.
--
-- HISTORY IS NOT REWRITTEN. `agent_runs` is insert-once (ON CONFLICT
-- (run_id) DO NOTHING) and a projection of `agents.run.recorded`; the
-- 13 rows are the pre-instrumentation era and backlog fd5ce137
-- recommends excluding rather than backfilling them. No UPDATE here:
-- turning a recorded zero into a NULL would be the projection
-- asserting something no event says, and a rebuild from the log would
-- put the zeros straight back. New rows are honest; old rows stay
-- exactly as honest as they were, with `detail.tokens_reported`
-- telling a reader which is which for that window.

ALTER TABLE agent_runs ALTER COLUMN total_tokens DROP NOT NULL;

-- The pin for the one fact that lives in two columns, restated so it
-- is still total now that the total can be absent: a measured split
-- has a total, and that total equals it.
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_total_matches_split_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_total_matches_split_check
    CHECK (
        input_tokens IS NULL
        OR (total_tokens IS NOT NULL AND total_tokens = input_tokens + output_tokens)
    );

COMMENT ON COLUMN agent_runs.total_tokens IS
  'Tokens the run spent. DERIVED from input_tokens + output_tokens '
  'when the split was measured (and constrained to equal it); the '
  'measurement itself when the reporter had only a total. NULL means '
  'the reporter had NO count to give — unknown, not zero, the same '
  'distinction usd_micros draws for an unpriced run (backlog '
  '65c9c05a). A roll-up over a set containing one of these reports no '
  'token total at all rather than an understated one, and counts them '
  'out loud as unreported_runs.';
