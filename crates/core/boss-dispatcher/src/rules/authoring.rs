//! Authoring writes for the `dispatcher_rules` registry — the control-plane
//! behind the rule-authoring UX.
//!
//! Mirrors the step_plugins versioned-registry semantics: append-only
//! `(name, version)`, exactly one active per name (the partial unique
//! index), draft → active via [`publish`] which retires the prior active in
//! the same transaction. The read/runtime path is `load_active_rules`
//! (registry.rs); these are the writes.
//!
//! A published change takes effect WITHOUT a restart: `boss_dispatcher`'s
//! main loop watches the registry fingerprint (`rules_changed`), reloads,
//! and rebinds its runners. (This paragraph used to say hot-reload was "a
//! planned follow-up"; it shipped, and the stale sentence read as an
//! argument for restarting the dispatcher after every rule edit.) If the
//! reloaded registry fails to parse, the loop keeps the registry it is
//! already running and logs — a bad edit degrades to "no change", never to
//! "no rules".
//!
//! [`validate`] reuses the runtime `Rule::from_raw`, so a draft that
//! validates here loads cleanly there. It runs on BOTH writes: on
//! [`create_draft`] against the caller's body, and on [`publish`] against
//! the stored row being promoted. The second is not redundant with the
//! first — rows reach this table by paths that never touch `create_draft`
//! (the SQL seeds in `41-dispatcher.sql` and friends), and a draft
//! validated under one build can be published under a later one. Publish
//! is the edge that puts a row into service, so publish is where the
//! question has to be asked.
//!
//! What `Rule::from_raw` does NOT check: handler NAMES. Those pass through
//! as opaque strings and resolve at dispatch, where an unknown one is a
//! loud `DispatchError::UnknownHandler` rather than a silent no-op.

use serde::Serialize;
use sqlx::{PgPool, Row};

use super::registry::{Cadence, RawDoStep, RawRule, RawSchedule, RegistryError, Rule};

