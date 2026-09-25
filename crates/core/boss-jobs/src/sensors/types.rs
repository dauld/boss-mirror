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

/// The sources that READ NOTHING: their readings arrive through
/// `POST /api/sensors/{id}/readings` from the service that observed
/// them — the gateway's site surface for `site` (backlog 0b5c5081) —
/// so the poll never visits them (`SensorRow::due_at` is false), the
/// declaration carries no credential and no period, and their
/// readings owe no packet, which is why the retention sweep deletes
/// them by age alone. Every other source is polled: `stripe` today.
pub const PUSH_ONLY_SOURCES: &[&str] = &["site"];

/// Is `source` one nothing polls?
pub fn is_push_only(source: &str) -> bool {
    PUSH_ONLY_SOURCES.contains(&source)
}

/// The actor a sensor's own poll signs as — the packet's owner, its
/// `opened_by`, and the one writer the readings door admits to a
/// POLLED sensor (a push from anyone else would open a packet the
/// source never observed). One spelling; the poll handler reads it
/// from here.
pub fn sensor_actor(sensor_id: &str) -> String {
    format!("automation:sensor:{sensor_id}")
}

/// One sensor as a tenant declares it (`[[sensor]]` in
/// `seeds/sensors.toml`) and as the batch door takes it. The tenant id
/// rides on the batch, not the row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorInput {
    pub id: String,
    /// Which adapter reads it — `stripe` (succeeded charges) and
    /// `stripe-payouts` (paid payouts) are the two the dispatcher
    /// binary registers — or a push-only source ([`PUSH_ONLY_SOURCES`])
    /// nothing reads.
    pub source: String,
    /// The `credentials` registry id the poller sends the value of.
    /// Empty (the default) is admitted only for a push-only source.
    #[serde(default)]
    pub credential: String,
    /// Minutes between polls. `0` (the default) is admitted only for
    /// a push-only source, where it says "never polled".
    #[serde(default)]
    pub every_minutes: i32,
    /// The workflow kind one reading opens.
    pub opens: String,
    /// The subject kind of the packet (its id is the reading's
    /// external id).
    pub subject_kind: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// WHICH STREAM within the source, when the source has more than
    /// one and they are not separate sources.
    ///
    /// The first case is mail (design bffc0aba, David 2026-09-21: "use
    /// inbox names to trigger different protocols"). `support@` and
    /// `finance@` route into ONE mailbox, and each must open a
    /// different department's protocol — so several sensors read the
    /// same place and each takes only its own mail. Without this the
    /// routing cannot be expressed as data, and the fallback is a
    /// branch per department inside one protocol: a `match` statement
    /// in a registry row, which is what §9 exists to prevent.
    ///
    /// `None` for a source with one stream, which is every source that
    /// exists today — Stripe's key IS the selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
}

fn enabled_default() -> bool {
    true
}

impl SensorInput {
    /// Declares a source nothing polls ([`PUSH_ONLY_SOURCES`]).
    pub fn is_push_only(&self) -> bool {
        is_push_only(&self.source)
    }
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
    slug("opens", &s.opens)?;
    slug("subject_kind", &s.subject_kind)?;
    if s.is_push_only() {
        // Nothing polls it: a credential it would never send and a
        // period it would never wait are refused as the untruth they
        // would be in the registry, not admitted as harmless.
        if !s.credential.is_empty() {
            return Err(format!(
                "sensor {}: source `{}` is push-only (nothing polls it), so it takes no credential",
                s.id, s.source
            ));
        }
        if s.every_minutes != 0 {
            return Err(format!(
                "sensor {}: source `{}` is push-only (nothing polls it), so it takes no every_minutes",
                s.id, s.source
            ));
        }
        if s.selector.is_some() {
            return Err(format!(
                "sensor {}: source `{}` is push-only (nothing polls it), so it takes no selector",
                s.id, s.source
            ));
        }
        return Ok(());
    }
    slug("credential", &s.credential)?;
    // NOT kebab-case: a selector names something the OUTSIDE world
    // already named — an address, a folder, a label — and
    // `support@algedonic.dev` is not kebab-case and never will be.
    // Empty is refused rather than read as absent, because the two mean
    // different things and a selector that silently meant everything
    // would hand one sensor every other sensor's mail.
    if let Some(sel) = &s.selector
        && sel.trim().is_empty()
    {
        return Err(format!(
            "sensor {}: selector is declared but blank — name the stream it reads, or omit it",
            s.id
        ));
    }
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
    /// Which stream within the source — see [`SensorInput::selector`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    pub tenant_id: String,
    pub published_at: DateTime<Utc>,
    /// When the poller last attempted this sensor (good or unreadable).
    pub last_polled_at: Option<DateTime<Utc>>,
    /// The newest `observed_at` a good read returned.
    pub cursor_at: Option<DateTime<Utc>>,
}

impl SensorRow {
    /// Holds a source nothing polls ([`PUSH_ONLY_SOURCES`]).
    pub fn is_push_only(&self) -> bool {
        is_push_only(&self.source)
    }

