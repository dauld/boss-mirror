-- A new job edge aborts like every other one.
--
-- SHIPPED BROKEN EARLIER TODAY, in my own car. Migration
-- 202608291430 added the `design-doc.translated_from` edge with the
-- four-column INSERT that 104 uses, and its comment asserted the
-- safety property that made the edge worth having:
--
--   "on_missing defaults to `abort` (105), which is what makes this
--    worth doing as an edge at all: the write path REF-CHECKS the
--    value, so a chain cannot be built out of ids that do not resolve."
--
-- That is false, and the live registry says so:
--
--   *              waiting_on       abort
--   design-doc     translated_from  WARN     <-- the new one
--   pr-train       boarded_jobs     abort
--   ship-a-change  backlog_item     abort
--   ship-a-change  train            abort
--
-- WHY. The column DEFAULT is 'warn' (104:26). Migration 105 did not
-- change the default — it ran a ONE-TIME `UPDATE ... WHERE
-- (source_kind, field_path) IN (...)` naming the three rows that
-- existed then. So "edges abort" was never a property of the table; it
-- was a property of three rows, restated in prose everywhere since. Any
-- edge added afterwards silently gets the weaker dial, and the
-- in-memory defaults (`InMemoryJobEdges`, which hardcodes
-- on_missing: "abort" for every row) drift from the database the moment
-- one is.
--
-- TWO FIXES, because the row is the symptom and the default is the
-- cause. Flipping the default is what stops the next edge repeating
-- this; without it a sixth edge added next month lands as `warn` again
-- and the same prose keeps claiming otherwise.
--
-- Rollback stays the one-row UPDATE back to 'warn' that 105 describes.
ALTER TABLE job_edges ALTER COLUMN on_missing SET DEFAULT 'abort';

UPDATE job_edges SET on_missing = 'abort'
 WHERE (source_kind, field_path) = ('design-doc', 'translated_from');
