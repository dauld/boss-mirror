-- 202609032030-cadence-supersede-by-name.sql — the boarding rule
-- actually becomes depth 3, and the retire-and-reseed idiom stops
-- being a silent trap.
--
-- WHAT WENT WRONG. 202609031515-board-on-three tried to make the
-- boarding threshold 3 by retiring version 3 and inserting version 4.
-- It did not take: the live /api/cadence/rules still served
-- min_dock_depth 4 hours after that migration deployed (measured
-- 2026-09-03 20:xx via the sanctioned API). The change was reported
-- as landed and was, in effect, a no-op — visible only because an
-- operator happened to query the live surface while investigating
-- something else.
--
-- WHY THE IDIOM IS A TRAP. `cadence_rules` carries a partial unique
-- index: at most one active row per name (proved by the
-- `one_active_row_per_rule_name` test). The retire-and-reseed idiom
-- used by 123 / 131 / board-on-three retires a SPECIFIC VERSION
-- (`WHERE version = N`) and inserts the next. But the cluster's
-- version history diverged from the migrations' assumptions long ago
-- (measured and documented in 123-cadence-registry-reconcile: the
-- runtime read a different cadence_rules than the record). So
-- `retire WHERE version = 3` retires a row that is not the active one,
-- the active row survives, and the new insert is either refused by the
-- partial index or — with an `ON CONFLICT (name, version)` clause that
-- does NOT cover the partial-active index — skipped. Version-keyed
-- supersede cannot be trusted once history has diverged even once.
--
-- THE SAFE IDIOM, established here for every future cadence supersede:
-- retire whatever is active BY NAME, then insert the next version
-- computed as MAX(version)+1. This is correct from ANY prior state —
-- divergent history, a stray active row, or clean — and can never be
-- silently skipped, because it never names a version it must guess.
UPDATE cadence_rules
   SET status = 'retired'
 WHERE name = 'train-board-on-dock-depth'
   AND status = 'active';

INSERT INTO cadence_rules
    (name, version, status, verb, basis, every_minutes, at_times, min_dock_depth, cooldown_minutes)
SELECT 'train-board-on-dock-depth',
       COALESCE(MAX(version), 0) + 1,
       'active', 'board', 'queue-depth', NULL, NULL, 3, 120
  FROM cadence_rules
 WHERE name = 'train-board-on-dock-depth';
