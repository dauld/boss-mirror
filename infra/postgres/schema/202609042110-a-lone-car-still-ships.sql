-- 202609042110-a-lone-car-still-ships.sql — boarding becomes
-- latency-bounded instead of quorum-bounded.
--
-- THE MEASURED PROBLEM. Boarding required THREE parked cars
-- (min_dock_depth 3), or one of two daily clock windows (06:05 /
-- 18:05 UTC). With this pipeline's real volume — a handful of cars a
-- day — that means a car whose neighbours have not arrived waits for
-- a window rather than for a train. On 2026-09-04 at 21:03 UTC two
-- green cars sat parked with the dock below threshold and the next
-- window nine hours away; earlier the same day a full dock waited two
-- hours behind a cooldown. Batching stopped being an optimisation and
-- became the thing that held delivery.
--
-- THE CHANGE. min_dock_depth 3 -> 1, cooldown 120 -> 45. Read it as
-- "board whatever is waiting, at most every 45 minutes":
--
--   - A LONE CAR SHIPS. Delivery latency is bounded by the cooldown
--     (<= 45 min) rather than by how many other people happened to
--     finish work today. That is the property we actually want; a car
--     that is green and parked is finished work sitting still.
--   - BATCHING STILL HAPPENS, opportunistically: everything parked
--     when the window opens rides together, which is what batching is
--     for. What is gone is the REQUIREMENT for company.
--   - THE COOLDOWN IS NOW THE REAL CONTROL. It bounds train frequency
--     (<= ~1.3/hour), which keeps CI serial-ish on one build host and
--     keeps the yard readable. It is also no longer a trap: since
--     fix/a-failed-board-does-not-hold-the-window (train #199) a
--     firing that failed does not consume its window, so a bad board
--     costs one tick rather than 45 minutes.
--
-- WHY NOT JUST ADD CLOCK WINDOWS. More at_times would shorten the
-- worst case but keeps the same shape — a car waits for a clock
-- rather than for readiness. Depth 1 + cooldown expresses the
-- intent directly: ship what is ready, do not stampede.
--
-- train-window (06:05 / 18:05) is deliberately LEFT IN PLACE as a
-- backstop: if the depth rule is ever wedged, the clock still boards.
--
-- IDIOM: retire by NAME, insert MAX(version)+1 — established by
-- 202609032030-cadence-supersede-by-name, which documents why
-- version-keyed supersede silently no-ops once history has diverged.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND status = 'active';

INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
SELECT 'train-board-on-dock-depth',
       COALESCE(MAX(version), 0) + 1,
       'active', 'board', 'queue-depth', NULL, NULL, 1, 45
  FROM cadence_rules
 WHERE name = 'train-board-on-dock-depth';
