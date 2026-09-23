//! `jobs.complete_step_matching` — a closing Job completes a step on
//! every open packet whose RECORDED step metadata matches a value the
//! closing Job carries.
//!
//! The gap this closes (backlog c34583cb, decided by design b64c4377):
//! the tenant workflow `publish-the-landing-page` has a `live` step that
//! is TRUE when the newest closed `maintenance-cluster-converge` packet
//! records a `site.hash` equal to the packet's own `publish.site_hash`.
//! No handler could express that. `jobs.complete_linked_step` follows a
//! DECLARED job edge, and the converge packet cannot know which site
//! packet it will make live — the edge points the wrong way, from a
//! packet that does not exist yet. `jobs.complete_step` only completes
//! zero-duration markers. So the site packet waited at `live` for a
//! person to read the converge's record and copy it over: the
//! "retyped, not copied" step the correctness protocol refuses.
//!
//! ## Why this is a handler and not landing-page code
//!
//! Every noun is a rule arg. The handler knows "search the open packets
//! of `kind`; on each one whose `match_step` is completed and recorded
//! `match_field` equal to the value at `event_path`, complete `step`
//! once" — a shape, not a policy. Which kind, which steps, which field
//! and which path are the rule row's business, and the rule that uses
//! it is TENANT-declared (the tenant contract's `seeds/rules.toml`, a
//! separate car). Point it at a different pair of packets and it
//! answers a different obligation: any "the newest X that records
//! value V makes every Y that asked for V true".
//!
//! Rule shape (a tenant's, not one this tree ships):
//! ```toml
//! [[rule]]
//! on_event = "jobs.job.closed"
//! when = "kind = \"maintenance-cluster-converge\" AND outcome = \"converged\""
//! [[rule.do]]
//! handler = "jobs.complete_step_matching"
//! args = { kind = "\"publish-the-landing-page\"", step = "\"live\"", match_step = "\"publish\"", match_field = "\"site_hash\"", event_path = "\"steps.run.site.hash\"", done_metadata = '"{\"converge\": \"{event.id}\", \"site_hash\": \"{value}\"}"', evidence_key = "\"made_live_by\"" }
//! ```
//!
//! ## Where the value comes from — `event_path`
//!
//! A dotted path, read FIRST against the triggering event's payload
//! (`id`, `kind`, `outcome`, `closed_on`, `title`, `subject_id` on a
//! `jobs.job.closed` marker) and, when the payload has no such key,
//! against the closed Job itself, fetched through `GET /api/jobs/{id}`.
//! The close marker deliberately carries no step metadata — it names
//! the packet, and every site that emits it (three, in jobs-api) sends
//! the same seven keys — so a path into the packet's steps has to be
//! fetched. `steps.<slug>.<field…>` finds the step by `spec_slug` and
//! walks `<field…>` inside its `metadata`; any other path walks the
//! Job's JSON as returned (`metadata.<key>`, `title`, …). At every
//! object level the LONGEST key that matches is taken first, so a
//! metadata key spelled with a dot (`"site.hash"`) is found as readily
//! as a nested object (`site: { hash }`) — the converge's record is a
//! tenant's to shape, and the handler must not force one spelling.
//!
//! The comparison is a STRING one on both sides: a string is itself, a
//! number or bool is its JSON text, anything else (an object, an
//! array, null, absent) is "no value". A packet records a hash as a
//! string; a rule that pointed at an object would match nothing and
//! say so.
//!
//! ## What is written
//!
//! `done_metadata` is the same shape `jobs.complete_linked_step` takes:
//! a JSON object on the rule row, filled into keys the step does NOT
//! already carry (a value a person wrote is their record), with string
//! values substituting `{event.<key>}` from the marker's top-level
//! scalars and `{value}` for the matched value. The triggering Job's id
//! lands under `evidence_key` (default `matched_from`) as a plain
//! string — the evidence is WHICH packet made this one true, and a
//! reader follows the id. Richer provenance is the template's to
//! carry, in the step kind's own vocabulary, because only the tenant
//! knows what its `live` step requires at done.
//!
//! ## Idempotence
//!
//! JetStream is at-least-once and the close marker is emitted from
//! three sites, so this WILL run more than once for one close. A
//! candidate whose `step` is already `completed` is a no-op, logged at
//! info — and a redelivery finds exactly that, because the first
//! delivery completed it. A candidate whose `step` is still `pending`
//! is skipped too: its `ready_when` has not opened it, and completing
//! a step nothing routed to would fabricate work. Only `ready`/`active`
//! is completed. No candidate at all is the common case for most
//! closes and is a debug line, not a warning; a rule whose `event_path`
//! finds NO value on an event it selected for is a warning, because
//! that is the rule and the event disagreeing about what the event
//! carries.

