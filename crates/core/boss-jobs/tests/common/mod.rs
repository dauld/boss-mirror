//! Shared scaffolding for the station boot-check tests.
//!
//! Two test binaries read the same boot pass from different angles —
//! `station_boot_quarantine` asserts the report and that boot wrote
//! nothing, `station_boot_log` asserts the ERROR line — so the rows
//! they seed live here rather than twice.
//!
//! Provides:
//! - `now()` — the fixed clock every spec is drafted against
//! - `viable(name)` / `always_empty(name)` — a healthy row and a
//!   contradictory one (the same key demanded present and absent)
//! - `seed_active()` — seeds straight to ACTIVE, as the SQL
//!   migrations do, bypassing publish
//! - `Captured` — a `MakeWriter` that collects what
//!   `tracing_subscriber::fmt` prints, so a test can read the boot
//!   log the way an operator would

#![allow(dead_code)]

use std::io::Write;
use std::sync::{Arc, Mutex};

use boss_core::job::JobStatus;
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::{InMemoryStations, StationKind, StationSpec};

pub fn now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap()
}

/// Seeded straight to ACTIVE, exactly as the SQL migrations do —
/// bypassing publish, which is the whole point of this pass.
pub fn seed_active(stations: &InMemoryStations, mut spec: StationSpec) {
    spec.status = WorkflowStatus::Active;
    stations.seed(spec).unwrap();
}

pub fn viable(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        "A real queue",
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            ..Default::default()
        },
        now(),
    )
}

/// A contradiction: the same key demanded present and absent.
pub fn always_empty(name: &str) -> StationSpec {
    let mut s = viable(name);
    s.predicate.metadata_present = vec!["train".into()];
    s.predicate.metadata_absent = vec!["train".into()];
    s
}

/// A `MakeWriter` that collects everything `tracing_subscriber::fmt`
/// prints, so a test can read the boot log the way an operator would.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
