//! `jobs.flight_overdue` — a flight left undecided past its observe
//! period, or decided and never cleaned out of the code, files an alarm
//! (design c4c2a607 §4, backlog 73c31776 car 1). Flags cannot rot.
//!
//! A flight is an open packet carrying a `flight` block in its job
//! metadata (`boss_jobs::flights`). Two ways it rots, each an alarm:
//!
//!   - PAST ITS PERIOD: turned on more than its observe period plus
//!     `grace_days` ago and still no verdict. The period runs from the
//!     newest completion among the rule's `started_by` steps, and is that
//!     step's own `observe_days` when it names one (an extension) or the
//!     packet's `flight.observe_days` otherwise.
//!   - DECIDED BUT NOT CLEANED UP: a `decided_by` step completed more
//!     than `cleanup_days` ago and no `cleaned_by` step has — the fork is
//!     still in the source, which is exactly what a flag rotting is.
//!
//! THE STEP NAMES RIDE THE RULE ROW (`started_by`, `decided_by`,
//! `cleaned_by`), so nothing here names the protocol: the read is every
//! open packet carrying the block, whatever its kind, and a reshaped
//! protocol is a rule edit, not a deploy — the idiom of
//! `jobs.agent_step_overdue`'s `hours.<workflow>`.
//!
//! A TIMER OVER A THRESHOLD, the dispatcher's standing exemption: no
//! event says "nobody decided". The alarm names the flight, its owner,
//! which case, the instant the clock started and how many days over —
//! the cause, not only the symptom.
//!
//! ONE ALARM PER FLIGHT PER CASE, EVER: the dedup read takes every
//! status, so a person's close is final and no tick nags. An open alarm
//! whose flight no longer rots (decided, cleaned up, or its packet
//! closed) is withdrawn at the step the alarm waits on. A flight whose
//! clock cannot be read (no stamp, no period) is left alone and counted
//! in the journal — "I cannot tell" is not "late".
//!
//! AN ALARM, NOT A LIMIT. Nothing here touches the flight's packet.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, jobs_where, owner_for_filing, post_json,
    recovery_note, retraction, with_lane, withdrawal_fields, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "jobs.flight_overdue";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "stale-flight";

/// The alarm-metadata key holding `<flight packet id>:<case>` — the
/// dedup key.
pub const STALE_KEY: &str = "stale_flight";

/// The key the schedule runner writes the firing instant under.
const TICK_AT: &str = "_at";

/// Which way a flight rots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    PastPeriod,
    NotCleanedUp,
}

impl Case {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PastPeriod => "past-period",
            Self::NotCleanedUp => "not-cleaned-up",
        }
    }
}

/// What the rule row declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub started_by: Vec<String>,
    pub decided_by: Vec<String>,
    pub cleaned_by: Vec<String>,
    pub grace_days: i64,
    pub cleanup_days: i64,
}

fn list_arg(args: &[(String, Value)], name: &str) -> Result<Vec<String>, String> {
    match arg(args, name) {
        Some(Value::String(s)) => {
            let out: Vec<String> = s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if out.is_empty() {
                Err(format!("`{name}` names no step"))
            } else {
                Ok(out)
            }
        }
        Some(other) => Err(format!(
            "`{name}` must be a comma-separated list of step slugs, got a {}",
            other.kind()
        )),
        None => Err(format!(
            "`{name}` is missing — it names the steps this watch reads"
        )),
    }
}

fn days_arg(args: &[(String, Value)], name: &str) -> Result<i64, String> {
    match arg(args, name) {
        Some(Value::Int(n)) if *n > 0 => Ok(*n),
        Some(other) => Err(format!(
            "`{name}` must be a positive whole number of days, got {other:?}"
        )),
        None => Err(format!("`{name}` is missing")),
    }
}

