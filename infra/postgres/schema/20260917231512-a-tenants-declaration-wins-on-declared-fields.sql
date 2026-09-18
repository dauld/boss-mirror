-- 20260917231512-a-tenants-declaration-wins-on-declared-fields.sql —
-- the agents batch records the row it CHANGED.
--
-- Origin: backlog 09887242. Measured 2026-09-17 on prod: the tenant's
-- employees.json declared emp-david at loc-algedonic-hq and the roster
-- read loc-hq — the FIRST publish's value, kept through every publish
-- since because POST /api/people answered 409 and the seed counted
-- that as "already there". agent-claude had the same shape: the
-- migration's display name, kept by an insert-if-absent batch that
-- NAMED the differing field and applied nothing. The decision, once,
-- for employees and agents: the tenant's declaration wins on the
-- fields it declares; what it does not declare is kept; nothing it
-- does not declare is deleted. The people door already records
-- `people.employee.updated` on its PUT; this is the agents batch's
-- fact for the same act — the row as it reads after, the `changes`
-- (field, from, to) the declaration made, and `updated_by`, the actor
-- the request signed with. One per row changed, none for a row
-- already as declared, none per batch.
--
-- Declared here so the kind does not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('agent.updated', 'jobs', 'A tenant''s batch changed one agent row the registry already held (POST /api/agents/batch, the declaration wins on declared fields): the row as it reads after, the changes (field, from, to) the declaration made, and updated_by, the actor the request signed with. One per row changed, none for a row already as declared, none per batch', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
