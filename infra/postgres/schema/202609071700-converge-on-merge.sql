-- 202609071700-converge-on-merge.sql — a merged train fires the cluster
-- converge immediately, instead of waiting up to ~10 min for the
-- cluster-deploy-runner timer to poll forge main.
--
-- On step.done.task where spec_slug = "merged" (the pr-train's merged
-- step), spawn an ops-request for host=forge verb=converge. The
-- already-installed boss-ops-runner (~1-min poll) resolves `converge`
-- from infra/ops/verbs.json and runs
-- `systemctl start --no-block cluster-deploy-runner.service`.
--
-- WHY spec_slug: the merged/deployed/ci steps all share kind=task, so
-- the topic step.done.task alone cannot tell them apart. spec_slug is
-- the step's stable slug, hoisted into the step.done payload as an
-- ALWAYS-PRESENT top-level field precisely so a rule can route on WHICH
-- step completed. The dispatcher expr binder resolves flat identifiers
-- only, and an ABSENT identifier is PredicateFailed -> retry ->
-- dead-letter, not false (the trap notify_on_done_rule.rs guards) —
-- which is why the spec_slug hoist landed BEFORE this rule. Pinned by
-- tests/converge_on_merge_rule.rs.
--
-- LATENCY ACCELERATOR, NOT THE TRIGGER. The whole event path lives
-- inside the cluster (NATS -> dispatcher -> SoR -> ops-request), so it
-- only fires while those are up. cluster-deploy-runner.timer and
-- cluster-watchdog remain the independent floor (they owe nothing to
-- NATS/SoR/the ops-runner), so a cluster-down event path costs latency,
-- never a missed converge. The converge is idempotent + STAMP-guarded,
-- so a redundant or spurious fire is a safe no-op.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('converge-on-merge', 1, 'active', 'step.done.task',
   'spec_slug = "merged"',
   '[{"handler":"jobs.spawn","args":{"kind":"\"ops-request\"","subject_kind":"\"custom\"","subject":"\"forge\"","title":"\"converge on forge — a train merged to main\"","metadata.host":"\"forge\"","metadata.verb":"\"converge\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
