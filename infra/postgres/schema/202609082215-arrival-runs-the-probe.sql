-- 202609082215-arrival-runs-the-probe.sql — a train arriving runs each
-- boarded car's recorded probe against production (backlog 28ac45ab).
--
-- On jobs.job.closed where kind = "pr-train" AND outcome = "arrived",
-- the jobs.run-car-probes handler reads the cars aboard that train
-- (metadata.train = the train's id) and, for each one that recorded a
-- `proof_probe` at park time (`boss gate --park-probe/--park-expect`,
-- copied onto the car by the auto-park handler) and still has its
-- `proven` step open, files ONE ops-request: host=forge,
-- verb=run-car-probe, args=[car id]. The root boss-ops-runner on the
-- forge (~1-min poll) resolves the verb from infra/ops/verbs.json and
-- runs infra/forge/run-car-probe.sh, which re-reads the car, refuses
-- unless it merged and recorded a probe, runs the probe AS DAVID (never
-- root) under a timeout, judges it by the two rules `boss prove`
-- applies (exit 0 + the expected string printed), and completes
-- `proven` with the same proof record `boss prove` writes — or stamps
-- `proof_attempt` on the car and leaves `proven` ready.
--
-- WHY ARRIVED, NOT MERGED: a probe means something only once the change
-- is in production, and `arrived` is `converged AND ci` — the first
-- moment every car aboard is both merged and rolled. `merged` fires ~10
-- minutes too early and would record honest failures against code not
-- yet deployed.
--
-- WHY NOT A SHELL IN THE DISPATCHER: the dispatcher is the queue watcher
-- (clock, threshold, matchmaking — David, 2026-08-14); a hung probe
-- would hold a JetStream consumer; and the vantage a probe needs
-- (kubectl as the converge user, the converged checkout, the forge
-- journal) lives on the forge host. So the run goes through the
-- existing ops-request door, one packet per car, and the verdict lands
-- on the car and on the packet's exit_code.
--
-- WHY kind/outcome, NOT spec_slug: the jobs.job.closed marker carries
-- `kind` and `outcome` on every emit site (outcome null on a catch-all
-- close), so the predicate reads false — never PredicateFailed → retry
-- → dead-letter — on the many closes sharing this topic. Pinned by
-- tests/run_car_probes_rule.rs.
--
-- CARS WITHOUT A PROBE STAY HAND-PROVEN: an event-bound car (recorded
-- `proof_event` instead) and a car whose builder wrote no probe are
-- left alone; a car already proven is not re-proven; a car with an open
-- run-car-probe request is not asked twice (at-least-once delivery).
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('run-car-probes-on-train-arrived', 1, 'active', 'jobs.job.closed',
   'kind = "pr-train" AND outcome = "arrived"',
   '[{"handler":"jobs.run-car-probes","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
