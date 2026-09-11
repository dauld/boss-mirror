-- 202609101200-the-flush-pipeline-is-deleted.sql — execute the deletion
-- settled in review 87f5bc84 on 2026-08-29 and recorded in
-- docs/architecture-decisions.md, "Design docs and the decision record"
-- (backlog f5da586c).
--
-- THE DECISION. **The packet is the doc.** The packet is authored and
-- the file, where one exists, is a generated artefact — so the
-- write-BACK half has nothing left to do: the flush pipeline, the
-- pending-decision staging table it drained, and the two reports
-- ("rejections", "stale-statuses") that only made sense while a
-- hand-written `**Status**:` line could drift from the tracker. With
-- them goes the concept of a "drifted" doc, since a packet's status IS
-- its status.
--
-- THE DECISION WAS SETTLED ON EVIDENCE, and the evidence held when
-- re-measured on 2026-09-10 against the live corpus: 58 flush jobs, of
-- which 30 FAILED and 6 were still QUEUED — 80 recorded answers across
-- 18 docs that never reached a file. The pipeline was run by hand, and
-- the answers it was copying were already on the packets.
--
-- NO HISTORY IS LOST. audit_log is the system of record; these are
-- projection and worklist tables. Every `docs.design.decision_recorded`
-- and `docs.design.indexed` event stays in the log, and the
-- `docs.design.decision_recorded` row in `event_kinds` is deliberately
-- LEFT IN PLACE: the kind was emitted, those rows exist, and deleting
-- the declaration would make boss-audit-integrity-check warn about
-- perfectly good history.
--
-- ======================================================================
-- 1. The pending-decision table becomes the closed recorded-decision
--    ledger, and it is renamed to say so.
-- ======================================================================
--
-- WHY IT IS NOT SIMPLY DROPPED, which the decision's own bullet list
-- implies. `upsert_doc` — the READ half, which stays — reads these rows
-- on every reindex and force-resolves their anchors "whatever the file
-- says" (boss-docs `apply_recorded_decisions`, added 2026-08-18 for
-- David's "I keep seeing jobs that I have responded to"). Reindex
-- delete-and-reinserts the whole question set from the parse, so an
-- answer known only outside the file is erased on every boot; these
-- rows are what put it back. Measured 2026-09-10: dropping both tables
-- would re-open 25 questions across 6 docs (break-glass-is-a-key-you-
-- hold, department-flow-dashboards, framing-convergence,
-- networks-meet-at-boundaries, the-build-plane-manages-itself,
-- the-cluster-is-the-system), and rule 107 plus the daily level sweep
-- would hand David six fresh review packets for questions he has
-- already answered. Neither the 2026-08-29 decision nor the backlog item
-- accounted for that read; this is it, accounted for.
--
-- It is CLOSED: nothing writes a row after this migration, and with the
-- flush gone nothing will ever write `(resolved)` into a heading either,
-- so the ledger is the only thing holding those questions shut. The
-- name stops lying — a row here is not pending anything.
ALTER TABLE IF EXISTS design_pending_decisions
    RENAME TO design_recorded_decisions;

ALTER INDEX IF EXISTS design_pending_decisions_doc
    RENAME TO design_recorded_decisions_doc;

COMMENT ON TABLE design_recorded_decisions IS
  'Closed ledger of every answer recorded against a design-doc question '
  'anchor. READ-ONLY since 2026-09-10: the flush pipeline that wrote it '
  'was deleted, and boss-docs reads it on reindex so an answered '
  'question does not re-open and re-spawn a review. New answers live on '
  'the design-doc packet, which is the doc.';

-- Recover the answers the flush pipeline had already swallowed. A
-- `create_flush_job` MOVED the pending rows into the job's immutable
-- payload and deleted them, so 80 answers sitting in queued and failed
-- payloads exist nowhere else once that table goes. `succeeded` jobs are
-- excluded: those did write the file, so the heading carries
-- `(resolved)` on its own and the parse already reports them resolved.
--
-- `->> 'anchor'` is nullable (a malformed payload entry yields NULL),
-- hence the WHERE. Idempotent on (doc_path, anchor) — the UNIQUE
-- constraint carried over from the old table decides ties in favour of
-- the row that was still pending, which is the later answer.
INSERT INTO design_recorded_decisions
       (id, doc_path, anchor, kind, resolution, rationale, decided_by, decided_at)
SELECT DISTINCT ON (j.doc_path, d ->> 'anchor')
       'fj-' || j.id || '#' || (d ->> 'anchor'),
       j.doc_path,
       d ->> 'anchor',
       CASE WHEN d ->> 'kind' = 'accept' THEN 'accept' ELSE 'override' END,
       coalesce(d ->> 'resolution', ''),
       d ->> 'rationale',
       j.requested_by,
       j.queued_at
  FROM design_flush_jobs j,
       LATERAL jsonb_array_elements(j.payload -> 'decisions') AS d
 WHERE j.status <> 'succeeded'
   AND d ->> 'anchor' IS NOT NULL
 ORDER BY j.doc_path, d ->> 'anchor', j.queued_at DESC
ON CONFLICT (doc_path, anchor) DO NOTHING;

-- ======================================================================
-- 2. The flush pipeline's own table. Its indexes and FK go with it.
-- ======================================================================
--
-- This is the table a service ran `git commit` and `git push` against,
-- and the one 202609090025 added `worked_by` to the night before the
-- decision residue was spotted — the column goes with the table, which
-- is what that packet said should happen.
DROP TABLE IF EXISTS design_flush_jobs;

-- ======================================================================
-- 3. `design_docs.pending_count` — a count of a table that no longer
--    has writers.
-- ======================================================================
--
-- Only the pending-decision endpoints ever moved it; reindex preserved
-- it and never computed it. With those endpoints gone it could only
-- ever read 0, and a "Pending decisions" column that is structurally
-- always 0 is the lying-record defect this whole item was filed about.
-- The live tracker already reported 1 across 57 docs.
ALTER TABLE design_docs DROP COLUMN IF EXISTS pending_count;
DROP INDEX IF EXISTS design_docs_pending;

-- ======================================================================
-- 4. Retire the two dispatcher rules that existed only for this half.
-- ======================================================================
--
-- `design-decision-flush-queue` (109) turned a recorded decision into a
-- queued flush. There is no flush to queue.
--
-- `maintenance-sweep-doc-status-daily` (145, v2 by 202608311700) filed a
-- daily sweep whose entire content was the drifted-doc report: its own
-- comment says "the detector already exists: GET
-- /api/design/stale-statuses". That route is deleted, and the concept
-- with it — so the sweep is retired rather than left firing at a
-- detector that answers 404. Both versions, because the partial unique
-- index is on the ACTIVE one and the loader reads status='active'.
--
-- Retired rather than deleted: dispatcher_rules is an append-only
-- registry and a retired row is the record that the rule existed.
UPDATE dispatcher_rules SET status = 'retired'
 WHERE name IN ('design-decision-flush-queue', 'maintenance-sweep-doc-status-daily');