    /// Is this sensor due at `now`? Enabled, polled (a push-only
    /// source is never due: its readings come to it), and either
    /// never polled or polled at least `every_minutes` ago. The one
    /// decision the 5-minute platform cadence makes per row.
    pub fn due_at(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled || self.is_push_only() {
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

/// One sensor's readings over a window — the count a retro reads
/// (backlog 35baed54). The window is `[since, until)` over
/// `observed_at`, the instant the SOURCE gives and the one the
/// retention sweep ages by. `arrived` counts every reading in it,
/// `stamped` those that opened a packet (`unstamped` is the rest), and
/// `packets` names the packets, sorted and distinct. Until this, the
/// readings door answered the unstamped readings only, so a reading
/// that opened a packet left every count the moment it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingsWindow {
    pub sensor_id: String,
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub arrived: u64,
    pub stamped: u64,
    pub unstamped: u64,
    pub packets: Vec<String>,
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
            selector: None,
        }
    }

    /// ONE SOURCE, MANY STREAMS (design bffc0aba, David 2026-09-21:
    /// "use inbox names to trigger different protocols"). A mailbox is
    /// the first source where several sensors read the SAME place and
    /// each must take only its own mail: support@ and finance@ both
    /// route into one Proton mailbox, and each opens a different
    /// department's protocol. Without a selector that routing cannot be
    /// expressed as data at all, and the fallback is a branch per
    /// department inside one protocol — a `match` statement in a
    /// registry row, which is what §9 exists to prevent.
    #[test]
    fn a_sensor_may_select_within_its_source() {
        let mut s = input();
        s.selector = Some("support@algedonic.dev".into());
        assert!(validate_sensor(&s).is_ok(), "a selector is admitted");
    }

    /// NOT kebab-case, deliberately. Every other string on a sensor row
    /// is an identifier this system coins; a selector names something
    /// the OUTSIDE world already named — an address, a folder, a label —
    /// and `support@algedonic.dev` is not kebab-case and never will be.
    /// Holding it to that rule would refuse every real value.
    #[test]
    fn a_selector_is_not_held_to_the_kebab_case_rule() {
        let mut s = input();
        s.selector = Some("Support Inbox/2026".into());
        assert!(validate_sensor(&s).is_ok(), "{:?}", validate_sensor(&s));
    }

    /// An EMPTY selector is refused rather than treated as absent. The
    /// two mean different things — absent is "this source has one
    /// stream", empty is "I meant to name one and did not" — and a
    /// selector that silently means everything would give one sensor
    /// every other sensor's mail.
    #[test]
    fn an_empty_selector_is_refused_rather_than_read_as_absent() {
        let mut s = input();
        s.selector = Some("   ".into());
        let err = validate_sensor(&s).expect_err("blank");
        assert!(err.contains("selector"), "{err}");
    }

    /// A push-only source is not polled, so it selects nothing — the
    /// same reasoning that already refuses it a credential and a
    /// period, rather than admitting a field that would never be read.
    #[test]
    fn a_push_only_source_takes_no_selector() {
        let site = SensorInput {
            id: "www-visits".into(),
            source: "site".into(),
            credential: String::new(),
            every_minutes: 0,
            opens: "marketing-weekly".into(),
            subject_kind: "custom".into(),
            enabled: true,
            selector: Some("anything".into()),
        };
        let err = validate_sensor(&site).expect_err("push-only");
        assert!(err.contains("selector"), "{err}");
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

    /// A push-only source (backlog 0b5c5081): admitted with no
    /// credential and no period — both default — and refused with
    /// either, since nothing would ever use them.
    #[test]
    fn a_push_only_source_takes_no_credential_and_no_period() {
        let site = SensorInput {
            id: "www-visits".into(),
            source: "site".into(),
            credential: String::new(),
            every_minutes: 0,
            opens: "marketing-weekly".into(),
            subject_kind: "custom".into(),
            enabled: true,
            selector: None,
        };
        assert!(site.is_push_only());
        assert!(validate_sensor(&site).is_ok());
        let mut bad = site.clone();
        bad.credential = "some-key".into();
        assert!(validate_sensor(&bad).unwrap_err().contains("no credential"));
        let mut bad = site.clone();
        bad.every_minutes = 15;
        assert!(
            validate_sensor(&bad)
                .unwrap_err()
                .contains("no every_minutes")
        );
        // A polled source still needs both.
        let mut polled = site;
        polled.source = "stripe".into();
        assert!(
            validate_sensor(&polled)
                .unwrap_err()
                .contains("credential is required")
        );
        polled.credential = "c".into();
        assert!(
            validate_sensor(&polled)
                .unwrap_err()
                .contains("every_minutes")
        );
        assert_eq!(sensor_actor("www-visits"), "automation:sensor:www-visits");
    }

    /// The tenant's spelling of a push-only sensor: no credential
    /// line, no every_minutes line.
    #[test]
    fn a_push_only_declaration_omits_credential_and_period() {
        let s: SensorInput = toml::from_str(
            "id = \"www-visits\"\nsource = \"site\"\nopens = \"marketing-weekly\"\n\
             subject_kind = \"custom\"\n",
        )
        .unwrap();
        assert!(validate_sensor(&s).is_ok(), "{s:?}");
        assert_eq!(s.every_minutes, 0);
        assert!(s.credential.is_empty());
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
            selector: None,
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
        // A push-only source is never due: the poll skips it.
        let mut site = row(None, true);
        site.source = "site".into();
        site.every_minutes = 0;
        assert!(site.is_push_only());
        assert!(!site.due_at(now));
    }
}