use super::common::{api_client, get_json, open_jobs_of_kind, write_json};
use super::jobs_complete_linked_step::{is_open, is_unset, step_by_slug, template_arg};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use serde_json::json;
use std::sync::Arc;

/// Default metadata key the triggering Job's id lands under on the
/// completed step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "matched_from";

pub struct JobsCompleteStepMatching {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsCompleteStepMatching {
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

    /// The value at `event_path`: the payload first, then the closed Job
    /// (fetched only when the payload does not answer).
    async fn event_value(
        &self,
        path: &str,
        job_id: &str,
        ctx: &InvocationContext,
    ) -> Result<Option<String>, HandlerError> {
        let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() {
            return Ok(None);
        }
        if let Some(v) = walk(&ctx.event_payload, &segments).and_then(scalar_text) {
            return Ok(Some(v));
        }
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{job_id}", self.base()),
            &ctx.rule_name,
        )
        .await?;
        Ok(value_on_job(&job, &segments))
    }

    /// Complete `step` on `job_id`: the template's vocabulary (unset
    /// keys only) plus the triggering id under `evidence_key`, merged
    /// into the step's existing metadata — PATCH-on-PUT replaces
    /// top-level `metadata` wholesale, and `authority_role` living
    /// there is what keeps the step gated.
    async fn complete_step(
        &self,
        job_id: &str,
        step: &serde_json::Value,
        template: Option<&serde_json::Map<String, serde_json::Value>>,
        facts: &Facts<'_>,
        evidence_key: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        let mut merged = match step.get("metadata").cloned() {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        if let Some(template) = template {
            render(&mut merged, template, facts);
        }
        merged.insert(evidence_key.to_string(), json!(facts.triggering_id));
        let url = format!("{}/api/jobs/{job_id}/steps/{step_id}", self.base());
        let body = json!({
            "status": "completed",
            "metadata": serde_json::Value::Object(merged),
        });
        write_json(&self.client, reqwest::Method::PUT, &url, &body, rule).await
    }
}

/// What a template may name: the marker's scalars as `{event.<key>}`
/// and the matched value as `{value}`.
struct Facts<'a> {
    triggering_id: &'a str,
    payload: &'a serde_json::Value,
    value: &'a str,
}

/// The string a JSON scalar compares as; `None` for anything that is
/// not one (an object, an array, null).
fn scalar_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Walk `segments` into `root`, longest matching key first at every
/// object level, so `["site", "hash"]` finds both `{"site": {"hash":
/// …}}` and `{"site.hash": …}`.
fn walk<'a>(root: &'a serde_json::Value, segments: &[&str]) -> Option<&'a serde_json::Value> {
    if segments.is_empty() {
        return Some(root);
    }
    let obj = root.as_object()?;
    (1..=segments.len()).rev().find_map(|take| {
        obj.get(&segments[..take].join("."))
            .and_then(|next| walk(next, &segments[take..]))
    })
}

/// PURE: `segments` read against a fetched Job. `steps.<slug>.<field…>`
/// is the step's metadata; anything else is the Job's own JSON.
fn value_on_job(job: &serde_json::Value, segments: &[&str]) -> Option<String> {
    let found = match segments {
        ["steps", slug, rest @ ..] if !rest.is_empty() => {
            walk(step_by_slug(job, slug)?.get("metadata")?, rest)
        }
        _ => walk(job, segments),
    };
    found.and_then(scalar_text)
}

