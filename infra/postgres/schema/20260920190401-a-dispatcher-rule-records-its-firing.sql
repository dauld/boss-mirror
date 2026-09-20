-- 20260920190401-a-dispatcher-rule-records-its-firing.sql — the most
-- automated hop on the world map proves it ran.
--
-- Origin: backlog b14afc48, found by the builder of aeb008bd. The IT
-- world map's borders (crates/core/boss-jobs/src/borders.rs) name the
-- machine that moves each hop and say when it last fired. Every border
-- could answer that EXCEPT marshalling -> dock, whose machine is the
-- dispatcher rule `auto-park-on-gate-green`: verified 2026-09-19, no
-- table anywhere in this directory recorded a dispatcher rule firing,
-- so that border could only say 'nothing records a dispatcher rule's
-- firings'. The one hop nobody watches — because it is fully automated
-- — was the one hop the map could not evidence.
--
-- MORE THAN A MISSING PANEL. The map's trouble condition is 'work
-- waiting AND the machine silent', which is what separates a hop that
-- has STOPPED from one that is merely quiet. With no firing record that
-- condition cannot be evaluated for a dispatcher rule at all, so a
-- stalled auto-park would render exactly like an idle one.
--
-- THE SHAPE IS `cadence_firings` (114-cadence-rules.sql), deliberately:
-- one row per firing, a deterministic id as the exactly-once guard, and
-- the evidence in `detail`. A redelivered event recomputes the same
-- `dispatcher:<rule>:<event-id>` and the primary key dedupes it, so a
-- NAK-and-retry does not count as a second firing — the same reasoning
-- the cadence claim uses for a restarted conductor.
--
-- WHAT IS DELIBERATELY ABSENT: any notion of an expected interval. A
-- cadence rule DECLARES a heartbeat and can therefore be judged silent;
-- a dispatcher rule fires on an event and declares nothing, so a column
-- saying 'expected every N' here would manufacture false silence the
-- first quiet hour. This table answers 'fired at T', and the reader
-- (borders::machine_of) leaves `silent` null for it — never false,
-- which is how a dead machine renders healthy.
--
-- RETENTION IS THIRTY DAYS (boss_dispatcher::rules::firings::
-- RETENTION_DAYS), pruned by the sink that writes it, at most hourly.
-- Unlike a cadence rule's few firings an hour, the rules runner fires
-- tens per second at warp — the write is one batched INSERT per event
-- that matched, and the prune is what keeps a measurement table from
-- becoming a log nobody trims. The window is the same thirty days
-- `surface_opens` keeps, and becomes a retention row when that registry
-- lands (backlog 16115a17).

CREATE TABLE IF NOT EXISTS dispatcher_firings (
    -- `dispatcher:<rule_name>:<event_id>` — deterministic, so the same
    -- event redelivered is the same firing.
    firing_id TEXT PRIMARY KEY,
    rule_name TEXT NOT NULL,
    -- What triggered it: the NATS topic (or audit-log subject) the
    -- rule matched. The clock-driven schedule runner does not write
    -- here yet — it fires on a sim-day boundary rather than an event,
    -- and no surface asks when it last did; when one does, its rows
    -- land in this column as `schedule:<cadence>`.
    fired_on  TEXT NOT NULL,
    -- When the runner dispatched it. Bound by the writer, never SQL
    -- NOW(), so a replayed record keeps the instant it happened.
    fired_at  TIMESTAMPTZ NOT NULL,
    -- Evidence: the triggering event id, the handlers that ran, and
    -- whether the payload was simulated.
    detail    JSONB NOT NULL DEFAULT '{}'::jsonb
);

-- The one read the map takes (newest firing of a named rule), and the
-- prune's window.
CREATE INDEX IF NOT EXISTS dispatcher_firings_rule_recency
    ON dispatcher_firings (rule_name, fired_at DESC);
CREATE INDEX IF NOT EXISTS dispatcher_firings_fired_at_idx
    ON dispatcher_firings (fired_at);

COMMENT ON TABLE dispatcher_firings IS
  'One row per dispatcher rule firing: which rule, what triggered it, '
  'when, and the handlers that ran. The cadence_firings idiom applied '
  'to the reactive side — its own record, not an audit event, thirty-'
  'day retention. It holds NO expected interval: an event-driven rule '
  'declares no heartbeat, so its silence is not a reading (b14afc48).';
