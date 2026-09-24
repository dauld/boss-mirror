-- 20260924113047-a-re-pin-is-on-the-record.sql — the re-pin door's
-- event is declared.
--
-- Origin: design 7cf202a9 (David 2026-09-23, all five questions
-- accepted as proposed), answering backlog 4347a1af. A packet stays on
-- its admission version unless an actor explicitly moves it, and the
-- move is on the record. Until this car the door (POST
-- /api/jobs/{id}/convert) changed jobs.workflow_version under a plain
-- jobs.job.updated, so a moved packet could not be told from one
-- admitted at that version without diffing successive payloads, and
-- its steps still read the admission version's text (backlog
-- 1e973965). The door now re-projects every unfinished step, materialises
-- every inserted one, appends to the job's reserved `repins` list, and
-- records this event beside the row state, in the same transaction.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.job.repinned', 'jobs', 'A packet was moved to another version of its protocol through the re-pin door (POST /api/jobs/{id}/convert, boss job convert): its unfinished steps re-projected from the target, the steps the target inserts materialised, completed steps left with the text they ran under, and one entry appended to the job''s append-only repins list. Payload {job_id, from, to, by, at, reprojected: [{step, step_id, changed, kept?}], inserted: [{step, step_id}]} with the actor as _actor. Informational: the rows ride the sibling jobs.job.updated / jobs.step.updated / jobs.step.created, which the rebuild replays', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