/// Fill `merged` from a template: unset keys only — metadata a person
/// already wrote is their record, not this obligation's to overwrite —
/// with string values substituting the facts.
fn render(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    template: &serde_json::Map<String, serde_json::Value>,
    facts: &Facts<'_>,
) {
    let event_scalars: Vec<(String, String)> = facts
        .payload
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| scalar_text(v).map(|s| (format!("{{event.{k}}}"), s)))
        .collect();
    for (k, v) in template {
        if !is_unset(merged.get(k)) {
            continue;
        }
        let v = match v {
            serde_json::Value::String(s) => {
                let s = event_scalars
                    .iter()
                    .fold(s.replace("{value}", facts.value), |acc, (from, to)| {
                        acc.replace(from, to)
                    });
                serde_json::Value::String(s)
            }
            other => other.clone(),
        };
        merged.insert(k.clone(), v);
    }
}

#[async_trait]
impl Handler for JobsCompleteStepMatching {
    fn name(&self) -> &'static str {
        "jobs.complete_step_matching"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let step_slug = arg_string(args, "step")?;
        let match_step = arg_string(args, "match_step")?;
        let match_field = arg_string(args, "match_field")?;
        let event_path = arg_string(args, "event_path")?;
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };
        let template = template_arg(args, "done_metadata", &ctx.rule_name);

        // The `jobs.job.closed` payload carries the closing Job's id. A
        // malformed marker is not something a redelivery can fix, so it
        // is a no-op rather than an error that retries forever.
        let Some(triggering_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        let Some(value) = self.event_value(event_path, triggering_id, ctx).await? else {
            // The rule selected this event and the event carries nothing
            // at the path the rule named: the two disagree about what
            // the event records, which is rule authoring worth a line
            // nobody has to go re-derive. Not an error — a redelivery
            // would find the same absence.
            tracing::warn!(
                rule = %ctx.rule_name,
                job = %triggering_id,
                event_path = %event_path,
                "no value at event_path — completing nothing"
            );
            return Ok(());
        };

        let facts = Facts {
            triggering_id,
            payload: &ctx.event_payload,
            value: &value,
        };
        let candidates = open_jobs_of_kind(&self.client, self.base(), kind, &ctx.rule_name).await?;
        let mut matched = 0usize;
        for job in &candidates {
            let recorded = step_by_slug(job, match_step)
                .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get(match_field))
                .and_then(scalar_text);
            if recorded.as_deref() != Some(value.as_str()) {
                continue;
            }
            matched += 1;
            let job_id = job.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let Some(step) = step_by_slug(job, step_slug) else {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    "matched but has no step `{step_slug}` — completing nothing on it"
                );
                continue;
            };
            let status = step.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "completed" {
                // A redelivery, or a person got there first. Either way
                // the record is already true.
                tracing::info!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    job = %triggering_id,
                    "`{step_slug}` already completed — no-op"
                );
                continue;
            }
            if !is_open(step) {
                // `pending`: the packet's own `ready_when` has not opened
                // it. A step nothing routed to is not this obligation's
                // to complete.
                tracing::debug!(
                    rule = %ctx.rule_name,
                    packet = %job_id,
                    "`{step_slug}` is {status}, not open — skipping"
                );
                continue;
            }
            self.complete_step(
                job_id,
                step,
                template.as_ref(),
                &facts,
                evidence_key,
                &ctx.rule_name,
            )
            .await?;
        }
        if matched == 0 {
            tracing::debug!(
                rule = %ctx.rule_name,
                job = %triggering_id,
                "no open {kind} packet records {match_step}.{match_field} = {value:?}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const CONVERGE: &str = "11111111-1111-1111-1111-111111111111";
    const SITE_A: &str = "22222222-2222-2222-2222-222222222222";
    const SITE_B: &str = "33333333-3333-3333-3333-333333333333";
    const LIVE_A: &str = "44444444-4444-4444-4444-444444444444";
    const LIVE_B: &str = "55555555-5555-5555-5555-555555555555";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "site-goes-live-on-converge".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    /// The args a tenant rule row would evaluate to.
    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".to_string(), Value::String("publish-the-landing-page".into())),
            ("step".to_string(), Value::String("live".into())),
            ("match_step".to_string(), Value::String("publish".into())),
            ("match_field".to_string(), Value::String("site_hash".into())),
            (
                "event_path".to_string(),
                Value::String("steps.run.site.hash".into()),
            ),
            (
                "done_metadata".to_string(),
                Value::String(
                    r#"{"converge": "{event.id}", "site_hash": "{value}", "note": "made live by {event.kind} {event.outcome}"}"#.into(),
                ),
            ),
            ("evidence_key".to_string(), Value::String("made_live_by".into())),
        ]
    }

    /// The close marker in the shape all three emit sites produce:
    /// every key present, and NO step metadata.
    fn close_marker() -> serde_json::Value {
        json!({
            "id": CONVERGE,
            "closed_on": "2026-09-17",
            "kind": "maintenance-cluster-converge",
            "outcome": "converged",
            "title": "Converge the cluster",
            "subject_id": "cluster",
            "parent_step_id": null,
        })
    }

    /// The closed converge packet, its `run` step recording the site
    /// hash it made live — nested, the way a runner writes an object.
    fn converge(run_metadata: serde_json::Value) -> serde_json::Value {
        json!({
            "id": CONVERGE,
            "kind": "maintenance-cluster-converge",
            "title": "Converge the cluster",
            "status": "closed",
            "metadata": { "outcome": "converged" },
            "steps": [
                { "id": "c-run", "spec_slug": "run", "status": "completed",
                  "metadata": run_metadata },
            ],
        })
    }

    /// An open site packet whose `publish` step recorded `site_hash`
    /// and whose `live` step is in `live_status`.
    fn site(id: &str, live_id: &str, site_hash: &str, live_status: &str) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "publish-the-landing-page",
            "title": format!("Publish {site_hash}"),
            "status": "open",
            "metadata": {},
            "steps": [
                { "id": format!("{id}-publish"), "spec_slug": "publish", "status": "completed",
                  "metadata": { "site_hash": site_hash } },
                { "id": live_id, "spec_slug": "live", "status": live_status,
                  "metadata": { "authority_role": "platform-admin" } },
            ],
        })
    }

    type Puts = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: `GET /api/jobs?kind=&status=` filters the
    /// stored Jobs and answers the `{data, total}` page the real list
    /// does (steps inline, as the real list enriches them); `GET
    /// /api/jobs/{id}` serves one; every step PUT is recorded as
    /// `(job, step, body)` and applied, so a second delivery reads
    /// what the first wrote.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Puts) {
        let puts: Puts = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let list_jobs = by_id.clone();
        let get_jobs = by_id.clone();
        let put_jobs = by_id.clone();
        let put_log = puts.clone();
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
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = get_jobs.clone();
                    async move {
                        by_id
                            .lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
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
                            puts.lock()
                                .unwrap()
                                .push((id.clone(), step_id.clone(), body.clone()));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                apply_step_put(job, &step_id, &body);
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

    /// The mock's PUT: overlay status + metadata on the stored step.
    fn apply_step_put(job: &mut serde_json::Value, step_id: &str, body: &serde_json::Value) {
        let Some(steps) = job.get_mut("steps").and_then(|s| s.as_array_mut()) else {
            return;
        };
        for step in steps.iter_mut() {
            if step.get("id").and_then(|v| v.as_str()) == Some(step_id) {
                if let Some(status) = body.get("status") {
                    step["status"] = status.clone();
                }
                if let Some(metadata) = body.get("metadata") {
                    step["metadata"] = metadata.clone();
                }
            }
        }
    }

    /// The obligation: the converge that made `abc123` live completes
    /// `live` on the ONE site packet that published `abc123` — not on
    /// the one that published `def456` — with the template rendered
    /// from the marker and the matched value, the converge's id under
    /// the evidence key, and the step's existing metadata kept (the
    /// PUT replaces top-level metadata wholesale).
    #[tokio::test]
    async fn a_matching_recorded_value_completes_exactly_one_step() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site": { "hash": "abc123" } })),
            site(SITE_A, LIVE_A, "abc123", "ready"),
            site(SITE_B, LIVE_B, "def456", "ready"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "exactly one step completed: {calls:?}");
        let (job, step, body) = &calls[0];
        assert_eq!(
            job, SITE_A,
            "the packet that published the hash the converge made live"
        );
        assert_eq!(step, LIVE_A);
        assert_eq!(body["status"], "completed");
        assert_eq!(
            body["metadata"]["made_live_by"], CONVERGE,
            "the evidence is the id"
        );
        assert_eq!(
            body["metadata"]["converge"], CONVERGE,
            "{{event.id}} rendered"
        );
        assert_eq!(
            body["metadata"]["site_hash"], "abc123",
            "{{value}} rendered"
        );
        assert_eq!(
            body["metadata"]["note"], "made live by maintenance-cluster-converge converged",
            "every marker scalar is a substitution"
        );
        assert_eq!(
            body["metadata"]["authority_role"], "platform-admin",
            "existing step metadata survives the PUT"
        );
    }

    /// A metadata key spelled WITH the dot (`"site.hash"`) is the same
    /// record as the nested object — the converge's shape is the
    /// tenant's, and the path must read both.
    #[tokio::test]
    async fn a_dotted_metadata_key_matches_as_readily_as_a_nested_one() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site.hash": "abc123" })),
            site(SITE_A, LIVE_A, "abc123", "active"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].1, LIVE_A, "an active step is open too");
    }

    /// A value no open packet recorded completes nothing.
    #[tokio::test]
    async fn a_value_nothing_recorded_completes_nothing() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site": { "hash": "zzz999" } })),
            site(SITE_A, LIVE_A, "abc123", "ready"),
            site(SITE_B, LIVE_B, "def456", "ready"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "no packet published zzz999"
        );
    }

    /// A redelivery (three emit sites, at-least-once) finds the step it
    /// completed and writes nothing — and so does a first delivery on a
    /// packet a person already made live.
    #[tokio::test]
    async fn an_already_completed_step_is_a_noop() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site": { "hash": "abc123" } })),
            site(SITE_A, LIVE_A, "abc123", "ready"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        h.invoke(&args(), &ctx(close_marker()))
            .await
            .expect("redelivery runs");
        assert_eq!(
            puts.lock().unwrap().len(),
            1,
            "the second delivery found `live` completed and wrote nothing"
        );

        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site": { "hash": "abc123" } })),
            site(SITE_A, LIVE_A, "abc123", "completed"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "already completed by a person"
        );
    }

    /// A `pending` step is one the packet's own `ready_when` has not
    /// opened; completing it would fabricate work nothing routed to.
    #[tokio::test]
    async fn a_pending_step_is_not_completed() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "site": { "hash": "abc123" } })),
            site(SITE_A, LIVE_A, "abc123", "pending"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(puts.lock().unwrap().is_empty());
    }

    /// A converge that recorded no hash (the path finds nothing on the
    /// marker OR on the fetched packet) completes nothing, and does not
    /// fail — a redelivery would find the same absence.
    #[tokio::test]
    async fn a_missing_event_value_completes_nothing() {
        let (base, puts) = mock_jobs(vec![
            converge(json!({ "exit_code": 0 })),
            site(SITE_A, LIVE_A, "abc123", "ready"),
        ])
        .await;
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(puts.lock().unwrap().is_empty());
    }

    /// A path the marker itself answers never fetches the packet: the
    /// mock holds no converge here, and the match still completes.
    #[tokio::test]
    async fn a_path_the_marker_answers_needs_no_fetch() {
        let (base, puts) = mock_jobs(vec![site(SITE_A, LIVE_A, CONVERGE, "ready")]).await;
        let mut a = args();
        a.retain(|(k, _)| k != "event_path");
        a.push(("event_path".to_string(), Value::String("id".into())));
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), base);
        h.invoke(&a, &ctx(close_marker())).await.expect("runs");
        assert_eq!(puts.lock().unwrap().len(), 1);
    }

    /// `is_unset` semantics: a value a person already wrote on the step
    /// is kept, and the `""` placeholder counts as unset.
    #[test]
    fn render_fills_unset_keys_only_and_substitutes_facts() {
        let payload = close_marker();
        let facts = Facts {
            triggering_id: CONVERGE,
            payload: &payload,
            value: "abc123",
        };
        let template: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"converge": "{event.id}", "site_hash": "{value}", "verdict": "ok", "n": 3}"#,
        )
        .unwrap();
        let mut merged: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"verdict": "a person's word", "converge": ""}"#).unwrap();
        render(&mut merged, &template, &facts);
        assert_eq!(merged["verdict"], "a person's word");
        assert_eq!(
            merged["converge"], CONVERGE,
            "the empty placeholder is unset"
        );
        assert_eq!(merged["site_hash"], "abc123");
        assert_eq!(merged["n"], 3, "non-strings are copied as they are");
    }

    #[test]
    fn value_on_job_reads_step_metadata_by_slug_and_the_job_otherwise() {
        let job = converge(json!({ "site": { "hash": "abc123" }, "n": 7, "ok": true }));
        assert_eq!(
            value_on_job(&job, &["steps", "run", "site", "hash"]).as_deref(),
            Some("abc123")
        );
        assert_eq!(
            value_on_job(&job, &["steps", "run", "n"]).as_deref(),
            Some("7")
        );
        assert_eq!(
            value_on_job(&job, &["steps", "run", "ok"]).as_deref(),
            Some("true")
        );
        assert_eq!(
            value_on_job(&job, &["steps", "run", "site"]),
            None,
            "an object is not a value"
        );
        assert_eq!(value_on_job(&job, &["steps", "nope", "x"]), None);
        assert_eq!(
            value_on_job(&job, &["metadata", "outcome"]).as_deref(),
            Some("converged"),
            "a path off the steps walks the Job itself"
        );
        assert_eq!(
            value_on_job(&job, &["title"]).as_deref(),
            Some("Converge the cluster")
        );
    }

    #[test]
    fn a_missing_required_arg_is_a_missing_arg_error() {
        let mut a = args();
        a.retain(|(k, _)| k != "match_field");
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), "http://unused");
        let err = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(h.invoke(&a, &ctx(close_marker())))
            .expect_err("match_field is required");
        assert!(matches!(err, HandlerError::MissingArg(ref a) if a == "match_field"));
    }

    /// The name a rule row invokes, and the cascade table's entry for
    /// it (what the handler EMITS — a step completion — which is what
    /// the rules visualization draws the loop from).
    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsCompleteStepMatching::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "jobs.complete_step_matching");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.complete_step_matching")
                .cloned(),
            Some(vec!["jobs.step.completed"]),
            "the cascade table knows what this handler emits"
        );
    }

    /// The fixture a tenant's `seeds/rules.toml` would carry — the
    /// shape this tree does NOT ship as a live rule (the rule is the
    /// tenant's, a separate car). It parses as a rule file, validates
    /// through the same function `POST /api/dispatcher/rules/_validate`
    /// calls, and its args evaluate to the strings the handler reads —
    /// the JSON template's quoting inside an expression string literal
    /// inside a TOML literal string is the part worth a witness.
    const RULE_FIXTURE: &str = r#"
