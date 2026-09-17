//! The sensor registry's shapes: a declaration as a tenant writes it,
//! a row as the registry holds it, a reading, the poll stamp, the sweep.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How long a reading lives, in days. A CONSTANT until the retention
/// registry lands (backlog 16115a17) — then a retention row keyed on
/// the table, and this goes. Ninety days, longer than `surface_opens`'
/// thirty: a reading is the evidence a packet was opened FROM, and a
/// quarter covers the reconciliation window of the work it opened (a
/// sponsorship's books close quarterly). The cursor does not depend on
/// the rows surviving (it is on the sensor), so this is a bound on
/// evidence, not on correctness.
pub const RETENTION_DAYS: i64 = 90;

/// One sensor as a tenant declares it (`[[sensor]]` in
/// `seeds/sensors.toml`) and as the batch door takes it. The tenant id
/// rides on the batch, not the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorInput {
    pub id: String,
    /// Which adapter reads it — `stripe` is the first.
    pub source: String,
    /// The `credentials` registry id the poller sends the value of.
    pub credential: String,
    pub every_minutes: i32,
    /// The workflow kind one reading opens.
    pub opens: String,
    /// The subject kind of the packet (its id is the reading's
    /// external id).
    pub subject_kind: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

fn enabled_default() -> bool {
    true
}

/// Why a declaration is refused. Named so the refusal says which test
/// failed, and the same check runs in `boss tenant check`, the batch
/// door and the in-memory adapter.
pub fn validate_sensor(s: &SensorInput) -> Result<(), String> {
    let slug = |field: &str, v: &str| -> Result<(), String> {
        if v.is_empty() {
            return Err(format!("sensor {}: {field} is required", s.id));
        }
        if !v
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "sensor {}: {field} `{v}` is not kebab-case (lowercase, digits, hyphens)",
                s.id
            ));
        }
        Ok(())
    };
    if s.id.is_empty() {
        return Err("a sensor needs an id (e.g. stripe-sponsorships)".into());
    }
    slug("id", &s.id)?;
    slug("source", &s.source)?;
    slug("credential", &s.credential)?;
    slug("opens", &s.opens)?;
    slug("subject_kind", &s.subject_kind)?;
    if s.every_minutes < 1 {
        return Err(format!(
            "sensor {}: every_minutes is {}; a sensor polls at least once a minute",
            s.id, s.every_minutes
        ));
    }
    Ok(())
}

/// What `POST /api/sensors/batch` takes: the publishing tenant and its
/// declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorBatch {
    pub tenant_id: String,
    pub sensors: Vec<SensorInput>,
}

/// One registry row, as the poller reads it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorRow {
    pub id: String,
    pub source: String,
    pub credential: String,
    pub every_minutes: i32,
    pub opens_kind: String,
    pub subject_kind: String,
    pub enabled: bool,
    pub tenant_id: String,
    pub published_at: DateTime<Utc>,
    /// When the poller last attempted this sensor (good or unreadable).
    pub last_polled_at: Option<DateTime<Utc>>,
    /// The newest `observed_at` a good read returned.
    pub cursor_at: Option<DateTime<Utc>>,
}

impl SensorRow {
    /// Is this sensor due at `now`? Enabled, and either never polled
    /// or polled at least `every_minutes` ago. The one decision the
    /// 5-minute platform cadence makes per row.
    pub fn due_at(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_polled_at {
            None => true,
            Some(last) => now - last >= chrono::Duration::minutes(i64::from(self.every_minutes)),
        }
    }
}

/// What a batch did: how many rows it received and how many it
/// inserted (the rest were already there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchOutcome {
    pub received: usize,
    pub inserted: usize,
}

/// One observation as the poller records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReading {
    pub external_id: String,
    pub observed_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// One reading as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub sensor_id: String,
    pub external_id: String,
    pub observed_at: DateTime<Utc>,
    pub payload: serde_json::Value,
    pub packet_id: Option<String>,
}

/// What a poll stamps on its sensor when it is done: when it ran, and
/// the newest observation it saw (absent when it saw none, or could not
/// read — the cursor never moves backwards and never on a failed read).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollStamp {
    pub polled_at: DateTime<Utc>,
    #[serde(default)]
    pub cursor_at: Option<DateTime<Utc>>,
}

/// What a retention sweep did (the `surface_opens` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sweep {
    pub deleted: u64,
    pub before: DateTime<Utc>,
    pub retention_days: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SensorInput {
        SensorInput {
            id: "stripe-sponsorships".into(),
            source: "stripe".into(),
            credential: "stripe-restricted-read".into(),
            every_minutes: 15,
            opens: "receive-a-sponsorship".into(),
            subject_kind: "custom".into(),
            enabled: true,
        }
    }

    #[test]
    fn a_declaration_is_kebab_case_ids_and_a_positive_period() {
        assert!(validate_sensor(&input()).is_ok());
        let mut bad = input();
        bad.every_minutes = 0;
        assert!(validate_sensor(&bad).unwrap_err().contains("every_minutes"));
        let mut bad = input();
        bad.opens = "Receive A Sponsorship".into();
        assert!(validate_sensor(&bad).unwrap_err().contains("kebab-case"));
        let mut bad = input();
        bad.credential = String::new();
        assert!(validate_sensor(&bad).unwrap_err().contains("required"));
        let mut bad = input();
        bad.id = String::new();
        assert!(validate_sensor(&bad).unwrap_err().contains("needs an id"));
    }

    #[test]
    fn enabled_defaults_to_true_in_the_declaration() {
        let s: SensorInput = toml::from_str(
            "id = \"a\"\nsource = \"stripe\"\ncredential = \"c\"\nevery_minutes = 5\n\
             opens = \"k\"\nsubject_kind = \"custom\"\n",
        )
        .unwrap();
        assert!(s.enabled);
    }

    fn row(last: Option<&str>, enabled: bool) -> SensorRow {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        SensorRow {
            id: "s".into(),
            source: "stripe".into(),
            credential: "c".into(),
            every_minutes: 15,
            opens_kind: "k".into(),
            subject_kind: "custom".into(),
            enabled,
            tenant_id: "t".into(),
            published_at: at("2026-09-17T00:00:00Z"),
            last_polled_at: last.map(at),
            cursor_at: None,
        }
    }

    /// Due = enabled and the period has elapsed since the last attempt;
    /// never polled is due now; disabled is never due.
    #[test]
    fn a_sensor_is_due_when_its_period_has_elapsed() {
        let now = DateTime::parse_from_rfc3339("2026-09-17T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(row(None, true).due_at(now));
        assert!(row(Some("2026-09-17T09:45:00Z"), true).due_at(now));
        assert!(!row(Some("2026-09-17T09:50:00Z"), true).due_at(now));
        assert!(!row(None, false).due_at(now));
    }
}
