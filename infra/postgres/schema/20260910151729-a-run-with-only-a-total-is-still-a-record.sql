-- 20260910151729-a-run-with-only-a-total-is-still-a-record.sql — let an
-- agent run be recorded truthfully when only a TOTAL token count is
-- known.
--
-- WHAT THE FIRST REAL CALLER FOUND. 20260910030644 created agent_runs
-- with `input_tokens BIGINT NOT NULL` and `output_tokens BIGINT NOT
-- NULL`. The first honest attempt to file a row — tonight's fifteen
-- coding-agent runs, the ones that built the cars that landed today,
-- including the car that built this recorder — could not satisfy it.
-- Those agents report `subagent_tokens`: ONE number, beside a tool-call
-- count and a duration. No input/output split exists anywhere for them.
-- So the only two ways forward were to invent a split or to record
-- nothing, and both are worse than the schema being wrong.
--
-- WHY THIS IS NOT MERELY A NULLABILITY TWEAK. agent_rate_card prices
-- input and output at DIFFERENT rates ($5.00 vs $25.00 per MTok on
-- opus-5), so a total genuinely cannot be priced. There is no blend
-- that is a measurement: assuming a 9:1 ratio would produce a figure
-- with a measurement's authority and a guess's accuracy, and a reader
-- six months from now could not tell which it was. The design already
-- has the right answer for this case, written into this table's own
-- comment: unpriced is NULL and never 0, because zero reads as free,
-- and a roll-up over a set containing an unpriced run reports no total
-- at all plus a count of the runs it could not price. A total-only run
-- is exactly that case, and `boss_jobs::agent_runs` now counts it
-- twice over — `unpriced_runs` says a price was not computed,
-- `total_only_runs` says why.
--
-- THE SHAPE, AND WHY total_tokens IS NOT NULL. Three columns could
-- hold the same fact twice (CLAUDE.md §9a), so exactly one of them is
-- the definition in each of the two legal shapes:
--
--   * split measured  → input_tokens + output_tokens are the fact, and
--                       total_tokens is DERIVED from them by the writer
--                       (`TokenUsage::total`). The CHECK below refuses
--                       any pair that disagrees, so a hand-written
--                       INSERT cannot create the drift either.
--   * total only      → total_tokens is the fact and the split is NULL.
--
-- Which makes total_tokens the one column every run can answer, which
-- is why it is NOT NULL rather than nullable-with-an-OR-check: "at
-- least one of (total) or (input AND output)" is enforced as "the total
-- is always present, and must equal the split whenever the split is".
-- A row with neither shape is still refused — by NOT NULL here, and by
-- `TokenUsage::from_parts` before a write is attempted, which names the
-- two ways to report tokens rather than leaving the caller to guess.
--
-- HALF A SPLIT IS NOT A SHAPE. input_tokens present with output_tokens
-- absent is refused rather than completed by subtracting from the
-- total: deriving the missing half would invent a measurement nobody
-- took, which is the same defect as the blended rate one constraint up.

ALTER TABLE agent_runs ALTER COLUMN input_tokens DROP NOT NULL;
ALTER TABLE agent_runs ALTER COLUMN output_tokens DROP NOT NULL;

-- Added nullable, backfilled from the split (the only shape that can
-- exist before this migration), then tightened. The UPDATE is a no-op
-- on every database where this lands first — agent_runs holds zero rows
-- on 2026-09-10 — and correct on any that already has split rows.
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS total_tokens BIGINT;
UPDATE agent_runs
   SET total_tokens = input_tokens + output_tokens
 WHERE total_tokens IS NULL;
ALTER TABLE agent_runs ALTER COLUMN total_tokens SET NOT NULL;

ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_total_tokens_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_total_tokens_check
    CHECK (total_tokens >= 0);

-- One half of a split is not a report. Either both halves were
-- measured or neither was.
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_split_is_whole_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_split_is_whole_check
    CHECK ((input_tokens IS NULL) = (output_tokens IS NULL));

-- The pin for the one fact that lives in two columns. Postgres cannot
-- generate total_tokens from the split (a generated column could not
-- then be supplied for a total-only run), so the derivation lives in
-- the writer and this constraint makes a disagreement unrepresentable.
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_total_matches_split_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_total_matches_split_check
    CHECK (input_tokens IS NULL OR total_tokens = input_tokens + output_tokens);

COMMENT ON COLUMN agent_runs.total_tokens IS
  'Tokens the run spent, the one figure every reporter can give. '
  'DERIVED from input_tokens + output_tokens when the split was '
  'measured (and constrained to equal it); the measurement itself when '
  'the reporter had only a total.';

COMMENT ON COLUMN agent_runs.input_tokens IS
  'NULL when the reporter had no split — not zero. A total-only run is '
  'recorded in full and priced by nothing: the rate card charges input '
  'and output at different rates, so no arithmetic turns one number '
  'into a cost, and usd_micros stays NULL with the roll-up counting it '
  'as unpriced rather than reporting a blended guess.';
