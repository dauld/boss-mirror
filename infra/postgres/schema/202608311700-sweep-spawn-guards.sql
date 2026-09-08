-- 202608311700-sweep-spawn-guards.sql — a daily sweep asks before it
-- spawns.
--
-- DEFECT 0517387b, measured 2026-08-29: every maintenance-sweep
-- spawner fired unconditionally — schedule + jobs.spawn and no when —
-- so an obligation nobody could discharge accumulated one packet per
-- day (5 open cluster-conformance sweeps at measurement). Same class
-- as 148 (car spawn) and 149 (publish spawn), now retired for the
-- whole sweep family at once via the GENERIC guard
-- `NOT open_job_exists("maintenance-sweep", "<subject>")` — a sweep's
-- subject IS its target, so no metadata breadcrumb is needed.
--
-- MUST LAND WITH THE CAR THAT ADDS open_job_exists to the helper
-- resolver (same caveat 149 recorded): against an older binary a
-- schedule-rule guard naming an unknown helper SKIPS the rule for the
-- day with a warning — quiet, not double-spawning, but still wrong.
--
-- RETIRE v1 BEFORE INSERTING v2 — dispatcher_rules_one_active_per_name
-- is a per-statement partial unique index (see 148/149 and
-- a-registry-version-bump-retires-before-it-inserts).

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-disk-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-disk-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "disk-headroom")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"disk-headroom\"","title":"\"Disk headroom sweep\"","metadata.target":"\"disk-headroom\"","metadata.area":"\"infra\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-build-caches-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-build-caches-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "stale-build-caches")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"stale-build-caches\"","title":"\"Stale build cache sweep\"","metadata.target":"\"stale-build-caches\"","metadata.area":"\"infra\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-image-freshness-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-image-freshness-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "image-freshness")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"image-freshness\"","title":"\"CI image freshness sweep\"","metadata.target":"\"image-freshness\"","metadata.area":"\"ci\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-converge-lag-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-converge-lag-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "deploy-convergence")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"deploy-convergence\"","title":"\"Deploy convergence sweep\"","metadata.target":"\"deploy-convergence\"","metadata.area":"\"deploy\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-cluster-conformance-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-cluster-conformance-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "cluster-conformance")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"cluster-conformance\"","title":"\"Cluster conformance sweep\"","metadata.target":"\"cluster-conformance\"","metadata.area":"\"cluster\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-17', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-empty-decisions-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-empty-decisions-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "empty-decisions")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"empty-decisions\"","title":"\"Empty decision sweep\"","metadata.target":"\"empty-decisions\"","metadata.area":"\"protocols\"","metadata.procedure":"\"Count completed steps of approval-surface kinds (ask the step registry for surface, never a kind name) that carry neither authored metadata content nor notes, completed since the previous sweep. Materialization keys (authority_role, context_md, procedure, outcome_kind, started_at, completed_at, sign_off_context) are not content. Zero new: clear, record the count. Any new: action_needed - each one is a human judgement that was recorded nowhere, and the packet it belongs to may still be warm enough to ask. Baseline at first firing: 62 of 100 historical sign-offs are empty (cdfe2e1a); count NEW ones only.\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-09-01', NULL)
ON CONFLICT (name, version) DO NOTHING;

UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'maintenance-sweep-doc-status-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-doc-status-daily', 2, 'active', NULL,
   'NOT open_job_exists("maintenance-sweep", "doc-status")',
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"doc-status\"","title":"\"Design doc status sweep\"","metadata.target":"\"doc-status\"","metadata.area":"\"docs\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-17', NULL)
ON CONFLICT (name, version) DO NOTHING;
