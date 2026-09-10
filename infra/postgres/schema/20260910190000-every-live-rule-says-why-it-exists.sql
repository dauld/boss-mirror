-- 20260910190000-every-live-rule-says-why-it-exists.sql — seed the four
-- dispatcher rules that were authored LIVE and never written back, so a
-- fresh database enforces the same registry the cluster does.
--
-- THE GAP THIS CLOSES (backlog 8d471ec5). `POST /api/dispatcher/rules`
-- plus publish authors a rule straight into the runtime registry, with
-- no file under infra/dispatcher/rules/ and no migration. Four rules
-- reached the table that way and stayed there:
--
--   auto-park-on-gate-green            the mechanism that files a car on
--                                      a green gate — how anything
--                                      reaches the dock at all
--   estate-alarm-on-comparison         the estate raiser; an ALARM
--   spawn-keg-return-on-delivery       brewery demo tenant
--   spawn-tasting-panel-on-brew-close  brewery demo tenant
--
-- Two of the four were deliberate two-phase flips — the handler shipped
-- INERT ("INERT until a rule on step.done.gate-verdict is published",
-- 898e41b1; "Inert until the rule on jobs.estate.compared publishes,
-- after deploy — the auto-park two-phase flip", a1951772) — and phase
-- two was the live POST. Nothing wrong with the flip; the defect is that
-- phase two never became a file. So `dispatcher_rules_seed_matches_toml`
-- never saw them (a TestDb is seeded from migrations) and
-- dispatcher-rules-ratchet.sh reported "OK (60 rules, each saying why it
-- exists)" — true about the DIRECTORY, false about the SYSTEM. Measured
-- 2026-09-10 18:52 UTC: 60 live, 56 authored, the same four.
--
-- WHY A MIGRATION AND NOT JUST FILES. The files make the rules
-- reviewable; this makes them REAL on a database nobody has hand-poked.
-- Without it the seed pin fails the other way (a file with no row), and
-- a fresh deployment — a new tenant, a rebuilt cluster, every TestDb —
-- would silently run without auto-park, without the estate alarm, and
-- without the brewery demo's two spawn loops.
--
-- A NO-OP AGAINST THE LIVE DATABASE. Every row below is copied verbatim
-- from `GET /api/dispatcher/rules` (names, topics, `when` expressions,
-- `do` steps, versions — including the two that are at version 2), and
-- ON CONFLICT (name, version) DO NOTHING means applying this where the
-- row already exists changes nothing. It is not an UPDATE on purpose: if
-- a live row has drifted from what is written here, the right answer is
-- a version bump a reader can see, not a migration that silently
-- overwrites the running registry.
--
-- VERSION 2 ON THE TWO BREWERY RULES is the live state and is recorded
-- as such. v1 of spawn-keg-return-on-delivery referenced `job_id`, which
-- is not in the `jobs.job.closed` payload (the field is `id`), and on
-- 2026-08-24 its arg-resolution failure took three neighbour rules on
-- that topic down with it — the incident that made rule matching
-- per-rule rather than all-or-nothing (boss-dispatcher
-- rules/registry.rs, `MatchOutcome`). No v1 row is inserted: a fresh
-- database has no history to retire, and inserting a retired v1 would
-- claim one that never happened there.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('auto-park-on-gate-green', 1, 'active', 'step.done.gate-verdict', NULL,
   '[{"handler":"jobs.auto-park","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL),
  ('estate-alarm-on-comparison', 1, 'active', 'jobs.estate.compared', NULL,
   '[{"handler":"estate.alarm","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL),
  ('spawn-keg-return-on-delivery', 2, 'active', 'jobs.job.closed',
   'kind = "wholesale-keg-order" AND outcome = "completed"',
   '[{"handler":"jobs.spawn","args":{"kind":"\"keg-return\"","subject_kind":"\"account\"","subject":"subject_id","title":"\"Keg fleet return\"","metadata.order_job_id":"id"}}]'::jsonb,
   NULL, NULL, NULL, NULL),
  ('spawn-tasting-panel-on-brew-close', 2, 'active', 'jobs.job.closed',
   '(kind = "morning-brew" OR kind = "morning-brew-ipa" OR kind = "morning-brew-stout" OR kind = "morning-brew-lager" OR kind = "morning-brew-hazy") AND outcome = "completed"',
   '[{"handler":"jobs.spawn","args":{"kind":"\"tasting-panel\"","subject_kind":"\"company\"","subject":"\"brewery\"","title":"\"Tasting panel\"","metadata.batch_job_id":"id"}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
