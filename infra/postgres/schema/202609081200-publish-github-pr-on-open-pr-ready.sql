-- 202609081200-publish-github-pr-on-open-pr-ready.sql — publish-to-github
-- v6's machine step (design packet 7b59af2c, David 2026-09-08).
--
-- When the protocol's `open-pr` step becomes READY — David signed the
-- approve step `approved` — spawn an ops-request for host=forge
-- verb=publish-github-pr. The root boss-ops-runner on the forge (~1-min
-- poll) resolves the verb from infra/ops/verbs.json and runs
-- infra/forge/publish-github-pr.sh: fetch forge main, build the dated
-- SNAPSHOT commit on the mirror's main (`git commit-tree <forge tree>
-- -p <mirror main>` — the mirror's history is publish snapshots, and a
-- merge conflicts in hundreds of files), push it to the dauld fork's
-- publish/<date> branch, `gh pr create` against algedonic-dev/boss as
-- dauld, then complete `open-pr` with `pr_url`. The merge on GitHub is
-- the second, human gate; nothing here touches it.
--
-- WHY metadata.ops_verb, not spec_slug: the `step.ready.<kind>` payload
-- carries the step's `metadata` (its metadata_defaults, stamped at
-- materialization) and does NOT hoist spec_slug the way step.done does.
-- v6's bundle row stamps `ops_verb = "publish-github-pr"` on open-pr, so
-- the marker is present exactly when this step is the one going ready.
-- On every other task step the path is Absent, which boss-expr treats
-- as UNEQUAL to the literal — the predicate reads false, never
-- PredicateFailed, so the shared step.ready.task topic never
-- dead-letters (the trap notify_on_done_rule.rs guards). Pinned by
-- tests/publish_github_pr_rule.rs.
--
-- ONCE, ON READINESS. The rule listens to step.ready, so the verb runs
-- when the approval makes the step ready and not again when the machine
-- completes it. If the verb fails, the ops-request records the output
-- and exit code and open-pr stays ready and visibly unfinished; a
-- person re-files the ops-request (POST /api/jobs kind=ops-request
-- host=forge verb=publish-github-pr) rather than the rule guessing.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('publish-github-pr-on-open-pr-ready', 1, 'active', 'step.ready.task',
   'metadata.ops_verb = "publish-github-pr"',
   '[{"handler":"jobs.spawn","args":{"kind":"\"ops-request\"","subject_kind":"\"custom\"","subject":"\"forge\"","title":"\"publish the mirror PR from the forge — a publish was approved\"","metadata.host":"\"forge\"","metadata.verb":"\"publish-github-pr\""}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