[[rule]]
name = "site-goes-live-on-converge"
why = "a converge that records the hash a site packet published makes that packet live"
version = 1
on_event = "jobs.job.closed"
when = "kind = \"maintenance-cluster-converge\" AND outcome = \"converged\""
[[rule.do]]
handler = "jobs.complete_step_matching"
args = { kind = "\"publish-the-landing-page\"", step = "\"live\"", match_step = "\"publish\"", match_field = "\"site_hash\"", event_path = "\"steps.run.site.hash\"", done_metadata = '"{\"converge\": \"{event.id}\", \"site_hash\": \"{value}\"}"', evidence_key = "\"made_live_by\"" }
"#;

    #[test]
    fn a_rule_file_can_reference_the_handler_with_these_args_and_validates() {
        use boss_dispatcher::rules::authoring::validate;
        use boss_dispatcher::rules::expr::NoHelpers;
        use boss_dispatcher::rules::registry::{Registry, match_event, parse_raw};

        let raw = parse_raw(RULE_FIXTURE).expect("the fixture is a rule file");
        assert_eq!(raw.rules.len(), 1);
        validate(&raw.rules[0]).expect("validates as _validate would");

        let reg = Registry::from_raw(raw).expect("loads as the dispatcher would");
        let outcome = match_event(&reg, "jobs.job.closed", &close_marker(), &NoHelpers);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        let inv = &outcome.matched[0].invocations[0];
        assert_eq!(inv.handler, "jobs.complete_step_matching");
        let expect = args();
        for (k, v) in expect.iter().filter(|(k, _)| k != "done_metadata") {
            assert_eq!(arg(&inv.args, k), Some(v), "arg {k}");
        }
        assert_eq!(
            arg(&inv.args, "done_metadata"),
            Some(&Value::String(
                r#"{"converge": "{event.id}", "site_hash": "{value}"}"#.into()
            )),
            "the template survives three layers of quoting as the JSON the handler parses"
        );
        let template = template_arg(&inv.args, "done_metadata", "site-goes-live-on-converge")
            .expect("and parses as a JSON object");
        assert_eq!(template["converge"], "{event.id}");
    }
}
