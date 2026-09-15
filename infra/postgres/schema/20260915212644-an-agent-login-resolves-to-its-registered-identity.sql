-- 20260915212644-an-agent-login-resolves-to-its-registered-identity.sql
-- — an agent is a registered actor with ONE id, and the address it
-- logs in with is an alias of that id, not a second actor.
--
-- Origin: backlog adf025df, decided as design 6fda05ae (2026-09-15).
-- Measured 2026-09-11 on the system of record: the one agent that does
-- most of the IT department's work had two spellings and no relation
-- between them — `claude@algedonic.dev` on 142 open-packet step
-- assignments and 12 completions (steps.assignee_id / completed_by),
-- and `claude:opus-5[1m]` on every agent_runs row. So "what did this
-- CPU build, and what did it cost" — the question agent_runs was built
-- to answer — could not be asked: provenance, the first property of
-- the correctness protocol, was missing at the actor.
--
-- A human never reaches a step as an address: boss-gateway's oidc.rs
-- resolves a login to an `emp-*` id through the People domain before
-- any write is signed, failing closed when no employee matches. An
-- agent had no such resolution because there was nothing to resolve
-- against. These two tables are that thing.
--
-- THE THREE RESOLUTIONS, as David accepted them:
--   id-shape          an emp-style opaque id, `agent-<slug>`; NOT the
--                     login address and NOT the model-qualified colon
--                     form. Addresses are logins; models are facts
--                     about a run.
--   model-on-run      the agent row carries a DEFAULT model and caps;
--                     the model a run actually used belongs on the run
--                     (a later car adds agent_runs.model — this one
--                     does not touch that table).
--   unregistered      refused, failing closed, exactly as oidc.rs —
--                     AFTER a migration window of one release in which
--                     an unmatched address is still admitted and
--                     COUNTED. This migration opens the window; the
--                     `actor.login.unresolved` kind below is the count.
--
-- WHY AN ALIAS TABLE AND NOT A REWRITE. The audit log cannot be
-- rewritten (it is the system of record), so the 154 live rows and
-- every historical `_actor` stay spelled as they were. The alias
-- relation is what makes them readable under the one identity. Live
-- assignee_id / completed_by re-pointing is a separate car.

CREATE TABLE IF NOT EXISTS agents (
    -- The canonical actor id: what a step's assignee_id, completed_by
    -- and an event's _actor carry once the login has resolved. The
    -- CHECK is the SQL spelling of boss_core::actor::REGISTERED_AGENT_
    -- PREFIX — the parse that turns this id into ActorId::RegisteredAgent
    -- reads the same prefix, so an id this table admits is one the
    -- actor model classifies as a machine.
    id                          TEXT PRIMARY KEY CHECK (id ~ '^agent-[a-z0-9][a-z0-9-]*$'),
    display_name                TEXT NOT NULL,
    -- The model this agent runs when a run does not say otherwise.
    -- Spelled as agent_rate_card.model spells it (`opus-5[1m]`, no
    -- `claude-` prefix) and pinned to that table by the FK, so a
    -- default that cannot be priced cannot be registered — the rate
    -- card's own header refuses the prefixed spelling for the same
    -- reason.
    default_model               TEXT NOT NULL REFERENCES agent_rate_card (model),
    -- The two caps boss_core::agent::AgentSpec already types. NULL is
    -- "no cap declared", which is the honest value while nothing reads
    -- them (budgets are backlog 7dd9f28c, not this car): a number here
    -- that nothing enforced would look like a measurement.
    hourly_budget_usd_micros    BIGINT CHECK (hourly_budget_usd_micros >= 0),
    max_concurrent_runs         INTEGER CHECK (max_concurrent_runs >= 0),
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE agents IS
  'The registered agents — the machine half of the actor roster the '
  'employees table is the human half of. An agent''s login (an address) '
  'resolves to a row here at the jobs API''s door the way a human''s '
  'login resolves to an employee at the gateway (design 6fda05ae). Rows '
  'arrive by migration for now, the rate-card precedent: a wrong '
  'identity is harder to notice than a missing one.';

CREATE TABLE IF NOT EXISTS actor_aliases (
    -- What arrives in X-Boss-User.id: the address a session signs with.
    alias       TEXT PRIMARY KEY,
    -- The one identity it resolves to.
    actor_id    TEXT NOT NULL REFERENCES agents (id),
    -- From when this alias has named that actor. Historical rows before
    -- this instant carry the alias itself as their actor; a reader
    -- joining them to the agent does so through this table.
    since       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE actor_aliases IS
  'A login (alias) -> the registered actor it signs as. Resolved at the '
  'jobs API''s door before any write is stamped, so no step or event '
  'carries the alias from the resolution instant on; rows written '
  'before `since` still carry it, and this table is how they are read '
  'under the one identity — the audit log is not rewritten.';

-- The one live agent and the one live address, measured 2026-09-11.
-- `opus-5[1m]` is the rate-card row the dev pod's own sessions run as
-- (see 20260910030644).
INSERT INTO agents (id, display_name, default_model) VALUES
  ('agent-claude', 'Claude (Claude Code sessions on the dev pod)', 'opus-5[1m]')
ON CONFLICT (id) DO NOTHING;

INSERT INTO actor_aliases (alias, actor_id) VALUES
  ('claude@algedonic.dev', 'agent-claude')
ON CONFLICT (alias) DO NOTHING;

-- The window's count. During the migration window a write signed with
-- an address no alias maps is still admitted (as every write was
-- before this migration) and this event records that it was, naming
-- the login, the method and the path. The count of these since landing
-- is what decides when the window can close: zero means the refusal
-- (the next car) will refuse nothing that is live. The emitted kind is
-- declared here so it does not ride inside a passing audit-integrity
-- run unread (infra/lint/emitted-kinds-are-declared.sh).
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('actor.login.unresolved', 'jobs', 'A write arrived signed with an address-shaped login that matches no actor alias; admitted during the one-release migration window of design 6fda05ae and counted here so the window''s close refuses nothing live', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
