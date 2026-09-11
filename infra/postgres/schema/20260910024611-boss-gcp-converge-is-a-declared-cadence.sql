-- 20260910024611 — the boss-gcp self-converge loop is a declared cadence
-- (backlog 408c81f6).
--
-- WHAT LANDED BESIDE THIS. boss-gcp now converges its own systemd units
-- from forge main every 30 minutes (infra/gcp/boss-gcp-converge.sh +
-- the `units` mode of infra/deploy-services.sh), because the 2026-09-04
-- conductor cutover retired the deploy hop that was the ONLY thing
-- installing units on that host and nothing replaced it. Measured
-- 2026-09-10: the five-minute estate unit observer's last packet was
-- filed ON the cutover day and was still open, the hourly
-- conservation-invariants timer's on 2026-08-19, and
-- maintenance-ml-inference-batch has never filed one at all.
--
-- WHY IT HAS TO BE DECLARED HERE. The loop that ends "landed but never
-- installed" must not itself be able to stop quietly — that would be
-- the same defect one level up, and the silence would hide every unit
-- fix behind it. `cadence-silence-sweep-daily` (202609082300) compares
-- each declared cadence against the newest packet of that kind and
-- files one alarm per silent kind; an undeclared chore is invisible to
-- it, which is exactly how the ML inference batch went 23 nights
-- unmissed. 30 is the timer's own OnUnitActiveSec, and
-- infra/lint/timers-leave-a-packet.sh check 9 fails CI if the two
-- numbers ever disagree (CLAUDE.md §9a: the interval lives twice, so
-- it is pinned).
--
-- RETIRE v1 BEFORE INSERTING v2 (the 148/150 lesson):
-- `dispatcher_rules_one_active_per_name` rejects two active versions,
-- and this file runs in one transaction so there is no window where the
-- sweep has no declaration at all. The args are otherwise byte-for-byte
-- v1's; mirrored in infra/dispatcher/rules/cadence-silence-sweep-daily.toml,
-- which dispatcher_rules_seed_matches_toml compares in BOTH directions.
UPDATE dispatcher_rules
   SET status = 'retired'
 WHERE name = 'cadence-silence-sweep-daily'
   AND version = 1;

INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('cadence-silence-sweep-daily', 2, 'active', NULL, NULL,
   '[{"handler":"cadence.silence.sweep","args":{
      "interval_minutes.maintenance-audit-integrity":"1440",
      "interval_minutes.maintenance-backup":"1440",
      "interval_minutes.maintenance-boss-gcp-converge":"30",
      "interval_minutes.maintenance-cluster-converge":"10",
      "interval_minutes.maintenance-cluster-watchdog":"5",
      "interval_minutes.maintenance-conservation-invariants":"60",
      "interval_minutes.maintenance-disk-floor-sweep":"60",
      "interval_minutes.maintenance-estate-observe-host":"1440",
      "interval_minutes.maintenance-estate-observe-units":"5",
      "interval_minutes.maintenance-files-gc":"1440",
      "interval_minutes.maintenance-forge-converge":"10",
      "interval_minutes.maintenance-ledger-recognize":"1440",
      "interval_minutes.maintenance-ledger-replay":"1440",
      "interval_minutes.maintenance-messages-purge":"1440",
      "interval_minutes.maintenance-ml-inference-batch":"1440",
      "interval_minutes.maintenance-reap-ci-jobs":"1440",
      "interval_minutes.maintenance-search-reindex":"10",
      "interval_minutes.maintenance-views-catchup":"5"
   }}]'::jsonb,
   NULL, 'daily', DATE '2026-09-09', NULL)
ON CONFLICT (name, version) DO NOTHING;
