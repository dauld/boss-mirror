-- 20260918022311-an-agent-holds-a-role-and-sits-in-a-department.sql —
-- an agent's row says what role it holds and where it sits in the
-- org, the two columns an employee's row already has.
--
-- Origin: backlog ab192a9f (2026-09-17). The real tenant's first
-- agents.toml declared agent-claude with role = engineering-agent and
-- department = engineering; the registry had no such columns, so the
-- tenant contract refused them by name (f56155f0) and the tenant
-- dropped them. Measured the same day: Audience::Role resolves to
-- HOLDERS OF THE ROLE, and every reader that enumerates holders read
-- the employees roster only — so a step whose audience is a role could
-- reach an agent only by naming it as an individual. An agent is an
-- executor like a person (CLAUDE.md: humans and agents are CPUs in the
-- same machine); where it sits and what it holds decide what a role
-- audience resolves to, exactly as for a person.
--
-- Both nullable: a registered agent that holds no role is still an
-- actor (it is reachable by id), and NULL is the honest value until
-- the tenant declares one. Both are Class codes under
-- (subject_kind = 'employee', member_attribute = 'role' / 'department')
-- — the same registry rows employees are validated against, checked
-- at the batch door (POST /api/agents/batch) with the same client
-- boss-people uses, so an undeclared code is refused by name there
-- rather than a CHECK here: the Class registry is data, and a
-- constraint that copied it would drift from it (CLAUDE.md §9a).

ALTER TABLE agents
    ADD COLUMN IF NOT EXISTS role        TEXT,
    ADD COLUMN IF NOT EXISTS department  TEXT;

COMMENT ON COLUMN agents.role IS
  'The role this agent holds — a Class code under (employee, role), the '
  'same registry an employee''s role names. A step whose audience is '
  '{ role = X } resolves to every holder of X, agents included '
  '(backlog ab192a9f). NULL until the tenant declares one.';

COMMENT ON COLUMN agents.department IS
  'Where this agent sits in the org — a Class code under (employee, '
  'department). Carried and validated like an employee''s; nothing '
  'routes on a department until design f5ebd2e1 car 2 makes it a '
  'selector. NULL until the tenant declares one.';