/// One stored `dispatcher_rules` row: the rule content + its lifecycle.
#[derive(Debug, Clone, Serialize)]
pub struct RuleVersion {
    pub name: String,
    pub version: i32,
    pub status: String,
    /// `None` for a schedule-triggered rule (mutually exclusive with
    /// `schedule`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_event: Option<String>,
    /// `None` for an event-triggered rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<RawSchedule>,
    pub when: Option<String>,
    // Serialize as `do` to match the cascade-viz feed (RawRule) + the SPA's
    // DispatcherRuleDo type — one rule-content shape across the API.
    #[serde(rename = "do")]
    pub do_steps: Vec<RawDoStep>,
    pub delay: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl RuleVersion {
    /// This stored row as the authoring shape [`validate`] reads.
    ///
    /// Pure and total, so the publish gate can ask "would this row load?"
    /// about a row that is already in the table — not just about a body a
    /// caller handed us. `version` is `u32` on the wire and `i32` in the
    /// column; a negative version cannot exist (versions are assigned by
    /// `MAX(version)+1` starting at 1) and is clamped rather than wrapped,
    /// because silently turning -1 into 4294967295 is the kind of quiet
    /// nonsense this whole gate exists to refuse.
    pub fn to_raw(&self) -> RawRule {
        RawRule {
            name: self.name.clone(),
            on_event: self.on_event.clone(),
            schedule: self.schedule.clone(),
            when: self.when.clone(),
            do_steps: self.do_steps.clone(),
            delay: self.delay.clone(),
            version: self.version.max(0) as u32,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthoringError {
    #[error("not found: {0}")]
    NotFound(String),
    /// The row is well-formed storage but would not load as a `Rule` —
    /// a `when` or arg expression that will not parse, an unparseable
    /// topic pattern, or a broken on_event/schedule XOR. (NOT an unknown
    /// handler: handler names are opaque to `Rule::from_raw` and resolve
    /// at dispatch.) Distinct from `Invalid` (a caller's bad body)
    /// because this one is about a row already in the table, and it
    /// leaves as 422 rather than 400.
    #[error("rule would not load: {0}")]
    Unviable(String),
    #[error("invalid rule: {0}")]
    Invalid(String),
    #[error("storage: {0}")]
    Storage(String),
}

fn store<E: std::fmt::Display>(e: E) -> AuthoringError {
    AuthoringError::Storage(e.to_string())
}

const SELECT_COLS: &str = "name, version, status, on_event, when_expr, do_steps, delay, \
     schedule_cadence, schedule_anchor, schedule_calendar, created_at";

/// Parse a draft through the SAME `Rule::from_raw` the runtime uses, so an
/// authoring error (bad topic / `when` / arg expr) surfaces before persist.
/// Pure — no I/O.
pub fn validate(raw: &RawRule) -> Result<(), RegistryError> {
    Rule::from_raw(raw.clone()).map(|_| ())
}

fn row_to_version(row: &sqlx::postgres::PgRow) -> Result<RuleVersion, AuthoringError> {
    let do_json: serde_json::Value = row.try_get("do_steps").map_err(store)?;
    let do_steps: Vec<RawDoStep> = serde_json::from_value(do_json)
        .map_err(|e| AuthoringError::Storage(format!("do_steps: {e}")))?;
    // Reassemble the schedule from its columns (both NULL for an event rule).
    let cadence: Option<String> = row.try_get("schedule_cadence").map_err(store)?;
    let anchor: Option<chrono::NaiveDate> = row.try_get("schedule_anchor").map_err(store)?;
    let calendar: Option<String> = row.try_get("schedule_calendar").map_err(store)?;
    let schedule = match (cadence, anchor) {
        (Some(c), Some(anchor_date)) => {
            let cadence = Cadence::parse(&c).ok_or_else(|| {
                AuthoringError::Storage(format!("unknown schedule_cadence {c:?}"))
            })?;
            Some(RawSchedule {
                cadence,
                anchor_date,
                business_calendar: calendar,
            })
        }
        (None, None) => None,
        _ => {
            return Err(AuthoringError::Storage(
                "schedule_cadence and schedule_anchor must both be set or both NULL".into(),
            ));
        }
    };
    Ok(RuleVersion {
        name: row.try_get("name").map_err(store)?,
        version: row.try_get("version").map_err(store)?,
        status: row.try_get("status").map_err(store)?,
        on_event: row.try_get("on_event").map_err(store)?,
        schedule,
        when: row.try_get("when_expr").map_err(store)?,
        do_steps,
        delay: row.try_get("delay").map_err(store)?,
        created_at: row.try_get("created_at").map_err(store)?,
    })
}

/// All versions of a rule name, oldest first (draft + active + retired).
pub async fn list_versions(pool: &PgPool, name: &str) -> Result<Vec<RuleVersion>, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 ORDER BY version");
    let rows = sqlx::query(&sql)
        .bind(name)
        .fetch_all(pool)
        .await
        .map_err(store)?;
    rows.iter().map(row_to_version).collect()
}

/// A specific version.
pub async fn get_version(
    pool: &PgPool,
    name: &str,
    version: i32,
) -> Result<RuleVersion, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 AND version = $2");
    let row = sqlx::query(&sql)
        .bind(name)
        .bind(version)
        .fetch_optional(pool)
        .await
        .map_err(store)?
        .ok_or_else(|| AuthoringError::NotFound(format!("{name} v{version}")))?;
    row_to_version(&row)
}

/// The active version of a rule, or `NotFound` if none is active.
pub async fn get_active(pool: &PgPool, name: &str) -> Result<RuleVersion, AuthoringError> {
    let sql =
        format!("SELECT {SELECT_COLS} FROM dispatcher_rules WHERE name = $1 AND status = 'active'");
    let row = sqlx::query(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(store)?
        .ok_or_else(|| AuthoringError::NotFound(format!("no active {name}")))?;
    row_to_version(&row)
}

/// Append a new draft version of `raw.name`. Validates first (a draft that
/// can't load is rejected with `Invalid`, no row written), assigns
/// `max(version) + 1`, status = `draft`.
pub async fn create_draft(pool: &PgPool, raw: &RawRule) -> Result<RuleVersion, AuthoringError> {
    validate(raw).map_err(|e| AuthoringError::Invalid(e.to_string()))?;
    let do_json = serde_json::to_value(&raw.do_steps).map_err(store)?;
    let mut tx = pool.begin().await.map_err(store)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM dispatcher_rules WHERE name = $1",
    )
    .bind(&raw.name)
    .fetch_one(&mut *tx)
    .await
    .map_err(store)?;
    // Decompose the schedule into its columns (all NULL for an event rule).
    let sched_cadence = raw.schedule.as_ref().map(|s| s.cadence.token());
    let sched_anchor = raw.schedule.as_ref().map(|s| s.anchor_date);
    let sched_calendar = raw
        .schedule
        .as_ref()
        .and_then(|s| s.business_calendar.clone());
    sqlx::query(
        "INSERT INTO dispatcher_rules \
            (name, version, status, on_event, when_expr, do_steps, delay, \
             schedule_cadence, schedule_anchor, schedule_calendar) \
         VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(&raw.name)
    .bind(next)
    .bind(&raw.on_event)
    .bind(&raw.when)
    .bind(&do_json)
    .bind(&raw.delay)
    .bind(sched_cadence)
    .bind(sched_anchor)
    .bind(sched_calendar)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    tx.commit().await.map_err(store)?;
    get_version(pool, &raw.name, next).await
}

/// Activate the latest draft of `name`, retiring the prior active in the
/// same tx (so the one-active-per-name index never trips mid-flight).
pub async fn publish(pool: &PgPool, name: &str) -> Result<RuleVersion, AuthoringError> {
    let mut tx = pool.begin().await.map_err(store)?;
    // Read the WHOLE draft row, not just its version: the gate below has to
    // see the rule content, and it has to see the content of the row this
    // transaction actually promotes — a copy re-read outside the tx could
    // race a concurrent author.
    let sql = format!(
        "SELECT {SELECT_COLS} FROM dispatcher_rules \
         WHERE name = $1 AND status = 'draft' ORDER BY version DESC LIMIT 1"
    );
    let draft = sqlx::query(&sql)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store)?;
    let Some(row) = draft else {
        return Err(AuthoringError::NotFound(format!(
            "no draft to publish for {name}"
        )));
    };
    let promoted = row_to_version(&row)?;
    let v = promoted.version;

    // The viability gate. `create_draft` validates what a caller HANDS us,
    // which is not the same question as whether the row is still loadable
    // now: a draft saved against one handler set can be published after a
    // deploy removed or renamed the handler it names. Without this, publish
    // moves such a row into the ACTIVE slot and the failure lands at the
    // dispatcher's next cold start, where a registry that will not parse
    // means the rules runner never starts and EVERY rule stops firing.
    // `?` here rolls the transaction back untouched, so the incumbent keeps
    // serving — a refused publish is not an outage.
    validate(&promoted.to_raw()).map_err(|e| AuthoringError::Unviable(e.to_string()))?;

    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .execute(&mut *tx)
    .await
    .map_err(store)?;
    sqlx::query("UPDATE dispatcher_rules SET status = 'active' WHERE name = $1 AND version = $2")
        .bind(name)
        .bind(v)
        .execute(&mut *tx)
        .await
        .map_err(store)?;
    tx.commit().await.map_err(store)?;
    get_version(pool, name, v).await
}

/// Retire the active version of `name` (idempotent — no-op if none active).
pub async fn retire(pool: &PgPool, name: &str) -> Result<(), AuthoringError> {
    sqlx::query(
        "UPDATE dispatcher_rules SET status = 'retired' WHERE name = $1 AND status = 'active'",
    )
    .bind(name)
    .execute(pool)
    .await
    .map_err(store)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn version(name: &str, on_event: Option<&str>, handler: &str) -> RuleVersion {
        RuleVersion {
            name: name.into(),
            version: 3,
            status: "draft".into(),
            on_event: on_event.map(str::to_string),
            schedule: None,
            when: None,
            do_steps: vec![RawDoStep {
                handler: handler.into(),
                args: Default::default(),
            }],
            delay: None,
            created_at: chrono::Utc.with_ymd_and_hms(2026, 8, 13, 0, 0, 0).unwrap(),
        }
    }

    /// The round trip the publish gate depends on: a stored row must
    /// reproduce the authoring shape faithfully, or the gate would be
    /// asking about a different rule than the one it promotes.
    #[test]
    fn a_stored_row_converts_back_to_the_shape_the_validator_reads() {
        let v = version("r", Some("jobs.step.done"), "assign_step");
        let raw = v.to_raw();
        assert_eq!(raw.name, "r");
        assert_eq!(raw.on_event.as_deref(), Some("jobs.step.done"));
        assert_eq!(raw.version, 3);
        assert_eq!(raw.do_steps.len(), 1);
        assert_eq!(raw.do_steps[0].handler, "assign_step");
        assert!(raw.schedule.is_none());
    }

    #[test]
    fn a_schedule_rule_keeps_its_trigger_through_the_conversion() {
        let mut v = version("nightly", None, "assign_step");
        v.schedule = Some(RawSchedule {
            cadence: Cadence::parse("daily").unwrap(),
            anchor_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            business_calendar: Some("us-banking".into()),
        });
        let raw = v.to_raw();
        assert!(
            raw.on_event.is_none(),
            "the XOR must survive the round trip"
        );
        let s = raw.schedule.expect("schedule preserved");
        assert_eq!(s.business_calendar.as_deref(), Some("us-banking"));
    }

    /// The gate is `validate(row.to_raw())`. Proving the pieces compose
    /// here means the only thing the DB-backed test has to prove is that
    /// publish CALLS it.
    #[test]
    fn the_gate_accepts_a_loadable_row_and_refuses_an_unparseable_expression() {
        let good = version("good", Some("jobs.step.done"), "assign_step");
        assert!(validate(&good.to_raw()).is_ok());

        let mut bad = version("bad", Some("jobs.step.done"), "assign_step");
        bad.when = Some("this is not (a parseable expression".into());
        let err = validate(&bad.to_raw()).expect_err("a broken `when` must be refused");
        assert!(err.to_string().contains("bad"), "names the rule: {err}");
    }

    /// What the gate does NOT cover, pinned so the limit is documented
    /// rather than assumed. `Rule::from_raw` checks the trigger XOR, the
    /// topic pattern, and every expression — it passes `handler` through
    /// as an opaque string. An unknown handler is therefore publishable,
    /// and surfaces at dispatch time instead. Tracked separately; if a
    /// handler-name check ever lands, this test is what tells you to
    /// delete it.
    #[test]
    fn an_unknown_handler_is_not_caught_by_this_gate() {
        let bad = version("bad", Some("jobs.step.done"), "handler_that_does_not_exist");
        assert!(
            validate(&bad.to_raw()).is_ok(),
            "handler names are not validated at authoring time — if this now \
             fails, the check landed and this test should be replaced by its \
             positive form"
        );
    }

    /// Neither trigger set is the XOR violation `from_raw` exists to
    /// catch — the shape a hand-written SQL seed can produce.
    #[test]
    fn a_row_with_no_trigger_at_all_is_refused() {
        let orphan = version("orphan", None, "assign_step");
        assert!(validate(&orphan.to_raw()).is_err());
    }
}
