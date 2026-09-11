-- 202609082300 — a declared cadence with no packet files an alarm
-- (backlog ecca2f43).
--
-- WHAT WAS MEASURED, 2026-09-08. Two silences the system of record
-- knew about and never spoke about:
--   * maintenance-ml-inference-batch — dead 23 nights (e109f57e). Its
--     unit died in a hard ExecStartPre every night, so the run never
--     started and the packet never opened. ZERO packets of that kind
--     exist in the SoR; the ML predictions went three weeks stale.
--   * maintenance-estate-observe-units — a FIVE-MINUTE observer whose
--     newest packet was opened 2026-09-04 and never followed
--     (408c81f6). Four days of silence from the loop that answers "is
--     boss-train.service alive".
-- Both were found by a human looking, which is the defect: CLAUDE.md
-- §Diagnosis, "a check nobody reads is a check that is not running".
--
-- WHY THE ESTATE ALARM DOES NOT COVER IT. estate.alarm's silence sweep
-- (a7a19a1a) watches the estate OBSERVATION series and raises when a
-- HOST stops being observed. A chore's evidence is a PACKET, not an
-- observation, so a chore that stops running is invisible to it. This
-- rule is the same idea one layer over, and reuses its idioms verbatim:
-- one bounded dedup read, an `open packet carries the key` dedup, a
-- truncation HOLD rather than a blind re-raise, a settled-recently
-- suppression window, and a best-effort accumulator so one kind's
-- failed read never blocks the other sixteen.
--
-- WHY A CLOCK RULE. The condition is the events that did NOT happen, so
-- there is no edge to react to; the dispatcher's schedule runner is the
-- sanctioned home for exactly that (the network-census and
-- maintenance-sweep dailies are the precedent). Day granularity is what
-- the runner offers, which is why the handler floors its silence window
-- at six hours — a daily sweep must not cry wolf about a five-minute
-- chore that was mid-deploy during the one pass.
--
-- WHY IT RUNS IN THE CLUSTER. Not on a host that may itself be the
-- thing that went silent: boss-gcp's five-minute observer going quiet
-- is one of the two motivating cases, and an alarm hosted beside its
-- subject dies with it. The remaining gap is stated rather than hidden
-- — this sweep still reports THROUGH the jobs API it reads, so it says
-- nothing while the SoR itself is down. That is backlog 6bf34846 and is
-- deliberately out of scope here.
--
-- THE ARGS ARE THE DECLARATION. `interval_minutes.<kind>` = minutes
-- between expected packets of that kind, each the LOOSEST rostered
-- executor for it. CLAUDE.md §9 prefers the Workflow row and it does
-- not work today: boss-platform-workflow-seed is INSERT-IF-MISSING by
-- design (protocols-as-data Q1), so a metadata edit to an
-- already-seeded kind never reaches the registry. The rule row is
-- registry data with the same properties — append-only, versioned,
-- editable through /api/dispatcher/rules without a deploy — so a
-- cadence declaration changes as data, which is the property §9 is
-- about. Mirrored in infra/dispatcher/rules.toml;
-- dispatcher_rules_seed_matches_toml compares the two in both
-- directions, and infra/lint/timers-leave-a-packet.sh check 8 pins this
-- roster against the timer files that state the same intervals.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('cadence-silence-sweep-daily', 1, 'active', NULL, NULL,
   '[{"handler":"cadence.silence.sweep","args":{
      "interval_minutes.maintenance-audit-integrity":"1440",
      "interval_minutes.maintenance-backup":"1440",
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
