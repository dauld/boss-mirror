-- 20260916020249-the-operator-surfaces-are-counted.sql — which surfaces
-- an operator opens is a fact the system holds, one row per open.
--
-- Origin: backlog 628f182b. David, 2026-09-16, running the company as
-- one human plus agents: "I am mostly concerned about my personal HCI
-- … the UI and good transparency into underlying state has been really
-- important for driving reliability and trust." Then: "let's measure
-- which surfaces I open for a week." Measured 2026-09-16 01:50Z:
-- nothing records a route open per actor. The SPA routes client-side
-- (only hard loads reach the gateway as document navigations), the
-- audit log carries no per-request events BY DECISION
-- (docs/architecture-decisions.md §Policy & auth), and boss-observability
-- aggregates service health, not page opens. So "which surfaces do I
-- actually open in a day" had no read at all.
--
-- NOT AN AUDIT EVENT, and not a projection of one. A page open is not
-- a state change of the company; it is a measurement of the operator's
-- attention, and the decision above keeps that class out of the log.
-- This table is its own record — the `cadence_firings` idiom, not the
-- `agent_runs` one — so there is no rebuilder and no outbox row.
--
-- THE ROUTE IS A PATTERN, NEVER A CONCRETE ID. `/ux/jobs/:jobId`, not
-- the uuid: the question is which SURFACES are opened, and a per-id
-- row would count a busy day on one packet as a hundred surfaces.
-- The pattern is minted client-side off the parsed route
-- (apps/web/src/shell/surface-opens.ts) and the daily chore compares
-- the opened patterns against the nav catalog's paths to name the
-- surfaces NOBODY opened — the deletion candidates.
--
-- THE ACTOR IS THE SESSION'S. The jobs API reads it off the
-- `x-boss-user` header the gateway signs from the cookie; the body
-- carries route and time and nothing else, and a machine-shaped actor
-- (an automation, an agent login) is refused at the door. Agents'
-- automated reads never reach here by construction — they do not run
-- the SPA — so the count is an operator's, which is the consent David
-- gave: he asked for it about himself.
--
-- RETENTION IS THIRTY DAYS, swept by the chore that reads the table
-- (`infra/surface-usage.sh`, `maintenance-surface-usage`). The number
-- is a constant in boss_jobs::surface_opens::RETENTION_DAYS until the
-- retention registry lands (backlog 16115a17), at which point it
-- becomes a retention row and the constant goes. Counts per route per
-- actor per day are the product; the rows are raw material for one
-- month of them and nothing else.

CREATE TABLE IF NOT EXISTS surface_opens (
    id          BIGSERIAL PRIMARY KEY,
    -- The session's actor, as the gateway signs it: an employee id
    -- (`emp-david`) or a guest login (`guest@algedonic.dev`). Never an
    -- automation or an agent — the door refuses those.
    actor_id    TEXT NOT NULL,
    -- The route PATTERN the SPA opened (`/it/codebase`, `/ux/jobs/:jobId`).
    route       TEXT NOT NULL,
    -- When the operator opened it, on the one real clock: this is a
    -- fact about attention, not a business date, so it does not route
    -- through the sim-aware clock port.
    opened_at   TIMESTAMPTZ NOT NULL
);

-- The two reads: a window over everything (the sweep, the chore's
-- roll-up) and a window per actor per route (the roll-up's GROUP BY).
CREATE INDEX IF NOT EXISTS surface_opens_opened_at_idx
    ON surface_opens (opened_at);
CREATE INDEX IF NOT EXISTS surface_opens_actor_route_opened_at_idx
    ON surface_opens (actor_id, route, opened_at);

COMMENT ON TABLE surface_opens IS
  'One row per surface an operator opened in the SPA: the session''s '
  'actor, the route PATTERN (never a concrete id), and when. Not an '
  'audit event by decision (§Policy & auth: no per-request events); '
  'its own record, thirty-day retention swept by maintenance-surface-'
  'usage, rolled up daily per actor per route (backlog 628f182b).';
