-- 20260915023614-a-machine-owned-proof-is-not-a-persons-task.sql — the
-- operator's task station stops listing a proof the machine owns.
--
-- THE MEASUREMENT (backlog b725e860). David, 2026-09-15: "I checked my
-- queue recently, and many of the jobs were buggy." `GET
-- /api/stations/q.platform-admin.task/queue` held 27 packets, eight of
-- them ship-a-change cars whose only open step was `proven`. Four of
-- those carried a `proof_probe` recorded at park time, every one with a
-- `proof_attempt.exit` of 75 (not yet): the forge ran the probe on
-- arrival (`run-car-probes-on-train-arrived`) and the daily recheck
-- re-runs it. No person can complete that step sooner than the machine
-- will; it is a wait, dressed as a task. The other four recorded a
-- `proof_event` instead — a car only an event can prove — and those ARE
-- a person's, so they stay (jobs_run_car_probes.rs: an event-bound car
-- "stays hand-proven"). A queue padded with machine-owned waits trains
-- the operator to skim it, which is the failure the queue exists to
-- prevent.
--
-- WHY A ROW AND NOT A BRANCH. `q.platform-admin.task` was never
-- authored: it is the projection of every active protocol's `(task,
-- platform-admin)` constraint (station_projection.rs), and the ship-a-
-- change `proven` step declares exactly that authority so a person CAN
-- prove a car by hand. That authority is unchanged here. What changes is
-- membership, and membership is the station row's business — the
-- projection yields to an authored row of the same name by design
-- ("authored wins"; a derived station never silently overwrites one).
-- The predicate vocabulary already says "Job metadata key missing or
-- null" (`metadata_absent`), and `proof_probe` is written once at park
-- and never released, so missing-or-null is the right reading — unlike
-- the dock's `hold`, which is released by writing `false` and needed
-- `metadata_unmarked` (20260910201453).
--
-- THE ROW IS THE PROJECTION'S PREDICATE PLUS ONE EXCLUSION. Step clause
-- `kind = task`, `metadata_equals.authority_role = platform-admin`,
-- `status_in = [ready, active]`; capability `roles = [platform-admin]`;
-- default discipline; no wip_limit — field for field what
-- `derived_stations` would have written, so `station_lint`'s
-- derived-namespace check (an authored `q.` name must serve the
-- constraint it shadows) passes, and steps for that pair still reach
-- this queue. Pinned by a_machine_owned_proof_is_not_a_persons_task_pg.
--
-- WHAT ELSE THE EXCLUSION TOUCHES, stated rather than discovered:
--   * A PARKED car with a probe (review ready, not yet merged) also
--     leaves this queue. Its station is the loading dock and the
--     conductor completes `review` at merge; a person never did. A car
--     parked with no probe stays listed at `review` as before.
--   * `/api/stations/flow` reports this station BLIND ("membership
--     turns on Job metadata"): the flow cube is keyed (job kind, step
--     kind, slug, authority role) and cannot subtract the excluded
--     steps, so the marshalling yard shows depth and "rate not counted"
--     for it rather than a figure it did not measure — the honest
--     outcome station_flow.rs documents for any metadata clause, and
--     the one cost of taking the data route. Measured 2026-09-15 before
--     this row: 13806 arrived / 13268 served over 168h, the busiest
--     station on the board by two orders of magnitude.
--   * The yard's inspection shed reads `/api/jobs?kind=ship-a-change`
--     and the dock queues, never this station; it still shows every
--     landed, unproven car (yard-shed.ts).
--
-- IDEMPOTENT, because migrate.sh is not the only thing that loads this
-- directory: every DB-backed test applies the whole set to a scratch
-- database. Guarded on no active row of this name already existing, so
-- an operator who has since re-authored the station through the API
-- keeps their version and this seed adds nothing.
INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
                      wip_limit, terminal_window_days, capability, rollup_parent)
SELECT 'q.platform-admin.task', 1, 'active',
       'platform-admin — task',
       'constraint',
       '{"status": "open",
         "metadata_absent": ["proof_probe"],
         "step": {"kind": "task",
                  "status_in": ["ready", "active"],
                  "metadata_equals": {"authority_role": "platform-admin"}}}'::jsonb,
       '["priority", "age"]'::jsonb,
       NULL, NULL,
       '{"roles": ["platform-admin"]}'::jsonb,
       NULL
 WHERE NOT EXISTS (
         SELECT 1 FROM stations
          WHERE name = 'q.platform-admin.task' AND status = 'active'
       )
ON CONFLICT (name, version) DO NOTHING;
