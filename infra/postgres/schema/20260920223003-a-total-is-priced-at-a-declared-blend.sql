-- 20260920223003-a-total-is-priced-at-a-declared-blend.sql — let a
-- model DECLARE the input/output ratio its bare totals are priced at,
-- so a dispatched run stops being structurally unpriceable.
--
-- WHAT WAS MEASURED. Backlog 6681a803, 2026-09-20: 85 agent runs on the
-- record, 2 carrying any cost figure, 83 carrying none. Both budget
-- desks compute a PRICED floor (`agent_budget::spent_in` filters to the
-- runs it can price, deliberately — a missing number must not stop the
-- stack), so an hourly cap of $N was being enforced against roughly a
-- fiftieth of the spend. The cause is not laziness: `boss dispatch
-- --report --tokens` asks for a split, the harness that runs a
-- dispatched builder prints `<subagent_tokens>182068` — ONE number,
-- never a split — and 20260910151729 refused to price one number
-- against two rates, in as many words, because "assuming a 9:1 ratio
-- would produce a figure with a measurement's authority and a guess's
-- accuracy".
--
-- WHAT CHANGED, AND WHAT DID NOT. That objection is right and survives
-- intact; what was wrong was where the assumption had to live. An
-- assumption hardcoded in Rust is one nobody can read or change, and a
-- figure that cannot say it rests on one is the zero-means-unknown
-- defect in better clothes — the shape this system fixed three times on
-- this same day (`total_tokens`, `Cost.usd_micros`, and the roll-up's
-- unpriced-is-not-free). So design 91a9bfe7, decided by David
-- 2026-09-20, put the assumption in the registry beside the two rates
-- it weighs, and required the record to name the basis of every figure
-- (`boss_jobs::agent_runs::PricingBasis`, DERIVED from the run's own
-- token shape and price, carried into every roll-up, and a bucket
-- holding one blended run reports itself blended). A measured split
-- still wins: the blend is consulted only where no split exists.
--
-- WHY A RATIO AND NOT A BLENDED RATE. The two rates in this table are
-- the prices; a stored blend would be the same fact a third time and
-- would go stale the day a price moved (CLAUDE.md §9a). The ratio is
-- the only thing here that is not a price, so it is the only thing
-- stored, and `RateCardRow::blended_usd_micros_per_mtok` weighs the
-- two on every read.
--
-- WHY NULLABLE, AND WHY ONE ROW. NULL means this model declares no
-- ratio, and a total-only run on it stays unpriced — undeclared is
-- unpriced, never assumed, the same shape as a model no row names at
-- all. Exactly one row is given a ratio, because exactly one row has
-- evidence behind it: all 88 recorded runs ran on `opus-5[1m]`, and its
-- nine split-reporting runs (read from the live record 2026-09-20)
-- measured an input share with a median of 87.4% and a pooled
-- 87.7% excluding one 4.5M-token outlier:
--
--   0.819  0.864  0.870  0.872  0.874  0.892  0.922  0.930  0.996
--
-- 875,000 ppm (87.5%) sits between the median and the pooled figure and
-- makes the blend $7.50/MTok exactly — 0.875 × $5.00 + 0.125 × $25.00 —
-- so a reader can check the arithmetic by hand. Seeding a ratio for a
-- model with no measured runs behind it would be the guess this column
-- exists to avoid.
--
-- EXISTING ROWS ARE LEFT ALONE. No backfill, here or anywhere: pricing
-- happens once, at record time, and a rebuild replays the recorded
-- figure rather than re-pricing (see `agent_runs`' table comment). The
-- 83 runs already unpriced stay unpriced, because marking them blended
-- would record assurance nobody has. The series starts where the
-- pricing starts, dated to this migration.
--
-- NO COLUMN ON agent_runs. The basis is not stored: it is derived from
-- the two columns that already decide it — whether the split is present
-- and whether a price is. A column would be the same fact twice, would
-- need a backfill for the rows above, and could drift from the
-- arithmetic it describes; a derivation replays identically in a
-- rebuild and answers correctly for every row written before today.

ALTER TABLE agent_rate_card
    ADD COLUMN IF NOT EXISTS blended_input_share_ppm BIGINT;

ALTER TABLE agent_rate_card DROP CONSTRAINT IF EXISTS agent_rate_card_blend_share_check;
ALTER TABLE agent_rate_card ADD CONSTRAINT agent_rate_card_blend_share_check
    CHECK (blended_input_share_ppm IS NULL
           OR (blended_input_share_ppm >= 0 AND blended_input_share_ppm <= 1000000));

COMMENT ON COLUMN agent_rate_card.blended_input_share_ppm IS
  'The share of a bare TOTAL token count this model assumes was input, '
  'in parts per million: 875000 is 87.5% input. It is an ASSUMPTION and '
  'it lives here so a tenant can read it and change it. NULL means the '
  'model declares none, and a run that reported only a total is then '
  'unpriced — undeclared is unpriced, not assumed. A run that reported '
  'a measured split is priced at the two rates and never consults this.';

-- The one model with runs behind it. See the header for the nine
-- measured splits this 87.5% comes from.
UPDATE agent_rate_card
   SET blended_input_share_ppm = 875000,
       note = note || ' — totals blended at 87.5% input ($7.50/MTok), '
                   || 'measured from nine split-reporting runs 2026-09-20 (backlog 6681a803)'
 WHERE model = 'opus-5[1m]'
   AND blended_input_share_ppm IS NULL;
