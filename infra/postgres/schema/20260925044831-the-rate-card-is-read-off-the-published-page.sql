-- 20260925044831-the-rate-card-is-read-off-the-published-page.sql —
-- every rate-card row restated from Anthropic's published pricing page,
-- as read, with the read date in its note; Fable 5.1's cache-read rate
-- corrected from $1.00 to $0.25 per MTok.
--
-- WHAT WAS MEASURED (backlog 8e1a2a6f, filed 2026-09-25 from run
-- 7e99b679). 20260924001627 seeded every row's cache-read rate as 0.1x
-- its input rate — the standard prompt-caching multiplier — without
-- reading each model's row. The page prices two models off that
-- multiplier: Opus 5.5 at 0.05x (20260925023146 wrote its row as read)
-- and Fable 5.1 at 0.025x, which the card still carried at 0.1x: $1.00
-- per MTok against the published $0.25. Cache reads were a median 96.8%
-- of a metered run's tokens (e6b2066f), so a Fable 5.1 run was priced
-- high on the component that is nearly all of it — a 1,509,040-token
-- run reads $2.2954 at the old rate and $1.1929 at the published one.
--
-- WHERE THE NUMBERS CAME FROM. https://platform.claude.com/docs/en/
-- about-claude/pricing, "Model pricing" table, read 2026-09-25 by the
-- builder of this car (per MTok: base input / 5m cache writes / cache
-- hits and refreshes / output):
--
--   Claude Fable 5.1   $10 / $12.50 / $0.25 (footnote 1: 0.025x) / $50
--   Claude Fable 5     $10 / $12.50 / $1                         / $50
--   Claude Opus 5.5    $4  / $5     / $0.20 (footnote 2: 0.05x)  / $20
--   Claude Opus 5      $5  / $6.25  / $0.50                      / $25
--   Claude Opus 4.8    $5  / $6.25  / $0.50                      / $25
--   Claude Sonnet 5    $2  / $2.50  / $0.20                      / $10
--   Claude Sonnet 4.6  $3  / $3.75  / $0.30                      / $15
--   Claude Haiku 4.5   $1  / $1.25  / $0.10                      / $5
--
-- Footnote 3 on Sonnet 5: the $2/$10 introductory price "is now the
-- standard price". The "Long context pricing" section puts the full 1M
-- window at standard pricing for Claude 4.6 and later, so each `[1m]`
-- row carries its base model's rates. Every row is WRITTEN AS READ — no
-- rate below is derived by a multiplier — and nine of the ten matched
-- the card already; only fable-5-1's cache read moves.
--
-- EACH NOTE IS REPLACED, not appended to. The earlier notes accumulated
-- " — cache read 0.1x / write 1.25x input", which is false of fable-5-1
-- and was never read for the others; a note is a statement about the
-- row as it stands, and the history is these migration files.
--
-- EXISTING agent_runs ROWS ARE LEFT ALONE, as every pricing change before
-- this one left them: a run is priced once, at record time, and a
-- rebuild replays the recorded figure. A metered Fable 5.1 run recorded
-- before this file keeps its high figure; re-pricing one is a decision
-- for its own car, not a side effect of this one.
--
-- No row is inserted, so boss_jobs::agent_spec::known_models (which reads
-- the INSERT blocks) is unchanged. The other half of the item — a dated
-- snapshot id such as claude-haiku-4-5-20251001 recorded as a model no
-- row names — is fixed where the id is spelled (boss-cli
-- transcript_usage::card_spelling), not by a row per snapshot.

UPDATE agent_rate_card
   SET input_usd_micros_per_mtok = v.input,
       output_usd_micros_per_mtok = v.output,
       cache_read_usd_micros_per_mtok = v.cache_read,
       cache_write_usd_micros_per_mtok = v.cache_write,
       note = v.note
  FROM (VALUES
    ('fable-5-1',    10000000, 50000000,  250000, 12500000, 'Claude Fable 5.1 — $10.00 in / $50.00 out / $0.25 cache read (0.025x) / $12.50 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('fable-5',      10000000, 50000000, 1000000, 12500000, 'Claude Fable 5 — $10.00 in / $50.00 out / $1.00 cache read / $12.50 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('opus-5-5',      4000000, 20000000,  200000,  5000000, 'Claude Opus 5.5 — $4.00 in / $20.00 out / $0.20 cache read (0.05x) / $5.00 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('opus-5-5[1m]',  4000000, 20000000,  200000,  5000000, 'Claude Opus 5.5, 1M context — same rates as opus-5-5: the 1M window is at standard pricing for Claude 4.6 and later (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('opus-5',        5000000, 25000000,  500000,  6250000, 'Claude Opus 5 — $5.00 in / $25.00 out / $0.50 cache read / $6.25 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('opus-5[1m]',    5000000, 25000000,  500000,  6250000, 'Claude Opus 5, 1M context — same rates as opus-5: the 1M window is at standard pricing for Claude 4.6 and later (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f); no blend since 2026-09-24 (backlog e6b2066f)'),
    ('opus-4-8',      5000000, 25000000,  500000,  6250000, 'Claude Opus 4.8 — $5.00 in / $25.00 out / $0.50 cache read / $6.25 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('sonnet-5',      2000000, 10000000,  200000,  2500000, 'Claude Sonnet 5 — $2.00 in / $10.00 out / $0.20 cache read / $2.50 5m cache write per MTok, the introductory price now standard (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('sonnet-4-6',    3000000, 15000000,  300000,  3750000, 'Claude Sonnet 4.6 — $3.00 in / $15.00 out / $0.30 cache read / $3.75 5m cache write per MTok (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)'),
    ('haiku-4-5',     1000000,  5000000,  100000,  1250000, 'Claude Haiku 4.5 — $1.00 in / $5.00 out / $0.10 cache read / $1.25 5m cache write per MTok; dated ids (claude-haiku-4-5-20251001) are recorded as this row (platform.claude.com/docs/en/about-claude/pricing, read 2026-09-25; backlog 8e1a2a6f)')
  ) AS v (model, input, output, cache_read, cache_write, note)
 WHERE agent_rate_card.model = v.model;