/// The rule's declaration, or a refusal naming the arg.
pub fn declared(args: &[(String, Value)]) -> Result<Declared, String> {
    Ok(Declared {
        started_by: list_arg(args, "started_by")?,
        decided_by: list_arg(args, "decided_by")?,
        cleaned_by: list_arg(args, "cleaned_by")?,
        grace_days: days_arg(args, "grace_days")?,
        cleanup_days: days_arg(args, "cleanup_days")?,
    })
}

/// One rotting flight.
#[derive(Debug, Clone, PartialEq)]
pub struct Stale {
    pub packet: String,
    pub code: String,
    pub owner: String,
    pub case: Case,
    /// The instant the clock started: the period's start, or the verdict.
    pub since: DateTime<Utc>,
    pub due: DateTime<Utc>,
    pub days_over: f64,
}

impl Stale {
    pub fn key(&self) -> String {
        format!("{}:{}", self.packet, self.case.as_str())
    }
}

fn completed_at(step: &Json) -> Option<DateTime<Utc>> {
    if step.get("status").and_then(Json::as_str) != Some("completed") {
        return None;
    }
    step.get("completed_at")
        .and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// A whole number of days, from a number or a numeric string.
fn days(v: Option<&Json>) -> Option<f64> {
    let d = match v? {
        Json::Number(n) => n.as_f64()?,
        Json::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (d.is_finite() && d > 0.0).then_some(d)
}

/// The newest completion among `slugs` on `job`, with its step.
fn newest<'a>(job: &'a Json, slugs: &[String]) -> Option<(DateTime<Utc>, &'a Json)> {
    job.get("steps")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("spec_slug")
                .and_then(Json::as_str)
                .is_some_and(|slug| slugs.iter().any(|w| w == slug))
        })
        .filter_map(|s| completed_at(s).map(|t| (t, s)))
        .max_by_key(|(t, _)| *t)
}

/// Is this flight rotting, and how? `Err` when its clock cannot be read
/// — a started flight with no readable period. Pure.
pub fn judge(job: &Json, d: &Declared, now: DateTime<Utc>) -> Result<Option<Stale>, String> {
    let flight = job.pointer("/metadata/flight");
    let text = |p: &str| {
        flight
            .and_then(|f| f.pointer(p))
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string()
    };
    let packet = job
        .get("id")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let stale = |case, since: DateTime<Utc>, due: DateTime<Utc>| Stale {
        packet: packet.clone(),
        code: text("/code"),
        owner: text("/owner"),
        case,
        since,
        due,
        days_over: (((now - due).num_seconds() as f64 / 86_400.0) * 10.0).round() / 10.0,
    };
    if newest(job, &d.cleaned_by).is_some() {
        return Ok(None);
    }
    if let Some((decided, _)) = newest(job, &d.decided_by) {
        let due = decided + Duration::days(d.cleanup_days);
        return Ok((now > due).then(|| stale(Case::NotCleanedUp, decided, due)));
    }
    let Some((started, step)) = newest(job, &d.started_by) else {
        return Ok(None);
    };
    let period = days(step.pointer("/metadata/observe_days"))
        .or_else(|| days(flight.and_then(|f| f.get("observe_days"))))
        .ok_or_else(|| {
            format!("flight packet {packet} is on but names no readable observe_days")
        })?;
    let due =
        started + Duration::seconds((period * 86_400.0) as i64) + Duration::days(d.grace_days);
    Ok((now > due).then(|| stale(Case::PastPeriod, started, due)))
}

