-- A packet relation is declared, not improvised (design c0d2787a).
--
-- WHY. `job_edges` has always been the right model — which field on
-- which kind points at another Job — but every edge declared so far
-- carries BEHAVIOUR: `answers` completes a design-review on publish,
-- `waiting_on` is cleared by the dispatcher when the blocker closes,
-- `backlog_item` is what the arrival rule routes on. There was no way
-- to state the plain fact that two packets are related, so authors
-- reached for freeform metadata and the fact became unreadable.
--
-- MEASURED, in one session on 2026-09-21: six packets filed, and the
-- relationships between them recorded four different ways —
-- `prerequisite_for`, `prerequisite`, `prerequisite_of`, `related`.
-- None declared here, so none resolved, none rendered in a Links
-- panel, none ref-checked, none queryable. Meanwhile the one STRONG
-- spelling, the declared `answers` edge, was REFUSED by its verb
-- because the target was disposition=build. The system permitted the
-- weak unqueryable spelling of the fact and forbade the strong one.
--
-- THE WORST OF IT was not untidiness. Two of those `prerequisite`
-- keys meant "this packet waits on that one" — which is `waiting_on`,
-- declared since migration 110, and cleared by the dispatcher when
-- the blocker closes. Writing prose instead of the edge meant the
-- waiters would never have been woken. The mechanism existed and was
-- walked past.
--
-- SO: THREE RELATIONS, NOT FOUR. The design proposed a prerequisite
-- relation alongside these; it is deliberately absent, because
-- `waiting_on` already is one and a second spelling of a live edge is
-- the very disease being cured. What is missing is only the
-- behaviourless kind:
--
--   occasioned_by  this packet exists because of that one
--   duplicate_of   this packet restates one already filed
--   supersedes     this packet replaces one that is now wrong
--
-- '*' because a relationship is not a property of a kind (the
-- wildcard the waiting_on migration taught the guard, 110).
-- `on_missing` takes the table default, which is `abort` since
-- 202608291630 — a relation naming a packet that does not resolve is
-- a claim about nothing, and that is worth refusing even for an edge
-- that causes nothing else.
INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('*', 'occasioned_by', 'job_id',
   'The packet whose work brought this one into being — provenance only; it gates nothing'),
  ('*', 'duplicate_of',  'job_id',
   'The packet this one restates; the duplicate is the one that closes'),
  ('*', 'supersedes',    'job_id',
   'The packet this one replaces, whose conclusion no longer holds')
ON CONFLICT (source_kind, field_path) DO NOTHING;
