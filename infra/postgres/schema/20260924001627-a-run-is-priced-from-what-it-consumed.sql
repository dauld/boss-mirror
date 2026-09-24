-- 20260924001627-a-run-is-priced-from-what-it-consumed.sql — price an
-- agent run from the four counts its turns were billed by, and stop
-- pricing the size of its final context window as though it were spend.
--
-- WHAT WAS MEASURED (backlog e6b2066f, 2026-09-23). A dispatched
-- builder reports the harness's `subagent_tokens`, and `boss dispatch
-- --report` recorded it as `total_tokens`. That number is the size of
-- the run's FINAL context window, not what the run consumed: it matched
-- the last turn's input + cache_write + cache_read + output within 1% on
-- 62 of 68 matched runs (run aa04ef77 recorded 154,978; its last turn
-- was 153,446). Summed over every turn, the tokens a run was billed for
-- were a median 48x the recorded total (25x to 440x), and cache reads
-- were a median 96.8% of them. Priced at the card's rates with the
-- cache multipliers, real spend was a median 4.9x the recorded
-- usd_micros (2.7x to 32x): 63 priced runs recorded $81.89 against about
-- $557. The per-turn counts were never missing — the harness writes
-- them into every run's transcript as `message.usage` — they were just
-- never read.
--
-- WHAT THIS FILE DOES, in four parts:
--
--  1. THE RATE CARD PRICES THE CACHE. Two nullable columns, one rate
--     each for prompt tokens read from the cache and written to it.
--     Seeded for every row from that row's own input rate: 0.1x to read
--     and 1.25x to write, Anthropic's published prompt-caching
--     multipliers (the same two the packet's measurement used). The
--     write rate is the 5-minute cache's; a 1-hour cache write costs 2x
--     input, so a run that writes the 1-hour cache is priced at a FLOOR
--     — the reader records the 1-hour count beside the run so the gap
--     is visible, and a third rate is a later migration if it matters.
--     NULL stays "no cache rate declared", which leaves a metered run
--     on that model unpriced rather than priced at three of four terms.
--
--  2. A RUN CAN CARRY ITS CACHE COUNTS. Two nullable columns on
--     agent_runs, both-or-neither, and only beside a measured split:
--     together with input_tokens (the UNCACHED input, as the harness
--     bills it) and output_tokens they are the four counts. total_tokens
--     stays the one column every run can answer, and for a metered run
--     it is DERIVED as the four-way sum — what the run processed — so
--     the constraint that pinned it to input + output is widened, not
--     dropped.
--
--  3. THE BLEND IS RETIRED. 20260920223003 declared an 87.5% input
--     share for opus-5[1m] so a bare total could be priced at all. The
--     bare total it priced was the final context size, so the blend
--     priced the wrong quantity at about a fifth of the spend, and it
--     rested on nine hand-rounded splits. A run whose transcript is read
--     is now metered; one whose is not is recorded in full and reads as
--     UNPRICED — counted out loud by the roll-up — rather than carrying
--     a figure known to be about five times low. The column and its
--     mechanism stay, because a tenant may still declare a ratio.
--
--  4. OVER BUDGET IS A READING. A claim over the agent's hourly budget
--     used to be refused with a 409; it is now admitted, and the numbers
--     the refusal carried ride the log as agents.claim.over_budget,
--     committed with the claim. David's direction on the packet
--     (2026-09-23): the goal is a cost signal protocols can read, and
--     budgets must not limit building — and with accurate figures the
--     $40 hour would have refused claims all day. The recorder's own
--     refusal (agents.run.denied, no row) goes the same way in code: an
--     over-cap run is a row whose budget reads deny.
--
-- EXISTING ROWS ARE LEFT ALONE, as every earlier pricing change left
-- them: pricing happens once, at record time, and a rebuild replays the
-- recorded figure. The 63 blended rows keep their figures and keep
-- saying `blended`; the series of metered figures starts here.

-- 1. The cache rates.
ALTER TABLE agent_rate_card
    ADD COLUMN IF NOT EXISTS cache_read_usd_micros_per_mtok BIGINT;
ALTER TABLE agent_rate_card
    ADD COLUMN IF NOT EXISTS cache_write_usd_micros_per_mtok BIGINT;

ALTER TABLE agent_rate_card DROP CONSTRAINT IF EXISTS agent_rate_card_cache_rates_check;
ALTER TABLE agent_rate_card ADD CONSTRAINT agent_rate_card_cache_rates_check
    CHECK ((cache_read_usd_micros_per_mtok IS NULL OR cache_read_usd_micros_per_mtok >= 0)
       AND (cache_write_usd_micros_per_mtok IS NULL OR cache_write_usd_micros_per_mtok >= 0));

COMMENT ON COLUMN agent_rate_card.cache_read_usd_micros_per_mtok IS
  'Micro-USD per million prompt tokens READ FROM the cache. NULL means the '
  'model declares none, and a metered run on it is unpriced rather than '
  'priced at three of its four counts (backlog e6b2066f).';
COMMENT ON COLUMN agent_rate_card.cache_write_usd_micros_per_mtok IS
  'Micro-USD per million prompt tokens WRITTEN TO the cache, at the '
  '5-minute cache''s rate. A 1-hour cache write costs more, so a run that '
  'writes one is priced at a floor (backlog e6b2066f).';

UPDATE agent_rate_card
   SET cache_read_usd_micros_per_mtok = input_usd_micros_per_mtok / 10,
       cache_write_usd_micros_per_mtok = input_usd_micros_per_mtok * 5 / 4,
       note = note || ' — cache read 0.1x / write 1.25x input (prompt-caching '
                   || 'multipliers, 5-minute write; backlog e6b2066f)'
 WHERE cache_read_usd_micros_per_mtok IS NULL
   AND cache_write_usd_micros_per_mtok IS NULL;

-- 3. The blend, retired on the one row that declared it.
UPDATE agent_rate_card
   SET blended_input_share_ppm = NULL,
       note = note || ' — blend retired 2026-09-24: the total it priced was the final '
                   || 'context size, not spend (backlog e6b2066f)'
 WHERE model = 'opus-5[1m]'
   AND blended_input_share_ppm IS NOT NULL;

-- 2. The cache counts on a run.
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS cache_read_tokens BIGINT;
ALTER TABLE agent_runs ADD COLUMN IF NOT EXISTS cache_write_tokens BIGINT;

ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_cache_tokens_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_cache_tokens_check
    CHECK ((cache_read_tokens IS NULL OR cache_read_tokens >= 0)
       AND (cache_write_tokens IS NULL OR cache_write_tokens >= 0));

-- Both cache counts or neither, and only beside both halves: the four
-- counts are one measurement.
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_cache_is_whole_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_cache_is_whole_check
    CHECK ((cache_read_tokens IS NULL) = (cache_write_tokens IS NULL)
       AND (cache_read_tokens IS NULL OR input_tokens IS NOT NULL));

-- The total pin, widened to the four-way sum. A split row's cache
-- columns are NULL and read as nothing added, so every row that passed
-- the old check passes this one. The presence clause is
-- 20260919194503's, kept: a NULL total makes the equality NULL, which
-- Postgres reads as a SATISFIED check.
ALTER TABLE agent_runs DROP CONSTRAINT IF EXISTS agent_runs_total_matches_split_check;
ALTER TABLE agent_runs ADD CONSTRAINT agent_runs_total_matches_split_check
    CHECK (
        input_tokens IS NULL
        OR (total_tokens IS NOT NULL
            AND total_tokens = input_tokens + output_tokens
                             + COALESCE(cache_read_tokens, 0) + COALESCE(cache_write_tokens, 0))
    );

COMMENT ON COLUMN agent_runs.cache_read_tokens IS
  'Prompt tokens the run read from the cache, summed over its turns from '
  'the transcript''s message.usage. NULL on every run that was not metered '
  '(backlog e6b2066f).';
COMMENT ON COLUMN agent_runs.cache_write_tokens IS
  'Prompt tokens the run wrote to the cache, summed over its turns. NULL on '
  'every run that was not metered (backlog e6b2066f).';
COMMENT ON COLUMN agent_runs.total_tokens IS
  'What the run spent, when anyone counted. For a METERED run (cache '
  'columns set) it is the four-way sum — everything the run processed. '
  'For a split, input + output. For a total-only run it is whatever the '
  'reporter had, and for a dispatched builder that was the harness''s '
  'subagent_tokens: the size of the FINAL context window, not what the run '
  'consumed (backlog e6b2066f measured it a median 48x smaller). Read a '
  'total-only row as a context size.';

-- The read relation carries the two new columns, appended so the view's
-- existing column order is unchanged.
CREATE OR REPLACE VIEW agent_runs_read AS
SELECT
    run_id,
    actor_id,
    model,
    started_at,
    finished_at,
    outcome,
    error,
    CASE
        WHEN total_tokens = 0 AND detail ->> 'tokens_reported' = 'false' THEN NULL
        ELSE total_tokens
    END AS total_tokens,
    input_tokens,
    output_tokens,
    tool_calls,
    usd_micros,
    priced_by,
    job_id,
    branch,
    detail,
    budget,
    recorded_at,
    cache_read_tokens,
    cache_write_tokens
FROM agent_runs;

-- 4. The reading the claim door leaves in place of a refusal.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('agents.claim.over_budget', 'jobs', 'An agent claimed a step whose budget did not fit its hourly cap: the claim was admitted and this is the reading (spent, budget, cap, window), a cost signal rather than a refusal', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
