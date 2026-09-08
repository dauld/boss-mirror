-- 202608310100 — a completed decision that recorded nothing becomes a
-- finding within a day, not after nine of silence.
--
-- THE DETECTOR HALF of cdfe2e1a, and deliberately the half that ships
-- with the lint rather than instead of it. The lint (workflow_lint
-- Phase 5) refuses NEW workflow versions whose decision points can
-- complete empty, and the corpus already requires `decision` — but
-- neither does anything for packets ALREADY in flight on old pinned
-- versions, whose completion contract is frozen at materialization.
-- Three of the four empty decisions found on 2026-08-29 were nine days
-- old; the defect was never rarity, it was silence. The packet's own
-- recommendation: detector first, because it is what makes the other
-- two verifiable — after the fix, this sweep's count staying zero is
-- how we know it worked.
--
-- WHAT ONE SWEEP INSPECTS (the measurement query, promoted): completed
-- steps whose kind's surface is `approval`, carrying neither authored
-- metadata content (materialization keys excluded) nor notes, since the
-- previous sweep. Zero new = clear. Any new = a judgement was lost
-- while its packet was still warm enough to ask the approver.
--
-- Mirrored in infra/dispatcher/rules.toml, and the seed test compares
-- the two in both directions.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-empty-decisions-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"empty-decisions\"","title":"\"Empty decision sweep\"","metadata.target":"\"empty-decisions\"","metadata.area":"\"protocols\"","metadata.procedure":"\"Count completed steps of approval-surface kinds (ask the step registry for surface, never a kind name) that carry neither authored metadata content nor notes, completed since the previous sweep. Materialization keys (authority_role, context_md, procedure, outcome_kind, started_at, completed_at, sign_off_context) are not content. Zero new: clear, record the count. Any new: action_needed - each one is a human judgement that was recorded nowhere, and the packet it belongs to may still be warm enough to ask. Baseline at first firing: 62 of 100 historical sign-offs are empty (cdfe2e1a); count NEW ones only.\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-09-01', NULL)
ON CONFLICT (name, version) DO NOTHING;
