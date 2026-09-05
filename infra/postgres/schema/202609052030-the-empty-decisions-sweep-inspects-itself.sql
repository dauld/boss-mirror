-- 202609052030 — the empty-decisions sweep gets its executor.
--
-- The spawn rule (202608310100) files a maintenance-sweep packet daily
-- whose `Inspect: empty-decisions` checklist step nothing completed, so
-- the packets piled up. This seeds the rule that routes that Inspect to
-- the maintenance.sweep.inspect handler (ee8ec68a: mechanical
-- inspections become automation). Rides step.ready.* and self-filters,
-- the same shape as the marker completer. Mirrored in
-- infra/dispatcher/rules.toml; dispatcher_rules_seed_matches_toml
-- compares the two in both directions.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('inspect-empty-decisions-sweep-on-step-ready', 1, 'active', 'step.ready.*', NULL, '[{"handler":"maintenance.sweep.inspect","args":{}}]'::jsonb, NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
