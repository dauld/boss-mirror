//! `jobs.age_out_step` — a clock rule completes an open step on every
//! packet of a kind that has gone SILENT past a bound.
//!
//! The gap this closes (design c87fb59b car 2, backlog 39d0b528): an
//! `agent-run` packet's `building` step is open while a builder works.
//! A builder that dies leaves it open forever — nothing completes it,
//! nothing reads its age, and the run it stood for looks exactly like
//! one still building. "A builder that dies is a packet that ages" was
//! the design's whole reason to make the run a packet, and this is the
//! reader of that age.
//!
//! ## Why this is a handler and not agent-run code
//!
//! Every noun is a rule arg: which `kind`, which `step`, how many
//! `hours` of silence, and what to write (`done_metadata`, the step
//! kind's own required vocabulary — `result = died` for a run). The
//! handler knows "on this tick, for every open packet of `kind` whose
//! `step` is open and whose last movement is older than `hours`,
//! complete `step`" — a shape, not a policy. Point it at a different
//! kind and it ages out a different obligation.
//!
//! Rule shape:
//! ```toml
//! [[rule]]
//! schedule = { cadence = "hourly", anchor_date = "2026-09-18" }
//! [[rule.do]]
//! handler = "jobs.age_out_step"
//! args = { kind = "\"agent-run\"", step = "\"building\"", hours = "\"4\"", done_metadata = '"{\"result\": \"died\"}"' }
//! ```
//!
//! ## What "silent" is measured from
//!
//! The newest `completed_at` across the packet's completed steps — the
//! server stamps it at every flip to `completed` and never
//! client-supplied — because that is the last instant the packet
//! provably MOVED. On a packet with no such stamp (one whose steps all
//! predate the column, or whose only completed step is a trigger born
//! completed without one) the fallback is `metadata.opened_at`, the
//! filing instant the create handler writes. A packet with neither is
//! reported and left alone: "I cannot tell how old this is" is a
//! finding, not a licence to guess.
//!
//! A kind whose life is a HEARTBEAT rather than a chain of completions
//! names the stamp as `since_key` (design 511fa7d4 car 2b, backlog
//! da925366): a `work-session` completes nothing between SessionStart
//! and SessionEnd, and its UserPromptSubmit hook writes
//! `metadata.last_active_at` on every prompt. With `since_key =
//! "last_active_at"` that stamp counts as movement alongside the
//! completions, and the newest of them all is what the silence is
//! measured from — without it a session working for seven hours would
//! be ended six hours after it opened.
//!
//! `now` is the tick's own `_at`, which the schedule runner writes onto
//! every sub-day firing. The handler holds no clock: a rule that fires
//! this on a DAILY cadence gets no `_at` and is refused as permanent —
//! a bound of hours judged once a day is the same silence one level up.
//!
//! ## Idempotence
//!
//! A completed step is not open, so the next tick finds nothing on a
//! packet this one aged out. A tick that dies between two packets
//! leaves the second for the next tick. Nothing is ever written twice.

use super::common::{api_client, complete_step, open_jobs_of_kind};
use super::jobs_complete_linked_step::{is_open, is_unset, step_by_slug, template_arg};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;

/// Default metadata key the measurement lands under on the completed
/// step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "aged_out";

/// The key the schedule runner writes the firing instant under, on
/// every sub-day tick (`schedule_runner::tick`).
const TICK_AT: &str = "_at";

