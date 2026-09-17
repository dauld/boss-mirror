//! Sensors — readings of the world outside BOSS, taken on a cadence,
//! recorded outside the audit log, turned into work when the operating
//! model says to act on them. Design 14c9b2ad (decided with David
//! 2026-09-17: "Polling is fine, use a restricted read-only key"),
//! backlog 2d33e111.
//!
//! **Two natures, one module.** The `sensors` registry says what to
//! read, with which credential, how often, and what work one reading
//! opens; its rows are TENANT data (`seeds/sensors.toml`, admitted by
//! `boss tenant publish` through the batch door). The `sensor_readings`
//! table is TELEMETRY — David, 2026-09-16: "the audit_log just needs to
//! capture all work that is done; sensors record data that don't flow
//! into the audit_log." A reading is not a state change of the company;
//! the PACKET the poller opens for it is, and that packet is the
//! audit-log fact. So the readings take the `surface_opens` shape — the
//! row is its own record, no outbox, no rebuilder.
//!
//! **Idempotent by external id.** `UNIQUE (sensor_id, external_id)` and
//! insert-if-absent at the door: a redelivery re-reads the same page and
//! inserts nothing new; a reading whose `packet_id` is NULL is the one
//! obligation left, and stamping it closes it. Two firings never open
//! two packets for one charge — pinned by the poll handler's test that
//! runs twice against the in-memory source and counts one packet.
//!
//! **The cursor lives on the sensor.** `cursor_at` (the newest
//! `observed_at` a good read returned) and `last_polled_at` are columns
//! on the registry row, advanced by the poller through
//! [`Sensors::mark_polled`], not derived from the readings — which are
//! swept on [`RETENTION_DAYS`] and would reset the cursor to the
//! beginning of time after a quiet season.
//!
//! **The value is never here.** A sensor names a `credentials` registry
//! id; the poll handler resolves that to an env var and sends the value
//! as a bearer. Nothing in this module, its tables or its HTTP surface
//! carries a secret.
//!
//! **Two kinds of source.** A POLLED source (`stripe`) is one the
//! product reads on the sensor's cadence with the sensor's credential;
//! its readings each open a packet. A PUSH-ONLY source
//! ([`PUSH_ONLY_SOURCES`]: `site`, backlog 0b5c5081) is one nothing
//! reads: the service that observed the world POSTs the readings to
//! `/api/sensors/{id}/readings` itself — the gateway's site surface
//! records one `www-visits` reading per page view — so the row
//! declares no credential and no period, `due_at` is never true, and
//! the readings owe no packet (a chore reads them in bulk when the
//! operating model says so). The readings door admits a push to a
//! polled sensor from its own poll actor ONLY: anyone else pushing
//! there would open a packet the source never observed, and is told so
//! (409). The sweep deletes a push-only reading by age alone, since no
//! stamp will ever come.
//!
//! Hexagonal like `surface_opens`: types + port + in-memory adapter +
//! Pg adapter + HTTP door, plus the one TOML loader `boss tenant check`
//! and `boss tenant publish` share ([`seed::load_sensors_toml`]).

pub mod http;
pub mod in_memory;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod seed;
pub mod types;

pub use in_memory::InMemorySensors;
pub use port::{Sensors, SensorsError, sweep_retention};
#[cfg(feature = "postgres")]
pub use postgres::PgSensors;
pub use seed::load_sensors_toml;
pub use types::{
    BatchOutcome, NewReading, PUSH_ONLY_SOURCES, PollStamp, RETENTION_DAYS, Reading, SensorInput,
    SensorRow, Sweep, is_push_only, sensor_actor, validate_sensor,
};
