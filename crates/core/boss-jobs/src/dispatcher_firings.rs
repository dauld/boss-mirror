//! The dispatcher's firing record — read-only here (backlog b14afc48).
//!
//! `dispatcher_firings` (migration
//! `20260920190401-a-dispatcher-rule-records-its-firing.sql`) holds one
//! row per dispatcher rule firing, written by the rules runner that
//! fired it (`boss_dispatcher::rules::firings`). The jobs API only ever
//! READS it, for one question the IT world map asks of every border:
//! when did this machine last fire?
//!
//! TWO READS, EACH A QUESTION A SURFACE ASKS. The map needs the newest
//! firing of a NAMED rule, which is the `dispatcher_firings_rule_recency`
//! index's own read. The second reader the first version of this port
//! anticipated arrived with the rules list (backlog 43c4451a, found by
//! page-audit 08a444bc): `/it/registry/rules` could not say when any
//! rule last fired, so a stalled `auto-park-on-gate-green` and an idle
//! one painted the same row. It asks for the newest firing of EVERY
//! rule — still one row per rule, never a page of firings a surface
//! would have to reduce.
//!
//! THE OTHER HALF OF A RULE'S HEALTH IS NOT IN THIS TABLE. A firing is
//! recorded only when every handler succeeded; a handler that failed
//! past its budget dead-letters onto the packet it owed
//! (`boss_dispatcher::rules::dead_letter`, a9c498eb) as the
//! [`DEAD_LETTER_KEY`] metadata key. [`dead_letter_rollup`] is the
//! per-rule reading of those annotations — pure, over packets the
//! caller already read — so "fired an hour ago" can sit beside "and
//! dead-lettered ten minutes ago", which is what tells a stalled rule
//! from an idle one.
//!
//! WHAT THIS PORT CANNOT ANSWER, and must not pretend to: how often a
//! rule is EXPECTED to fire. A dispatcher rule fires on an event and
//! declares no heartbeat, so there is no interval to compare a silence
//! against — see [`crate::borders::DispatcherFiring`].

use async_trait::async_trait;
use boss_core::job::Job;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// How long a firing row is kept. Unlike a cadence rule's few firings
/// an hour, the rules runner fires tens per second at warp, so the
/// writer prunes its own table — a measurement record nobody trims is
/// a log. Thirty days is the window `surface_opens` keeps, and becomes
/// a retention row when that registry lands (backlog 16115a17).
///
/// ONE COPY. The writer (`boss_dispatcher::rules::firings`, which
/// depends on this crate) prunes by it and re-exports it from here; the
/// rules list says "no firing in the last N days" by it. It lived only
/// in the writer until the reader needed it (backlog 43c4451a), and a
/// reader typing its own 30 would have been a second copy free to drift.
pub const RETENTION_DAYS: i64 = 30;

/// The packet metadata key a dispatcher dead-letter is written under.
/// The writer (`boss_dispatcher::rules::dead_letter::METADATA_KEY`)
/// re-exports this one; the rollup below reads it.
pub const DEAD_LETTER_KEY: &str = "dead_letter";

#[derive(Debug, thiserror::Error)]
pub enum DispatcherFiringsError {
    #[error("storage: {0}")]
    Storage(String),
}

/// The newest firing of one rule, as the row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFiring {
    pub firing_id: String,
    /// The topic the rule matched on.
    pub fired_on: String,
    pub fired_at: DateTime<Utc>,
}

#[async_trait]
pub trait DispatcherFiringsRepository: Send + Sync {
    /// The newest firing of `rule`, or `None` when the record holds no
    /// row for it. An error is NOT a `None`: the caller has to be able
    /// to tell "never fired" from "could not be read", because the
    /// second rendered as the first is how a dead machine reads
    /// healthy.
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, DispatcherFiringsError>;

