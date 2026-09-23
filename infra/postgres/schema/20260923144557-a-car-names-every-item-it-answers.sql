-- A car can name EVERY item it answers, and each closes on merge.
--
-- WHY A LIST BESIDE `backlog_item` (backlog a994f533).
-- `ship-a-change.backlog_item` is one id, and the arrival rule
-- `complete-feedback-branch-on-car-merged` follows exactly that id to
-- route the item's triage and complete its `build`. A change that
-- answers TWO items could close only one; the other stayed open as
-- landed-but-unclosed residue, and `boss dispatch` handed it out.
--
-- MEASURED, 2026-09-23, twice. 5994de6d (owed-readings copy) was
-- dispatched as run 8a96f985 though its fix had landed in trains
-- #572/#574 on cars filed under d4698bc2, 37fc5837 and 228faa58.
-- cab50f4c (cadence lint vs pin) was dispatched as run eb266e76 though
-- its fix had landed in #527 on car c842f18b, which named only
-- 3ec04168. Both builders refused correctly and the operator closed
-- each by hand — a builder run and an operator's re-verification spent
-- on each, for a fact the car could have carried.
--
-- `job_id_list`: the write path's trigger (104 + 125) ref-checks and
-- normalises EACH element to the full Job id, so a listed item cannot
-- be a typo or an 8-char prefix the arrival rule cannot follow. A
-- non-array value is skipped by the trigger, so the handler reads a
-- non-array as no items and says so.
--
-- The rule reads it through its `also_link` arg (v5 of the rule file);
-- on_missing takes the table default, `abort` since 202608291630.
INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('ship-a-change', 'also_answers', 'job_id_list',
   'Every other backlog/feedback Job this change answers — each closes on merge, as backlog_item does')
ON CONFLICT (source_kind, field_path) DO NOTHING;
