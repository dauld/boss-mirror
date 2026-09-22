-- 20260922051115-a-pre-instrumentation-zero-reads-as-no-count.sql
-- — `agent_runs_read`: the relation every reader of a run should read.
--
-- Origin: backlog f19589ac, found by the builder of 65c9c05a naming
-- the reading trap its own fix creates. 20260919194503 made
-- `total_tokens` NULL when nobody measured it, which is right for
-- every row written after it — and left the column two-era. Rows
-- before that migration carry `total_tokens = 0` for the same fact,
-- distinguishable ONLY by a companion `detail.tokens_reported: false`.
-- Re-measured against the live table on 2026-09-22: 95 rows, 13 of
-- them a zero, all 13 carrying the flag, 28 an explicit NULL. So the
-- roll-up at /api/agent-runs/cost counted those 13 as MEASURED zeros —
-- `total_only_runs`, folded into a sum — and any query spanning the
-- whole table averages 13 fake zeros into its answer. With effort now
-- genuinely selecting the CPU, the first person to ask what a run
-- costs by effort queries straight across that boundary.
--
-- A VIEW, NOT A BACKFILL, AND NOT AN EXCLUSION. `agent_runs` is
-- insert-once (ON CONFLICT (run_id) DO NOTHING) and a projection of
-- `agents.run.recorded`: an UPDATE here would be the projection
-- asserting something no event says, and a rebuild from the log would
-- put the zeros straight back. A recorded_at bound would work too, and
-- was the other option f19589ac offered, but it discards the runs in
-- that window that DID report. The view only reinterprets the ones
-- that did not, and it reinterprets them from the row's OWN evidence —
-- the flag the writer of the era put there for exactly this reading —
-- rather than from a date a reader has to remember. Nothing is
-- rewritten; a reader stops having to know.
--
-- The flag is not a second copy of the column (§9a): no row carries
-- both spellings. Before the cutover the flag was the only place the
-- fact lived; after it the column says it and the flag is gone. This
-- view is the one place that knows both spellings, so no query has to.
--
-- THE THREE ERA BOUNDARIES, BY SHA, IN ONE PLACE. fd5ce137 asked for
-- this: a caveat in someone's head is the mostly-sure failure the
-- effort-vs-cost series exists to avoid. Each sha below is the merge
-- that landed the car, read from git log, and each bounds a DIFFERENT
-- column's honesty:
--
--   28937f2d (2026-09-19T16:40Z, car 8f1de7bf) — before it, a run
--     recorded no effort and its outcome was the literal 'success'.
--   d0a307d8 (2026-09-19T17:30Z, car e720dd00) — before it, a run's
--     declared effort did not select the agent definition, so an
--     effort label in this window was recorded but never applied:
--     worse than absent, because it looks like data.
--   fa45a86d (2026-09-19T21:00Z, car 65c9c05a, migration
--     20260919194503) — before it, "no count" was spelled 0 + the
--     flag; after it, NULL. This view is what makes that one boundary
--     invisible to a reader; the two above still bound any
--     effort-vs-cost comparison and cannot be papered over the same
--     way, because the rows carry no evidence of what actually ran.

CREATE OR REPLACE VIEW agent_runs_read AS
SELECT
    run_id,
    actor_id,
    model,
    started_at,
    finished_at,
    outcome,
    error,
    -- The one reinterpretation. A zero WITHOUT the flag is a row that
    -- said it measured zero, and it keeps saying so.
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
    recorded_at
FROM agent_runs;

COMMENT ON VIEW agent_runs_read IS
  'agent_runs as a READER should read it: a pre-20260919194503 zero '
  'carrying detail.tokens_reported false reads as NULL, because that '
  'is what it meant. The table keeps the row exactly as recorded — '
  'this view is a projection, not a backfill (backlog f19589ac). Read '
  'this relation, not the table, unless you are asking what was '
  'literally written. The three era boundaries that bound any '
  'cross-cutover comparison are recorded by sha in this view''s '
  'migration file.';