    /// The newest firing of EVERY rule the record holds, one entry per
    /// rule, ordered by rule name. A rule with no row is absent — the
    /// caller holds the rule list and says "no firing recorded" for it
    /// by name. An error is the whole record unread, never an empty
    /// list: the same line [`Self::last_firing`] draws between "never
    /// fired" and "could not be read".
    async fn last_firings(&self) -> Result<Vec<RuleLastFiring>, DispatcherFiringsError>;
}

/// One rule's newest firing, as the every-rule read answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleLastFiring {
    pub rule: String,
    pub fired_on: String,
    pub fired_at: DateTime<Utc>,
}

/// Every rule a dead-letter annotation names. The hoisted `rule` is the
/// FIRST failure only; `failures` carries every one, each spelled
/// `<rule>/<handler>: <error>` (the writer's `HandlerFailure` Display),
/// so a multi-handler topic that failed two rules is counted against
/// both. Rule names are kebab-case and never hold a `/`, so the text
/// before the first `/` is the rule. The writer's own test holds its
/// annotation to this reading (`boss_dispatcher::rules::dead_letter`,
/// `the_rollup_reads_every_rule_the_annotation_names`), so the spelling
/// cannot move on one side alone.
pub fn dead_letter_rules(annotation: &Value) -> Vec<String> {
    let hoisted = annotation
        .get("rule")
        .and_then(Value::as_str)
        .map(str::to_string);
    let listed = annotation
        .get("failures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|f| f.split_once('/').map(|(rule, _)| rule.to_string()));
    let mut rules: Vec<String> = hoisted
        .into_iter()
        .chain(listed)
        .filter(|r| !r.is_empty())
        .collect();
    rules.sort();
    rules.dedup();
    rules
}

/// One rule's dead-letters, rolled up over the packets that carry one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadLetterRollup {
    pub rule: String,
    /// How many packets carry a dead-letter naming this rule, recorded
    /// inside the window.
    pub packets: usize,
    /// The newest of those annotations' `recorded_at`, when any could
    /// be read.
    pub newest_at: Option<DateTime<Utc>>,
    /// The packet that carries the newest one — where a reader goes to
    /// see what failed.
    pub newest_job_id: Option<String>,
}

/// Per-rule dead-letter counts over `jobs`, keeping annotations recorded
/// at or after `since` — the firing record's own retention window, so
/// the two columns a surface draws side by side answer the same period.
/// Packets with no [`DEAD_LETTER_KEY`] are ignored. An annotation whose
/// `recorded_at` cannot be read is COUNTED, undated: nothing shows it is
/// outside the window, and a failure dropped for a bad timestamp is the
/// quiet this rollup exists to end. Ordered by rule name.
pub fn dead_letter_rollup(jobs: &[Job], since: DateTime<Utc>) -> Vec<DeadLetterRollup> {
    let mut out: std::collections::BTreeMap<String, DeadLetterRollup> =
        std::collections::BTreeMap::new();
    for job in jobs {
        let Some(annotation) = job.metadata.get(DEAD_LETTER_KEY) else {
            continue;
        };
        let at = annotation
            .get("recorded_at")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        if at.is_some_and(|t| t < since) {
            continue;
        }
        for rule in dead_letter_rules(annotation) {
            let entry = out.entry(rule.clone()).or_insert_with(|| DeadLetterRollup {
                rule,
                packets: 0,
                newest_at: None,
                newest_job_id: None,
            });
            entry.packets += 1;
            if let Some(t) = at
                && entry.newest_at.is_none_or(|n| t > n)
            {
                entry.newest_at = Some(t);
                entry.newest_job_id = Some(job.id.to_string());
            }
        }
    }
    out.into_values().collect()
}

/// In-memory adapter: the firings a test declares, keyed by rule name,
/// newest-wins.
pub struct InMemoryDispatcherFirings {
    firings: Vec<(String, LastFiring)>,
}

impl InMemoryDispatcherFirings {
    pub fn new(firings: Vec<(String, LastFiring)>) -> Self {
        Self { firings }
    }
}

