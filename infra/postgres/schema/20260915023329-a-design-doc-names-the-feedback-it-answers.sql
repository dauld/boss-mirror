-- A design doc names the feedback it answers.
--
-- THE DEFECT (David's bug 4f6019d7, 2026-09-15 00:52Z, on
-- /ux/jobs/61366e5a; backlog 5f0b2661). A feedback routed to `design`
-- gave one person TWO decisions: the design-doc packet's review — three
-- structured questions at /it/design — and the feedback packet's own
-- `design-review` step, an `answer-question` carrying `verdict: ""` and
-- nothing else. The second is empty by construction: the design IS the
-- question, and it lives on the other packet. "There is no question,
-- just a statement … If I was assigned a decision step there is
-- something to decide and the needed context has been provided by
-- upstream job workers." And if the operator decided that step first,
-- the design went unread.
--
-- The two packets were linked by `metadata.design_packet` on the
-- feedback — a free-text string an operator typed, which nothing read.
-- Both live pairs (61366e5a <- c6bd173e, 33324fe9 <- c6f9fb3e) had it,
-- and both feedback steps sat open after David decided both designs.
--
-- WHY AN EDGE, AND WHY ON THE DESIGN. `boss design --answers <packet>`
-- writes `metadata.answers` on the design-doc: the design is the packet
-- that closes, so the design's close is the moment the obligation can
-- fire, and the dispatcher's `jobs.complete_linked_step` follows an
-- edge FROM the closing Job (`link = "answers"`, exactly the shape
-- ship-a-change's `backlog_item` already has on merge). Declared here
-- so the write path REF-CHECKS and prefix-normalises it (104 + 125): a
-- mistyped id is refused at the filing instead of a rule completing
-- nothing, silently, weeks later — the failure mode the prose field
-- had.
--
-- `job_id`, not `job_id_list`: one design decides one packet. A design
-- that settles several is a different relation, filed when it happens.
--
-- on_missing takes the table default, `abort` since 202608291630.
INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('design-doc', 'answers', 'job_id',
   'The user-feedback or backlog-item this design decides — publishing the design completes the design-review step of that packet')
ON CONFLICT (source_kind, field_path) DO NOTHING;
