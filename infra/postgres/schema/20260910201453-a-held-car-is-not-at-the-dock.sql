-- 20260910201453-a-held-car-is-not-at-the-dock.sql — teach the
-- loading-dock row the brake the conductor has always applied.
--
-- THE MEASUREMENT (backlog 36c3d4ca). "Parked and ready to board" was
-- answered in two places and they disagreed. The conductor's
-- `parked_ready` (boss-cli train.rs) has excluded a car carrying
-- `hold` on its review step since a car repointing the gate rig at a
-- node label was parked green and the only node carrying that label
-- was then cordoned for a hardware fault — landing it would have left
-- the rig unschedulable and stopped gating entirely. The station row
-- never learned. At 18:07:34Z on 2026-09-10, immediately after train
-- 8fa00047 took the two boardable cars, `/api/yard/status` reported
--
--   dock_depth   2
--   threshold_met true
--   "2 car(s) parked now — the dock threshold is met"
--
-- while BOTH remaining cars were held (dd2f4b2b waiting on a kubectl
-- delete, f73828a9 waiting on `boss rerail --finish`) and neither could
-- board. For boarding purposes the dock was empty and the surface said
-- the opposite — and the yard's `held` lane listed neither of them, so
-- a held car rendered as an ordinary dock car, appeared in no held
-- list, and was counted toward a predicate claiming a train would
-- board. The `parked_ready` comment names the second-order cost: the
-- cadence loop shares that predicate for dock DEPTH precisely so a
-- held car does not fire a train it then declines to join, which is
-- what produced the empty windows that made arrival rate unreadable
-- (feedback f4baea39).
--
-- WHY THIS IS A ROW AND NOT A BRANCH IN THE CONDUCTOR. The hold
-- mechanism was added to the compiled copy and never to the registry,
-- which is the leak CLAUDE.md's three-layers reading names: a protocol
-- that cannot be replaced without a deploy has leaked into the
-- substrate. Stations are data-defined by design (§9), so the braking
-- rule belongs in the row — where the yard, the cadence loop's depth
-- probe, `boss orient` and the /queue endpoint all read it, and where
-- an operator can change it without a deploy.
--
-- THE CLAUSE, AND WHY IT IS NOT `metadata_absent`. `metadata_unmarked`
-- is new in `station_queue.rs`, and it reads `stranded::marked`'s
-- rule — the definition the whole system already reads markers by:
-- `null`, `false` and blank are NO marker; `true` is a marker with no
-- reason; a non-blank string is the reason. The Job-level
-- `metadata_absent` clause reads "missing or null", and that would have
-- been a BUG here: a hold is RELEASED by writing `hold: false`, not by
-- deleting the key (car 04520403 was released that way on 2026-09-10,
-- and `boss gate --hold` writes a string). Under missing-or-null a
-- released car would read as held forever and never board — a brake
-- turned into a parking brake with no lever. The clause calls
-- `stranded::marked` rather than carrying its own copy, because a
-- second answer to "is this marker set" is how these two drifted apart
-- in the first place (CLAUDE.md §9a).
--
-- VERSION-AGNOSTIC ON PURPOSE. The live cluster's loading-dock is at
-- version 3, authored through the API; this tree's migrations produce
-- version 2 (116 seeds v1, 133 bumps to v2, 119 and 138 UPDATE the
-- active row in place). Hardcoding "4" would be right for both today
-- and wrong the next time someone publishes through the UI, so the new
-- version is max(version) + 1 and every column is carried forward from
-- whatever row is active — including `upstream` and `lens`, which
-- 119 and 138 set by UPDATE and which are worse absent than broken
-- (119's own comment: the dock lens loses its FEEDBACK link exactly
-- when someone is diagnosing).
--
-- IDEMPOTENT, because migrate.sh is not the only thing that loads this
-- directory: every DB-backed test applies the whole set to a scratch
-- database. Both statements are guarded on the active row NOT already
-- carrying the clause, so a second application is a no-op rather than a
-- fifth version.
--
-- ROLLBACK NOTE. `StepMatch` is `deny_unknown_fields`, so an image
-- older than this clause cannot deserialize the row: `/api/stations`
-- and the /queue endpoint would refuse it and the yard would fall back
-- to its hand-rolled dock predicate. Rolling the image back past this
-- migration therefore needs the row rolled back too (retire the new
-- version, re-activate the prior one). That hazard is inherent to
-- extending the predicate language and is stated here so the next
-- person reads it before the rollback, not during one.

-- RETIRE FIRST, THEN INSERT — `stations_one_active_per_name` (116) is a
-- plain partial unique index, enforced per STATEMENT and not deferred
-- to commit, so inserting an active row while the old one is still
-- active takes the whole schema load down and the error names a
-- constraint rather than this file. That is how 130-watchlist-dismiss
-- and then 133-dock-wip-limit each reddened a train in one night;
-- `infra/lint/registry-bump-retires-first.sh` now checks the order.
-- The retired row is still readable inside the transaction, which is
-- what the INSERT below selects from.
UPDATE stations
   SET status = 'retired'
 WHERE name = 'loading-dock'
   AND status = 'active'
   AND predicate #> '{step,metadata_unmarked}' IS NULL;

INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
                      wip_limit, terminal_window_days, capability, rollup_parent,
                      upstream, lens)
SELECT s.name,
       (SELECT max(version) + 1 FROM stations WHERE name = 'loading-dock'),
       'active',
       s.title,
       s.kind,
       -- `create_missing = true`: the seeded predicate has a `step`
       -- object already (slug review, status ready/active), so this
       -- adds one key beside `slug`/`status_in` and leaves the rest of
       -- the predicate exactly as the active row had it.
       jsonb_set(s.predicate, '{step,metadata_unmarked}', '["hold"]'::jsonb, true),
       s.discipline,
       s.wip_limit,
       s.terminal_window_days,
       s.capability,
       s.rollup_parent,
       s.upstream,
       s.lens
  FROM stations s
 WHERE s.name = 'loading-dock'
   AND s.predicate #> '{step,metadata_unmarked}' IS NULL
   -- Only when the retire above actually ran: if an active row already
   -- carries the clause this migration has been applied, and a second
   -- application must add nothing.
   AND NOT EXISTS (
         SELECT 1 FROM stations
          WHERE name = 'loading-dock' AND status = 'active'
       )
 ORDER BY s.version DESC
 LIMIT 1
ON CONFLICT (name, version) DO NOTHING;
