-- A design doc can name what it revises.
--
-- DECIDED by David in review 87f5bc84 (Q5, 2026-08-29): "a new packet
-- with a `translated_from` edge, matching the answer already recorded
-- for packet translation (fixed protocol set at creation; a translation
-- is a new packet through admission, never a mutation). It keeps the
-- envelope immutable and makes the doc's life legible as a chain."
--
-- WHY AN EDGE AND NOT A COLUMN. A design doc has a life — drafted,
-- reviewed, revised, superseded — and under immutable packets a
-- revision cannot be an update. It is a new packet that points at the
-- one it supersedes, and "the doc" is then the CHAIN rather than a row.
-- The fold reads the chain to assemble current truth, and the chain IS
-- the decision history, with an actor and a timestamp on every answer,
-- which the markdown Decision-history sections only ever approximated.
--
-- `job_id`, not `job_id_list`: a revision revises exactly one packet.
-- Two docs merging into one is a different relation and should be
-- filed as one when it is actually needed, rather than guessed at now.
--
-- on_missing defaults to `abort` (105), which is what makes this worth
-- doing as an edge at all: the write path REF-CHECKS the value, so a
-- chain cannot be built out of ids that do not resolve. That is the
-- property the markdown "supersedes: some-other-doc.md" lines never
-- had — they were prose, and three of them named files that had already
-- been deleted.
INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('design-doc', 'translated_from', 'job_id',
   'The design-doc packet this one revises — the previous link in the chain')
ON CONFLICT (source_kind, field_path) DO NOTHING;