pub struct JobsAgeOutStep {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsAgeOutStep {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local stand-in for jobs-api).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// The last instant this packet provably moved: the newest
/// `completed_at` on any completed step — and, when the rule names a
/// `since_key`, the newest of those and `metadata.<since_key>` (a
/// heartbeat) — else `metadata.opened_at`. `None` when none is
/// readable.
pub(crate) fn last_moved(
    job: &serde_json::Value,
    since_key: Option<&str>,
) -> Option<DateTime<Utc>> {
    let parse = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let metadata = |k: &str| job.get("metadata").and_then(|m| m.get(k)).and_then(parse);
    let newest_completion = job
        .get("steps")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
        .filter_map(|s| s.get("completed_at").and_then(parse))
        .max();
    let heartbeat = since_key.and_then(metadata);
    newest_completion
        .max(heartbeat)
        .or_else(|| metadata("opened_at"))
}

/// Hours of silence, or `None` when the packet's age cannot be read.
pub(crate) fn silent_hours(
    job: &serde_json::Value,
    since_key: Option<&str>,
    now: DateTime<Utc>,
) -> Option<f64> {
    last_moved(job, since_key).map(|since| (now - since).num_seconds() as f64 / 3600.0)
}

/// The `hours` arg as a positive bound. A rule authored with a bound
/// that is not a positive number is permanent — no redelivery fixes it.
fn bound_hours(args: &[(String, Value)]) -> Result<f64, HandlerError> {
    let raw = arg_string(args, "hours")?;
    match raw.trim().parse::<f64>() {
        Ok(h) if h.is_finite() && h > 0.0 => Ok(h),
        _ => Err(HandlerError::Permanent(format!(
            "hours must be a positive number of hours, got {raw:?}"
        ))),
    }
}

#[async_trait]
impl Handler for JobsAgeOutStep {
    fn name(&self) -> &'static str {
        "jobs.age_out_step"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let step_slug = arg_string(args, "step")?;
        let bound = bound_hours(args)?;
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };
        let template = template_arg(args, "done_metadata", &ctx.rule_name);
        let since_key = match arg(args, "since_key") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        };

        // The tick's own instant. Absent on a daily firing, which is a
        // rule-authoring error this handler cannot make good by
        // guessing a clock.
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — jobs.age_out_step judges hours and \
                     needs a sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        let candidates = open_jobs_of_kind(&self.client, self.base(), kind, &ctx.rule_name).await?;
        for job in &candidates {
            let job_id = job.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let Some(step) = step_by_slug(job, step_slug).filter(|s| is_open(s)) else {
                continue;
            };
            let Some(silent) = silent_hours(job, since_key, now) else {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    "`{step_slug}` is open but the packet carries no completed_at and no \
                     opened_at — its age cannot be read, leaving it alone"
                );
                continue;
            };
            if silent < bound {
                continue;
            }
            let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            let existing = step.get("metadata").and_then(|m| m.as_object());
            // The template's vocabulary, ABSENT keys only: a value a
            // person wrote on the step is their record.
            let mut fields: serde_json::Map<String, serde_json::Value> = template
                .iter()
                .flatten()
                .filter(|(k, _)| is_unset(existing.and_then(|m| m.get(*k))))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            fields.insert(
                evidence_key.to_string(),
                json!({
                    "silent_hours": (silent * 100.0).round() / 100.0,
                    "bound_hours": bound,
                    "at": now.to_rfc3339(),
                    "rule": ctx.rule_name,
                }),
            );
            // Those keys through the step merge door, then the status
            // alone (backlog e39a9d2a): this PUT the step's metadata AS
            // READ plus them, which the step PUT refuses once anything
            // wrote the step in between, and refuses outright under the
            // decided end state. The merge door keeps what it is not
            // sent.
            complete_step(
                &self.client,
                self.base(),
                job_id,
                step_id,
                fields,
                &ctx.rule_name,
            )
            .await?;
            tracing::info!(
                rule = %ctx.rule_name,
                packet = %job_id,
                "`{step_slug}` silent {silent:.1}h past the {bound}h bound — aged out"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SILENT: &str = "11111111-1111-1111-1111-111111111111";
    const FRESH: &str = "22222222-2222-2222-2222-222222222222";
    const AGELESS: &str = "33333333-3333-3333-3333-333333333333";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "agent-run-dies-when-building-is-silent".into(),
            triggering_event_id: "tick-1".into(),
            triggering_topic: "schedule".into(),
            event_payload: payload,
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".to_string(), Value::String("agent-run".into())),
            ("step".to_string(), Value::String("building".into())),
            ("hours".to_string(), Value::String("4".into())),
            (
                "done_metadata".to_string(),
                Value::String(r#"{"result": "died"}"#.into()),
            ),
        ]
    }

    /// The tick the schedule runner emits for a sub-day cadence.
    fn tick(at: &str) -> serde_json::Value {
        json!({ "_day": &at[..10], "_at": at })
    }

    /// An open run whose `briefed` completed at `briefed_at` (the
    /// moment the build began) and whose `building` is open.
    fn run(id: &str, briefed_at: Option<&str>, opened_at: Option<&str>) -> serde_json::Value {
        let mut metadata = json!({ "packet": "p", "step": "build" });
        if let Some(o) = opened_at {
            metadata["opened_at"] = json!(o);
        }
        json!({
            "id": id,
            "kind": "agent-run",
            "title": format!("run {id}"),
            "status": "open",
            "metadata": metadata,
            "steps": [
                { "id": format!("{id}-claimed"), "spec_slug": "claimed", "status": "completed",
                  "completed_at": null, "metadata": {} },
                { "id": format!("{id}-briefed"), "spec_slug": "briefed", "status": "completed",
                  "completed_at": briefed_at, "metadata": { "prompt_bytes": "1" } },
                { "id": format!("{id}-building"), "spec_slug": "building", "status": "ready",
                  "metadata": { "authority_role": "platform-admin" } },
                { "id": format!("{id}-reported"), "spec_slug": "reported", "status": "ready",
                  "metadata": {} },
            ],
        })
    }

    type Puts = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Puts) {
        let puts: Puts = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let list_jobs = by_id.clone();
        let put_jobs = by_id.clone();
        let merge_jobs = by_id.clone();
        let put_log = puts.clone();
        let merge_log = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = list_jobs.clone();
                    async move {
                        let rows: Vec<serde_json::Value> = by_id
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{step_id}",
                axum::routing::put(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let puts = put_log.clone();
                        let by_id = put_jobs.clone();
                        async move {
                            // The decided end state (e39a9d2a).
                            if let Some(refused) =
                                super::super::listing_stub::end_state_step_put(&id, &step_id, &body)
                            {
                                return refused;
                            }
                            puts.lock()
                                .unwrap()
                                .push((id.clone(), step_id.clone(), body.clone()));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] == json!(step_id) {
                                        step["status"] = body["status"].clone();
                                    }
                                }
                            }
                            Json(json!({ "ok": true })).into_response()
                        }
                    },
                ),
            )
            // The step merge door: merges into the stored step, and is
            // recorded in order with the PUTs as `<step>/metadata`.
            .route(
                "/api/jobs/{id}/steps/{step_id}/metadata",
                axum::routing::patch(
                    move |Path((id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let puts = merge_log.clone();
                        let by_id = merge_jobs.clone();
                        async move {
                            puts.lock().unwrap().push((
                                id.clone(),
                                format!("{step_id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let Some(steps) =
                                    job.get_mut("steps").and_then(|s| s.as_array_mut())
                            {
                                for step in steps.iter_mut() {
                                    if step["id"] == json!(step_id)
                                        && let (Some(stored), Some(sent)) =
                                            (step["metadata"].as_object_mut(), body.as_object())
                                    {
                                        for (k, v) in sent {
                                            stored.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                            }
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts)
    }

    /// The obligation: a run briefed five hours ago with `building`
    /// still open is aged out — `result = died`, the measurement under
    /// `aged_out`, the step's own keys kept — and a run briefed an hour
    /// ago is not. A second tick finds the first run's step completed
    /// and writes nothing more.
    #[tokio::test]
    async fn a_silent_run_is_aged_out_once_and_a_fresh_one_is_left_alone() {
        let (base, puts) = mock_jobs(vec![
            run(SILENT, Some("2026-09-18T10:00:00Z"), None),
            run(FRESH, Some("2026-09-18T14:00:00Z"), None),
        ])
        .await;
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(
            written.len(),
            2,
            "exactly one step completed — the merge, then the flip: {written:?}"
        );
        let (job, step, body) = &written[0];
        assert_eq!(job, SILENT);
        assert_eq!(step, &format!("{SILENT}-building/metadata"));
        assert_eq!(body["result"], "died");
        assert!(
            body.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them (e39a9d2a)"
        );
        assert_eq!(body["aged_out"]["silent_hours"], 5.0);
        assert_eq!(body["aged_out"]["bound_hours"], 4.0);
        assert_eq!(
            body["aged_out"]["rule"],
            "agent-run-dies-when-building-is-silent"
        );
        assert_eq!(written[1].1, format!("{SILENT}-building"));
        assert_eq!(
            written[1].2,
            json!({ "status": "completed" }),
            "the flip alone"
        );

        h.invoke(&args(), &ctx(tick("2026-09-18T16:00:00Z")))
            .await
            .expect("the next tick runs");
        assert_eq!(puts.lock().unwrap().len(), 2, "nothing is written twice");
    }

    /// A packet with no completion stamp is measured from its filing
    /// instant; one with neither is reported and left open.
    #[tokio::test]
    async fn the_age_falls_back_to_opened_at_and_an_unreadable_age_is_left_alone() {
        let (base, puts) = mock_jobs(vec![
            run(SILENT, None, Some("2026-09-18T09:00:00+00:00")),
            run(AGELESS, None, None),
        ])
        .await;
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(written.len(), 2, "the merge, then the flip: {written:?}");
        assert_eq!(written[0].0, SILENT);
        assert_eq!(written[0].2["aged_out"]["silent_hours"], 6.0);
    }

    /// A value a person wrote on the step is their record: the template
    /// fills absent keys only.
    #[tokio::test]
    async fn the_template_never_overwrites_a_recorded_value() {
        let mut silent = run(SILENT, Some("2026-09-18T10:00:00Z"), None);
        silent["steps"][2]["metadata"]["result"] = json!("refused");
        let (base, puts) = mock_jobs(vec![silent]).await;
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args(), &ctx(tick("2026-09-18T15:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        assert_eq!(written.len(), 2, "the merge, then the flip: {written:?}");
        assert!(
            written[0].2.get("result").is_none(),
            "a recorded value is neither overwritten nor re-sent"
        );
    }

    /// A daily firing carries no `_at`; the handler refuses rather than
    /// reading a clock of its own, and says which cadence to use.
    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let (base, puts) = mock_jobs(vec![run(SILENT, Some("2026-09-18T10:00:00Z"), None)]).await;
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), &base);
        let err = h
            .invoke(&args(), &ctx(json!({ "_day": "2026-09-18" })))
            .await
            .expect_err("no _at, no judgement");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("_at") && m.contains("hourly")),
            "{err}"
        );
        assert!(puts.lock().unwrap().is_empty());
    }

    #[test]
    fn a_bound_that_is_not_a_positive_number_is_permanent() {
        for bad in ["0", "-2", "soon", ""] {
            let mut a = args();
            a[2].1 = Value::String(bad.into());
            assert!(
                matches!(bound_hours(&a), Err(HandlerError::Permanent(_))),
                "{bad:?}"
            );
        }
        assert_eq!(bound_hours(&args()).unwrap(), 4.0);
    }

    #[test]
    fn last_moved_prefers_the_newest_completion_over_the_filing_instant() {
        let mut job = run(
            SILENT,
            Some("2026-09-18T10:00:00Z"),
            Some("2026-09-18T09:00:00Z"),
        );
        assert_eq!(
            last_moved(&job, None).unwrap().to_rfc3339(),
            "2026-09-18T10:00:00+00:00"
        );
        job["steps"][1]["completed_at"] = json!(null);
        assert_eq!(
            last_moved(&job, None).unwrap().to_rfc3339(),
            "2026-09-18T09:00:00+00:00"
        );
        job["metadata"]["opened_at"] = json!(null);
        assert_eq!(last_moved(&job, None), None);
    }

    /// A kind whose life is a heartbeat, not a chain of completions: a
    /// work-session prompts for hours and completes nothing between
    /// SessionStart and SessionEnd (design 511fa7d4 car 2b, backlog
    /// da925366). With `since_key = last_active_at` the heartbeat is
    /// movement — a session that prompted an hour ago is left alone
    /// however long ago it opened; one whose last prompt is past the
    /// bound is ended, `ended = silent`, measured from that prompt and
    /// not from its opening.
    #[tokio::test]
    async fn the_since_key_reads_a_heartbeat_as_movement() {
        let session = |id: &str, last_active_at: Option<&str>| {
            let mut metadata = json!({
                "actor": "emp-david", "host": "boss-dev-0",
                "started_at": "2026-09-19T00:00:00Z", "opened_at": "2026-09-19T00:00:00Z",
            });
            if let Some(t) = last_active_at {
                metadata["last_active_at"] = json!(t);
            }
            json!({
                "id": id, "kind": "work-session", "title": "session", "status": "open",
                "metadata": metadata,
                "steps": [
                    { "id": format!("{id}-opened"), "spec_slug": "opened", "status": "completed",
                      "completed_at": null, "metadata": {} },
                    { "id": format!("{id}-active"), "spec_slug": "active", "status": "ready",
                      "metadata": { "authority_role": "platform-admin" } },
                ],
            })
        };
        let (base, puts) = mock_jobs(vec![
            // Opened 9h ago, prompted 1h ago: alive.
            session(FRESH, Some("2026-09-19T08:00:00Z")),
            // Opened 9h ago, last prompt 7h ago: silent past six.
            session(SILENT, Some("2026-09-19T02:00:00Z")),
            // Opened 9h ago, never prompted: measured from its opening.
            session(AGELESS, None),
        ])
        .await;
        let args = vec![
            ("kind".to_string(), Value::String("work-session".into())),
            ("step".to_string(), Value::String("active".into())),
            ("hours".to_string(), Value::String("6".into())),
            (
                "since_key".to_string(),
                Value::String("last_active_at".into()),
            ),
            (
                "done_metadata".to_string(),
                Value::String(r#"{"ended": "silent"}"#.into()),
            ),
        ];
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), &base);
        h.invoke(&args, &ctx(tick("2026-09-19T09:00:00Z")))
            .await
            .expect("the tick runs");
        let written = puts.lock().unwrap().clone();
        // The merges: one per ended session, onto its `active` step.
        let merges: Vec<_> = written
            .iter()
            .filter(|(_, s, _)| s.ends_with("/metadata"))
            .collect();
        let mut ended: Vec<&str> = merges.iter().map(|(j, _, _)| j.as_str()).collect();
        ended.sort();
        assert_eq!(ended, vec![SILENT, AGELESS], "{written:?}");
        assert_eq!(written.len(), 4, "a merge and a flip for each: {written:?}");
        let silent = merges.iter().find(|(j, _, _)| j == SILENT).unwrap();
        assert_eq!(silent.2["ended"], "silent");
        assert_eq!(
            silent.2["aged_out"]["silent_hours"], 7.0,
            "measured from the last prompt, not the opening"
        );
        let never = merges.iter().find(|(j, _, _)| j == AGELESS).unwrap();
        assert_eq!(never.2["aged_out"]["silent_hours"], 9.0);
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsAgeOutStep::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "jobs.age_out_step");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.age_out_step")
                .cloned(),
            Some(vec!["jobs.step.completed"]),
            "the cascade table knows what this handler emits"
        );
    }
}
