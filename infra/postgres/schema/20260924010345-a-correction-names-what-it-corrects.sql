-- 20260924010345-a-correction-names-what-it-corrects.sql — the
-- corrections door's event is declared.
--
-- Origin: design 4105b020, answering backlog 56727f95. A completed
-- step is a fact and the step API rightly refuses to rewrite it; what
-- was missing was a TIE between it and the correction written beside
-- it. Measured 2026-09-23: about 132 corrections sat in job metadata
-- under 25 key names each author invented, and nothing read any of
-- them. The door is POST /api/jobs/{id}/steps/{step_id}/corrections
-- (`boss correct`): it appends one entry to the job's reserved
-- `corrections` list and records this event beside the JOB_UPDATED
-- row state, in the same transaction.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('jobs.step.corrected', 'jobs', 'A correction was appended beside a completed or skipped step (POST /api/jobs/{id}/steps/{step_id}/corrections, boss correct): the step and its events untouched, one entry added to the job''s append-only corrections list. Payload {job_id, step_id, index, correction: {step, field, reads, should_read, why, by, at} or {step, field, withdraws, why, by, at}} with the actor as _actor. Informational: the job''s row state rides the sibling jobs.job.updated, which the rebuild replays', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