/// The alarm one rotting flight files.
pub fn alarm_body(s: &Stale, owner: &str, now: DateTime<Utc>, rule: &str) -> Json {
    let id8 = &s.packet[..s.packet.len().min(8)];
    let (what, ask) = match s.case {
        Case::PastPeriod => (
            format!(
                "was turned on (its observe period started) at {since} and has had no verdict \
                 since; it is {over} days past its period plus the grace this rule declares",
                since = s.since.to_rfc3339(),
                over = s.days_over,
            ),
            "Record the reading and decide it — promote or pull — or pull it now: an agent may \
             pull at any time, and off is the safe direction.",
        ),
        Case::NotCleanedUp => (
            format!(
                "was decided at {since} and its cleanup has not landed; it is {over} days past \
                 the bound this rule declares",
                since = s.since.to_rfc3339(),
                over = s.days_over,
            ),
            "Land the cleanup car that deletes the fork, with its git-grep probe, and complete \
             the cleanup step. Until then the flag is still in the source.",
        ),
    };
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, design c4c2a607) at {now}: flight `{code}` (packet \
         {packet}, owner {flight_owner}) {what}. {ask} This alarm refuses nothing, and it is \
         withdrawn once the flight no longer rots.",
        now = now.to_rfc3339(),
        code = s.code,
        packet = s.packet,
        flight_owner = s.owner,
    );
    let metadata = json!({
        "area": "platform",
        "scope": SCOPE,
        STALE_KEY: s.key(),
        "flight_packet": s.packet,
        "flight_code": s.code,
        "flight_owner": s.owner,
        "case": s.case.as_str(),
        "since": s.since.to_rfc3339(),
        "due": s.due.to_rfc3339(),
        "days_over": s.days_over,
        "measured_at": now.to_rfc3339(),
        "detail": detail,
    });
    let title = match s.case {
        Case::PastPeriod => format!(
            "STALE FLIGHT: `{}` is {} days past its observe period with no verdict — packet {id8}",
            s.code, s.days_over
        ),
        Case::NotCleanedUp => format!(
            "STALE FLIGHT: `{}` was decided and is {} days past its cleanup bound — packet {id8}",
            s.code, s.days_over
        ),
    };
    json!({
        "kind": "backlog-item",
        "title": title,
        "subject": {"subject_kind": "custom", "id": s.packet},
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// Every alarm this handler has filed, keyed by `<packet>:<case>`.
pub fn alarms_by_key(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter_map(|row| {
            let key = row
                .pointer(&format!("/metadata/{STALE_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((key.to_string(), row.clone()))
        })
        .collect()
}

pub struct JobsFlightOverdue {
    client: reqwest::Client,
    jobs_base: String,
    /// Who the alarms are owned by — the platform owner through the port;
    /// never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsFlightOverdue {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn withdraw(
        &self,
        key: &str,
        alarm: &Json,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        let evidence = format!(
            "{HANDLER} read the open flights at {}: {key} no longer rots (decided, cleaned up, \
             or its packet closed).",
            now.to_rfc3339()
        );
        match retraction(alarm) {
            None => {
                tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
                Ok(false)
            }
            // The fields through the step merge door, then the flip
            // (e39a9d2a).
            Some(Retraction::Complete { step_id, .. }) => {
                complete_step(
                    &self.client,
                    self.base(),
                    id,
                    &step_id,
                    withdrawal_fields(&evidence, HANDLER, &now.to_rfc3339()),
                    rule,
                )
                .await?;
                Ok(true)
            }
            Some(Retraction::Annotate { .. })
                if alarm
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_some_and(|v| !v.is_null()) =>
            {
                Ok(false)
            }
            Some(Retraction::Annotate { why_open }) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.base()),
                    &Json::Object(recovery_note(
                        &evidence,
                        HANDLER,
                        &now.to_rfc3339(),
                        &why_open,
                    )),
                    rule,
                )
                .await?;
                Ok(false)
            }
        }
    }
}

