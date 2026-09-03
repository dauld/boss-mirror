-- 202609031515-board-on-three.sql — the dock boards on three, not
-- twelve.
--
-- WHY. On the morning of 2026-09-03 three gate-proven cars (a design
-- essay, the filer-field admission validator, the credential
-- registry) sat parked for over eight hours while the yard idled
-- under them: min_dock_depth 12 tuned boarding for batch economics,
-- and the wall windows were sparse. David, watching finished work
-- wait for eleven friends it did not need: "there is plenty of work
-- and the train yard sits idle." The operating model is protocol
-- data; this row is that sentence, versioned.
--
-- Three, not one: a single car still waits for company or a window,
-- keeping some batching against CI cost — but three proven cars are
-- a train's worth of value, and the 120-minute cooldown (unchanged)
-- still bounds the CI spend to at most one depth-fired train per two
-- hours.
--
-- Retire-and-reseed, matching 123/131: a threshold change is a NEW
-- version of the rule, never an edit to the row that was in force —
-- "what was the threshold when this train boarded?" stays answerable
-- against cadence_firings forever.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND version = 3
   AND status <> 'retired';

INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
VALUES
    ('train-board-on-dock-depth', 4, 'active', 'board', 'queue-depth', NULL, NULL, 3, 120)
ON CONFLICT (name, version) DO NOTHING;
