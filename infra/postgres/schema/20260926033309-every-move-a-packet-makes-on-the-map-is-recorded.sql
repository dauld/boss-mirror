-- 20260926033309-every-move-a-packet-makes-on-the-map-is-recorded.sql
-- — the yard's moves record (design e765b3fc, "The Department Map",
-- §3; car M1 on feedback 84cba7e2).
--
-- WHY. David, on 84cba7e2: "I think we need to visualize the actual
-- jobs/cars moving as the animation rather than a representative." The
-- IT map's motion replays each rail's 24-hour RATE as tokens, and says
-- on screen that they are not individual events — because nothing
-- recorded an individual move. The regions read answers where packets
-- STAND; the audit log holds what happened to each packet; no record
-- said "this packet left the gates for the dock, because of event X".
--
-- A DERIVED PROJECTION, NOT A SECOND LOG. One mover loop per jobs-api
-- replica (boss-jobs `http::moves::run_mover`) reads the audit log's
-- head once a second, re-places the packets the new rows name through
-- the one placement definition (`regions::members`), and writes a row
-- here for every packet whose place changed. Each row CITES its cause
-- (`cause_event_id`, `cause_seq`, `cause_kind`) and takes its instant
-- from that event's `created_at`, so provenance is checkable row by
-- row against `audit_log`.
--
-- IDEMPOTENT: unique on (cause_event_id, packet). A replayed batch — a
-- restarted loop, a second replica reaching the same conclusion —
-- writes nothing twice.
--
-- `from_region` / `to_region` are NULL off the map: a packet filed onto
-- it, or leaving it. `declared` is whether the map draws the route a
-- region-to-region move took (NULL for on/off the map, whose off-ramps
-- arrive with the derived routes, car R2) — the observed-undeclared
-- count is `declared = false`, which is zero on a network whose map is
-- true. `handoff_from` names the packet this one continues under a new
-- identity (the gate-run whose green filed this car) and `lineage` the
-- key that links them; `aboard` lists a train's cars.
--
-- NO RETENTION YET. Measured 2026-09-25 17:21Z: the ten drawn borders
-- summed to 1,163 crossings a day, so a year is under half a million
-- narrow rows. The rates that will be read from here (car M3) need a
-- full window at least; a prune arrives with the first reader that
-- needs a bound.

CREATE TABLE IF NOT EXISTS yard_moves (
    -- The record's own order, which a stream resumes from
    -- (`Last-Event-ID`).
    seq            BIGSERIAL PRIMARY KEY,
    -- The causing event's created_at: wall clock, never sim time.
    at             TIMESTAMPTZ NOT NULL,
    packet         TEXT NOT NULL,
    kind           TEXT NOT NULL,
    -- The branch where the packet has one, otherwise its title.
    label          TEXT NOT NULL,
    from_region    TEXT,
    to_region      TEXT,
    declared       BOOLEAN,
    cause_event_id UUID NOT NULL,
    cause_seq      BIGINT NOT NULL,
    cause_kind     TEXT NOT NULL,
    handoff_from   TEXT,
    lineage        TEXT,
    aboard         JSONB NOT NULL DEFAULT '[]'::jsonb,
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT yard_moves_once UNIQUE (cause_event_id, packet),
    -- A move goes somewhere: never from a place to the same place, and
    -- never from off the map to off the map.
    CONSTRAINT yard_moves_goes_somewhere CHECK (from_region IS DISTINCT FROM to_region)
);

-- The window reads: the undeclared count per route, and the rates.
CREATE INDEX IF NOT EXISTS yard_moves_at ON yard_moves (at DESC);

COMMENT ON TABLE yard_moves IS
  'Every move a packet made on the IT map: from where to where, and the '
  'audit event that moved it. A projection of audit_log through the one '
  'placement definition (regions::members), idempotent on '
  '(cause_event_id, packet); design e765b3fc car M1.';