#[async_trait]
impl Handler for JobsFlightOverdue {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let d = declared(args).map_err(HandlerError::Permanent)?;
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} needs a sub-day cadence \
                     (hourly, every-<n>-minutes)"
                ))
            })?;

        // Every open flight, whatever its kind — the same generic read as
        // `GET /api/flights/mine`.
        let flights = jobs_where(
            &self.client,
            self.base(),
            "status=open&metadata_has=flight",
            rule,
        )
        .await?;
        let mut stale: Vec<Stale> = Vec::new();
        let mut unreadable = 0usize;
        for job in &flights {
            match judge(job, &d, now) {
                Ok(Some(s)) => stale.push(s),
                Ok(None) => {}
                Err(why) => {
                    unreadable += 1;
                    tracing::warn!(
                        rule,
                        "{HANDLER}: {why} — its clock cannot be read, leaving it alone"
                    );
                }
            }
        }

        // EVERY status: a flight alarmed once for a case is never alarmed
        // again for it.
        let filed = alarms_by_key(
            &jobs_where(
                &self.client,
                self.base(),
                &format!("kind=backlog-item&metadata_has={STALE_KEY}"),
                rule,
            )
            .await?,
        );

        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let mut raised = 0usize;
        for s in &stale {
            if filed.contains_key(&s.key()) {
                continue;
            }
            post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &alarm_body(s, &owner, now, rule),
                rule,
            )
            .await?;
            raised += 1;
        }

        let still: BTreeSet<String> = stale.iter().map(Stale::key).collect();
        let mut withdrawn = 0usize;
        for (key, alarm) in &filed {
            let open = alarm.get("status").and_then(Json::as_str) == Some("open");
            if !open || still.contains(key) {
                continue;
            }
            if self.withdraw(key, alarm, now, rule).await? {
                withdrawn += 1;
            }
        }
        tracing::info!(
            rule,
            flights = flights.len(),
            stale = stale.len(),
            unreadable,
            raised,
            withdrawn,
            "{HANDLER}: read the open flights"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-10-20T12:00:00+00:00";

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("started_by".into(), Value::String("turn-on,extend".into())),
            ("decided_by".into(), Value::String("decide,pull".into())),
            ("cleaned_by".into(), Value::String("cleanup".into())),
            ("grace_days".into(), Value::Int(7)),
            ("cleanup_days".into(), Value::Int(14)),
        ]
    }

    fn d() -> Declared {
        declared(&args()).unwrap()
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "a-flight-past-its-period-is-an-alarm".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-10-20", "_at": NOW}),
        }
    }

    fn step(slug: &str, completed_at: Option<&str>, md: Json) -> Json {
        match completed_at {
            Some(t) => json!({"id": format!("s-{slug}"), "spec_slug": slug, "status": "completed",
                              "completed_at": t, "metadata": md}),
            None => json!({"id": format!("s-{slug}"), "spec_slug": slug, "status": "ready",
                           "metadata": md}),
        }
    }

    /// The design's first flight: 7 days observed, David first.
    fn flight(id: &str, steps: Vec<Json>) -> Json {
        json!({
            "id": id, "kind": "flight-a-change", "status": "open",
            "metadata": {"flight": {"code": "it-map-motion", "owner": "agent-claude",
                                    "observe_days": 7, "audience": {"actors": ["emp-david"]}}},
            "steps": steps,
        })
    }

    #[test]
    fn a_declaration_names_its_steps_and_its_bounds() {
        assert_eq!(d().started_by, vec!["turn-on", "extend"]);
        for (name, bad) in [
            ("started_by", Value::String(" , ".into())),
            ("decided_by", Value::Int(3)),
            ("grace_days", Value::Int(0)),
            ("cleanup_days", Value::String("14".into())),
        ] {
            let mut a = args();
            a.retain(|(k, _)| k != name);
            a.push((name.into(), bad));
            let err = declared(&a).unwrap_err();
            assert!(err.contains(name), "{name}: {err}");
        }
        let mut missing = args();
        missing.retain(|(k, _)| k != "cleaned_by");
        assert!(declared(&missing).unwrap_err().contains("cleaned_by"));
    }

    /// Turned on 2026-10-01: 7 days + 7 grace is due 10-15, so on 10-20
    /// it is 5 days past its period with no verdict.
    #[test]
    fn a_flight_on_past_its_period_plus_grace_with_no_verdict_is_stale() {
        let f = flight(
            "f1",
            vec![
                step("turn-on", Some("2026-10-01T12:00:00Z"), json!({})),
                step("decide", None, json!({})),
            ],
        );
        let s = judge(&f, &d(), at(NOW)).unwrap().expect("stale");
        assert_eq!(s.case, Case::PastPeriod);
        assert_eq!(s.days_over, 5.0);
        assert_eq!(s.code, "it-map-motion");
        assert_eq!(s.owner, "agent-claude");
        assert_eq!(s.key(), "f1:past-period");
    }

    #[test]
    fn a_flight_inside_its_period_or_never_turned_on_is_not() {
        let fresh = flight(
            "f",
            vec![step("turn-on", Some("2026-10-10T12:00:00Z"), json!({}))],
        );
        assert_eq!(judge(&fresh, &d(), at(NOW)).unwrap(), None);
        let unshipped = flight("f", vec![step("turn-on", None, json!({}))]);
        assert_eq!(judge(&unshipped, &d(), at(NOW)).unwrap(), None);
    }

    /// An extension restarts the clock with the period it names.
    #[test]
    fn an_extension_restarts_the_period_with_its_own_days() {
        let f = flight(
            "f",
            vec![
                step("turn-on", Some("2026-09-01T12:00:00Z"), json!({})),
                step(
                    "extend",
                    Some("2026-10-01T12:00:00Z"),
                    json!({"observe_days": "14"}),
                ),
            ],
        );
        assert_eq!(judge(&f, &d(), at(NOW)).unwrap(), None, "due 10-22");
    }

    #[test]
    fn decided_and_not_cleaned_up_after_the_bound_is_stale_and_cleaned_up_is_not() {
        let decided = flight(
            "f2",
            vec![
                step("turn-on", Some("2026-09-01T12:00:00Z"), json!({})),
                step("pull", Some("2026-10-01T12:00:00Z"), json!({})),
                step("cleanup", None, json!({})),
            ],
        );
        let s = judge(&decided, &d(), at(NOW)).unwrap().expect("stale");
        assert_eq!(s.case, Case::NotCleanedUp);
        assert_eq!(s.days_over, 5.0);
        let cleaned = flight(
            "f3",
            vec![
                step("pull", Some("2026-10-01T12:00:00Z"), json!({})),
                step("cleanup", Some("2026-10-02T12:00:00Z"), json!({})),
            ],
        );
        assert_eq!(judge(&cleaned, &d(), at(NOW)).unwrap(), None);
    }

    #[test]
    fn a_started_flight_with_no_readable_period_is_unreadable_not_late() {
        let mut f = flight(
            "f",
            vec![step("turn-on", Some("2026-01-01T00:00:00Z"), json!({}))],
        );
        f["metadata"]["flight"]["observe_days"] = json!("soon");
        assert!(judge(&f, &d(), at(NOW)).is_err());
    }

    #[test]
    fn the_alarm_names_the_flight_its_owner_the_case_and_the_days_over() {
        let f = flight(
            "f1-abcdef-123",
            vec![step("turn-on", Some("2026-10-01T12:00:00Z"), json!({}))],
        );
        let s = judge(&f, &d(), at(NOW)).unwrap().unwrap();
        let body = alarm_body(&s, "emp-owner", at(NOW), "r");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[STALE_KEY], "f1-abcdef-123:past-period");
        assert_eq!(m["flight_code"], "it-map-motion");
        assert_eq!(m["flight_owner"], "agent-claude");
        assert_eq!(m["case"], "past-period");
        assert_eq!(m["scope"], SCOPE);
        let title = body["title"].as_str().unwrap();
        assert!(title.contains("`it-map-motion` is 5 days past"), "{title}");
        assert!(m["detail"].as_str().unwrap().contains("pull at any time"));
    }

    // -- end to end, against the stub jobs API ---------------------------

    const FLIGHTS: &str = "/api/jobs?status=open&metadata_has=flight";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&metadata_has=stale_flight";

    fn handler(stub: &listing_stub::Stub) -> Arc<JobsFlightOverdue> {
        JobsFlightOverdue::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn late() -> Json {
        flight(
            "f1",
            vec![step("turn-on", Some("2026-10-01T12:00:00Z"), json!({}))],
        )
    }

    fn alarm_row(key: &str, status: &str, triage: &str) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": status,
            "metadata": {"scope": SCOPE, STALE_KEY: key},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": triage,
                       "metadata": {"authority_role": "platform-admin"}}],
        })
    }

    #[tokio::test]
    async fn a_stale_flight_files_one_alarm() {
        let stub = listing_stub::serve(vec![
            (FLIGHTS, listing(vec![late()])),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"][STALE_KEY], "f1:past-period");
    }

    #[tokio::test]
    async fn a_flight_already_alarmed_is_not_alarmed_again_in_any_status() {
        for (status, triage) in [("open", "ready"), ("closed", "completed")] {
            let stub = listing_stub::serve(vec![
                (FLIGHTS, listing(vec![late()])),
                (
                    ALARMS,
                    listing(vec![alarm_row("f1:past-period", status, triage)]),
                ),
            ])
            .await;
            handler(&stub).invoke(&args(), &ctx()).await.unwrap();
            assert!(stub.writes().is_empty(), "{status}: {:?}", stub.writes());
        }
    }

    #[tokio::test]
    async fn a_flight_that_no_longer_rots_withdraws_its_open_alarm() {
        let stub = listing_stub::serve(vec![
            (FLIGHTS, listing(vec![])),
            (
                ALARMS,
                listing(vec![alarm_row("f1:past-period", "open", "ready")]),
            ),
        ])
        .await;
        handler(&stub).invoke(&args(), &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "the merge, then the flip: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/steps/al-1-triage/metadata");
        assert_eq!(sent[0].1["cleared_by"], HANDLER);
        assert!(
            sent[0].1.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them"
        );
        assert_eq!(sent[1].0, "PUT /api/jobs/al-1/steps/al-1-triage");
        assert_eq!(sent[1].1, json!({"status": "completed"}));
    }

    #[tokio::test]
    async fn a_flights_read_with_no_data_array_refuses_by_name() {
        let stub = listing_stub::serve(vec![(FLIGHTS, listing_stub::no_data_array())]).await;
        listing_stub::assert_refused_by_name(
            handler(&stub).invoke(&args(), &ctx()).await,
            "GET /api/jobs",
        );
        assert!(stub.writes().is_empty());
    }

    #[tokio::test]
    async fn a_daily_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-10-20"});
        let err = handler(&stub).invoke(&args(), &c).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at")),
            "{err}"
        );
    }

    /// The shipped rule parses into a declaration, and the step names it
    /// reads are steps the shipped protocol has — a rename on either side
    /// that leaves this watch reading nothing fails here, by name.
    #[test]
    fn the_shipped_rule_reads_steps_the_shipped_protocol_has() {
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        let rule = raw
            .rules
            .iter()
            .find(|r| r.do_steps.iter().any(|d| d.handler == HANDLER))
            .expect("a shipped rule invokes this handler");
        let args: Vec<(String, Value)> = rule
            .do_steps
            .iter()
            .flat_map(|d| d.args.iter())
            .map(|(k, v)| {
                let t = v.trim();
                let val = match t.parse::<i64>() {
                    Ok(n) => Value::Int(n),
                    Err(_) => Value::String(t.trim_matches('"').to_string()),
                };
                (k.clone(), val)
            })
            .collect();
        let d = declared(&args).unwrap_or_else(|e| panic!("{}: {e}", rule.name));
        assert_eq!(
            (d.grace_days, d.cleanup_days),
            (7, 14),
            "the design's bounds"
        );
        let protocol =
            boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
                .expect("the platform bundle parses")
                .into_iter()
                .find(|w| w.kind == "flight-a-change")
                .expect("flight-a-change ships in the platform bundle");
        for slug in d
            .started_by
            .iter()
            .chain(&d.decided_by)
            .chain(&d.cleaned_by)
        {
            assert!(
                protocol.steps.iter().any(|s| &s.title == slug),
                "{} reads step `{slug}`, which flight-a-change does not have",
                rule.name
            );
        }
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsFlightOverdue::new(
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
    }
}
