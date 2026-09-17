-- 20260917041600-sensors-are-declared-and-their-readings-are-kept.sql —
-- a SENSOR is a reading of the world outside BOSS, taken on a cadence,
-- recorded outside the audit log, and turned into WORK (a packet) when
-- the operating model says to act on it. Design 14c9b2ad (decided with
-- David 2026-09-17), backlog 2d33e111. The first sensor reads Stripe's
-- charges and opens a receive-a-sponsorship packet per new one.
--
-- TWO TABLES, TWO NATURES.
--
-- `sensors` is a REGISTRY: what to read, with which credential, how
-- often, and what work to open. The rows are TENANT data — the product
-- knows how to read Stripe; WHAT to open for it is the company's
-- business and lives with the company (`seeds/sensors.toml`, admitted
-- by `boss tenant publish` through POST /api/sensors/batch,
-- insert-if-absent like the classes batch). The two cursor columns are
-- the poller's own bookkeeping, not declaration.
--
-- `sensor_readings` is TELEMETRY. David, 2026-09-16: "the audit_log
-- just needs to capture all work that is done; sensors record data
-- that don't flow into the audit_log." A reading is not a state change
-- of the company; the PACKET the handler opens for it is, and that
-- packet is the audit-log fact. So this is the `surface_opens` shape —
-- the row is its own record, no outbox, no rebuilder — and never the
-- `agent_runs` one.
--
-- IDEMPOTENT BY EXTERNAL ID. `UNIQUE (sensor_id, external_id)`: a
-- redelivery or an overlapping cursor re-reads the same page and
-- inserts nothing new (ON CONFLICT DO NOTHING at the door); a reading
-- whose `packet_id` is NULL is the one obligation left — open the
-- packet, stamp it — and a reading stamped is done. Two firings can
-- never open two packets for one charge.
--
-- THE CURSOR LIVES ON THE SENSOR, NOT IN THE READINGS. `cursor_at` is
-- the newest `observed_at` a good read returned, advanced monotonically
-- after each poll. It is a column here rather than MAX(observed_at)
-- over the readings on purpose: the readings are swept on a retention
-- (boss_jobs::sensors::RETENTION_DAYS), and a cursor derived from
-- swept rows would reset to "the beginning of time" after a quiet
-- season and re-read every charge the source still holds — the unique
-- key would stop the duplicate packets, but the source would be read
-- whole for nothing. `last_polled_at` is what the 5-minute platform
-- cadence compares `every_minutes` against.

CREATE TABLE IF NOT EXISTS sensors (
    -- The tenant's name for it, e.g. 'stripe-sponsorships'; the actor
    -- the poll signs as is automation:sensor:<id>.
    id              TEXT PRIMARY KEY,
    -- Which adapter reads it ('stripe'); an id the product's source
    -- registry does not know is an unreadable sensor, alarmed.
    source          TEXT NOT NULL,
    -- The `credentials` registry id whose value the handler sends —
    -- resolved to its env var by the handler, never stored here.
    credential      TEXT NOT NULL,
    every_minutes   INTEGER NOT NULL CHECK (every_minutes >= 1),
    -- The workflow kind one reading opens, and the subject kind the
    -- packet is about (subject id = the reading's external id).
    opens_kind      TEXT NOT NULL,
    subject_kind    TEXT NOT NULL,
    enabled         BOOLEAN NOT NULL DEFAULT true,
    -- Which tenant published the row (`[meta] tenant_id`).
    tenant_id       TEXT NOT NULL,
    published_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The poller's bookkeeping (see the header).
    last_polled_at  TIMESTAMPTZ,
    cursor_at       TIMESTAMPTZ
);

CREATE TABLE IF NOT EXISTS sensor_readings (
    id            BIGSERIAL PRIMARY KEY,
    sensor_id     TEXT NOT NULL REFERENCES sensors (id),
    -- The source's own id for the observation (a Stripe charge id).
    external_id   TEXT NOT NULL,
    -- When the SOURCE says it happened (a charge's `created`), on the
    -- one real clock: a sensor reads the real world, so this does not
    -- route through the sim-aware clock port.
    observed_at   TIMESTAMPTZ NOT NULL,
    payload       JSONB NOT NULL,
    -- The packet opened for it, once opened. NULL = still owed.
    packet_id     TEXT,
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (sensor_id, external_id)
);

-- The two reads: the retention sweep (a window over observed_at) and
-- "which readings still owe a packet" (small, partial).
CREATE INDEX IF NOT EXISTS sensor_readings_observed_at_idx
    ON sensor_readings (observed_at);
CREATE INDEX IF NOT EXISTS sensor_readings_unstamped_idx
    ON sensor_readings (sensor_id)
    WHERE packet_id IS NULL;

COMMENT ON TABLE sensors IS
  'The sensor registry (design 14c9b2ad): what the platform polls on a '
  'cadence, with which credential, and what work a reading opens. '
  'Tenant-published through POST /api/sensors/batch; cursor columns are '
  'the poller''s own.';
COMMENT ON TABLE sensor_readings IS
  'Telemetry, not audit: one row per observation a sensor made, unique '
  'per (sensor, external id). The packet opened for a reading is the '
  'audit-log fact; the reading is not (David 2026-09-16). Swept on '
  'boss_jobs::sensors::RETENTION_DAYS.';