#[async_trait]
impl DispatcherFiringsRepository for InMemoryDispatcherFirings {
    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, DispatcherFiringsError> {
        Ok(self
            .firings
            .iter()
            .filter(|(name, _)| name == rule)
            .map(|(_, f)| f)
            .max_by_key(|f| f.fired_at)
            .cloned())
    }

    async fn last_firings(&self) -> Result<Vec<RuleLastFiring>, DispatcherFiringsError> {
        let mut newest: std::collections::BTreeMap<&str, &LastFiring> =
            std::collections::BTreeMap::new();
        for (rule, f) in &self.firings {
            let slot = newest.entry(rule.as_str()).or_insert(f);
            if f.fired_at > slot.fired_at {
                *slot = f;
            }
        }
        Ok(newest
            .into_iter()
            .map(|(rule, f)| RuleLastFiring {
                rule: rule.to_string(),
                fired_on: f.fired_on.clone(),
                fired_at: f.fired_at,
            })
            .collect())
    }
}

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::{PgPool, Row};

    /// Postgres adapter. One statement, the recency index's own.
    pub struct PgDispatcherFirings {
        pool: PgPool,
    }

    impl PgDispatcherFirings {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    fn storage(e: sqlx::Error) -> DispatcherFiringsError {
        DispatcherFiringsError::Storage(e.to_string())
    }

    #[async_trait]
    impl DispatcherFiringsRepository for PgDispatcherFirings {
        async fn last_firing(
            &self,
            rule: &str,
        ) -> Result<Option<LastFiring>, DispatcherFiringsError> {
            let row = sqlx::query(
                "SELECT firing_id, fired_on, fired_at FROM dispatcher_firings \
                 WHERE rule_name = $1 ORDER BY fired_at DESC LIMIT 1",
            )
            .bind(rule)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
            let Some(r) = row else { return Ok(None) };
            Ok(Some(LastFiring {
                firing_id: r.try_get("firing_id").map_err(storage)?,
                fired_on: r.try_get("fired_on").map_err(storage)?,
                fired_at: r.try_get("fired_at").map_err(storage)?,
            }))
        }

        /// `DISTINCT ON (rule_name)` walks the recency index
        /// (`rule_name, fired_at DESC`) and keeps each rule's first row —
        /// one row per rule, however many firings the month holds.
        async fn last_firings(&self) -> Result<Vec<RuleLastFiring>, DispatcherFiringsError> {
            let rows = sqlx::query(
                "SELECT DISTINCT ON (rule_name) rule_name, fired_on, fired_at \
                 FROM dispatcher_firings ORDER BY rule_name, fired_at DESC",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.into_iter()
                .map(|r| {
                    Ok(RuleLastFiring {
                        rule: r.try_get("rule_name").map_err(storage)?,
                        fired_on: r.try_get("fired_on").map_err(storage)?,
                        fired_at: r.try_get("fired_at").map_err(storage)?,
                    })
                })
                .collect()
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgDispatcherFirings;

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, JobStatus, Priority, Subject};
    use serde_json::json;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn firing(rule: &str, at: &str) -> (String, LastFiring) {
        (
            rule.to_string(),
            LastFiring {
                firing_id: format!("dispatcher:{rule}:{at}"),
                fired_on: "step.done.gate-verdict".into(),
                fired_at: t(at),
            },
        )
    }

    fn packet(metadata: Value) -> Job {
        Job {
            id: JobId::new(),
            kind: "maintenance-sweep".into(),
            workflow_version: 1,
            subject: Subject::new("custom", "s"),
            title: "a packet".into(),
            owner_id: "emp-david".into(),
            status: JobStatus::Open,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
            opened_at: None,
            due_on: None,
            closed_on: None,
            metadata,
            tags: vec![],
            partition: boss_core::partition::Partition::Real,
        }
    }

    fn dead_letter(rule: &str, failures: &[&str], recorded_at: &str) -> Value {
        json!({ DEAD_LETTER_KEY: {
            "rule": rule,
            "handler": "h",
            "error": "503",
            "failures": failures,
            "attempts": 8,
            "class": "budget-exhausted",
            "recorded_at": recorded_at,
        }})
    }

    /// The rules list's read: one entry per rule, the newest, in name
    /// order — never every firing.
    #[tokio::test]
    async fn every_rule_answers_its_newest_firing_once() {
        let repo = InMemoryDispatcherFirings::new(vec![
            firing("auto-park-on-gate-green", "2026-09-24T09:00:00Z"),
            firing("auto-park-on-gate-green", "2026-09-24T11:00:00Z"),
            firing("auto-park-on-gate-green", "2026-09-24T10:00:00Z"),
            firing("complete-marker-on-step-ready", "2026-09-23T08:00:00Z"),
        ]);
        let all = repo.last_firings().await.unwrap();
        assert_eq!(
            all.iter().map(|f| f.rule.as_str()).collect::<Vec<_>>(),
            ["auto-park-on-gate-green", "complete-marker-on-step-ready"]
        );
        assert_eq!(all[0].fired_at, t("2026-09-24T11:00:00Z"));
        assert_eq!(all[1].fired_at, t("2026-09-23T08:00:00Z"));
        assert!(
            InMemoryDispatcherFirings::new(vec![])
                .last_firings()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_dead_letter_names_its_hoisted_rule_and_every_listed_one() {
        let one = dead_letter("r-a", &["r-a/h.a: GET /api/jobs returned 503"], "x");
        assert_eq!(dead_letter_rules(&one[DEAD_LETTER_KEY]), ["r-a"]);
        let two = dead_letter("r-a", &["r-a/h.a: 503", "r-b/h.b: 422 bad/path"], "x");
        assert_eq!(dead_letter_rules(&two[DEAD_LETTER_KEY]), ["r-a", "r-b"]);
        // An annotation with no readable rule names nothing, rather than
        // inventing an empty-named rule.
        assert!(dead_letter_rules(&json!({ "failures": ["no slash here"] })).is_empty());
        assert!(dead_letter_rules(&json!([])).is_empty());
    }

    /// The rollup a rules row reads: how many packets, the newest, and
    /// where it is — inside the firing record's own window.
    #[test]
    fn dead_letters_roll_up_per_rule_inside_the_window() {
        let since = t("2026-08-25T00:00:00Z");
        let newest = packet(dead_letter(
            "r-a",
            &["r-a/h: 503"],
            "2026-09-24T10:00:00+00:00",
        ));
        let jobs = vec![
            packet(dead_letter("r-a", &["r-a/h: 503"], "2026-09-20T10:00:00Z")),
            newest.clone(),
            // Both rules failed on one event.
            packet(dead_letter(
                "r-b",
                &["r-b/h: 422", "r-a/h: 503"],
                "2026-09-22T10:00:00Z",
            )),
            // Older than the window: the firing record has pruned that
            // month, so this column must not reach further back.
            packet(dead_letter("r-a", &["r-a/h: 503"], "2026-07-01T00:00:00Z")),
            // Undatable: counted, never dated.
            packet(dead_letter("r-c", &["r-c/h: 503"], "yesterday")),
            packet(json!({ "channel": "monitoring" })),
        ];
        let roll = dead_letter_rollup(&jobs, since);
        assert_eq!(
            roll.iter()
                .map(|r| (r.rule.as_str(), r.packets))
                .collect::<Vec<_>>(),
            [("r-a", 3), ("r-b", 1), ("r-c", 1)]
        );
        assert_eq!(roll[0].newest_at, Some(t("2026-09-24T10:00:00Z")));
        assert_eq!(roll[0].newest_job_id, Some(newest.id.to_string()));
        assert_eq!(roll[2].newest_at, None);
        assert_eq!(roll[2].newest_job_id, None);
        assert!(dead_letter_rollup(&[], since).is_empty());
    }
}
