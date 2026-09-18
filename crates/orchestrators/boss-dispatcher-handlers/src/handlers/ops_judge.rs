//! `ops.judge` — an answered ops-request's verdict line files the next
//! verb, with args, so a follow-on that needs a word (`--for-real`)
//! rides a rule instead of a person.
//!
//! The gap this closes (backlog a1d3c762, measured in retro 27fad542):
//! the `publish-drift` verb's `--check` half rides a rule
//! (check-publish-drift-on-boss-gcp-converge-moved fires the moment
//! boss-gcp's checkout moves), but its `--for-real` half could not —
//! `jobs.job.closed` carries no metadata and no step output, and
//! `jobs.spawn` cannot pass an args list, so nothing could read the
//! answered `--check`'s verdict line and file the publish. 23 hand
//! publishes on 2026-09-18 13:4x were that gap. `maintenance.sweep.judge`
//! (970c0c94) already reads a verdict off a closed ops-request and
//! completes a step with it; this is the same shape, spawning instead of
//! completing, with every noun in the rule's args so the NEXT verb chain
//! is a rule file and not a handler.
//!
//! ## The rule's args
//!
//! - `verb` — which answered ops-request this rule judges.
//! - `verdict_pattern` — a regex over the run step's recorded output,
//!   with NAMED groups; the LAST line it matches is the verdict. The arg
//!   passes through two string lexers (TOML, then boss-expr's), so write
//!   `[0-9]+` rather than `\d+`, which boss-expr's lexer reads as `d+`.
//! - `when` — a boss-expr predicate over the groups, each an int when it
//!   parses as one (`k = 0 AND n >= 1`). Measured: boss-expr evaluates
//!   its identifiers against any JSON object, so ONE definition of the
//!   predicate language serves rule `when`, step `ready_when`, and this.
//! - `then_verb`, `then_host`, `then_args` — the follow-on ops-request;
//!   `then_args` is whitespace-separated (a verb param admits none,
//!   `infra/ops/ops-runner.sh`), rendered as the JSON array the runner's
//!   argv contract reads off `metadata.args`.
//!
//! ## What it does
//!
//! On `jobs.job.closed` for an `ops-request` that closed `answered`:
//!
//! 1. Read the closed request. It is this rule's only when its
//!    `metadata.verb` is `verb`; a request that IS the packet this rule
//!    would file (same verb, same args) is never judged by it, so a
//!    pattern that happened to match the follow-on's own answer could
//!    not chain forever.
//! 2. A request already carrying `metadata.judged` was judged by an
//!    earlier delivery (JetStream is at-least-once): nothing more.
//! 3. Find the verdict — the last line of the `execute` step's `output`
//!    that matches `verdict_pattern`. None means the verb predates the
//!    verdict or was killed before its last line: warn, write nothing.
//! 4. Evaluate `when` over the groups. FALSE: write `judged` (the
//!    predicate, the groups, the line) onto the judged request and file
//!    nothing — a refusal is decided by a person, at the surface that
//!    shows it. TRUE: file the follow-on unless an open `then_verb`
//!    request already carries this request's id as `for_check` (or the
//!    same `for_converge`, when the judged request names one), then write
//!    `judged` naming what was filed. The spawned packet carries
//!    `for_check` = the judged request's id, so a reader follows the
//!    evidence from the publish to the check that earned it.

use super::common::{api_client, get_json, open_jobs_of_kind, write_json};
use super::jobs_complete_linked_step::step_by_slug;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::{self, Value};
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::json;
use std::sync::Arc;

/// The step the ops-runner completes with the verb's output.
const REPORT_STEP: &str = "execute";
/// The key this handler writes onto a judged request, and reads first
/// on a redelivery.
pub(crate) const JUDGED: &str = "judged";
/// The link a filed follow-on carries back to the request it judged.
pub(crate) const FOR_CHECK: &str = "for_check";

/// One rule's declaration, parsed off its args. A bad regex or a bad
/// predicate is rule authoring, identical on every redelivery, so both
/// are `Permanent`.
pub(crate) struct Judgement {
    pub verb: String,
    pub pattern: regex::Regex,
    pub when_src: String,
    pub when: expr::Expr,
    pub then_verb: String,
    pub then_host: String,
    pub then_args: Vec<String>,
}

impl Judgement {
    pub(crate) fn from_args(args: &[(String, Value)]) -> Result<Self, HandlerError> {
        let verb = arg_string(args, "verb")?.to_string();
        let pattern_src = arg_string(args, "verdict_pattern")?;
        let pattern = regex::Regex::new(pattern_src).map_err(|e| {
            HandlerError::Permanent(format!(
                "verdict_pattern {pattern_src:?} is not a regex: {e}"
            ))
        })?;
        let when_src = arg_string(args, "when")?.to_string();
        let when = expr::parse(&when_src).map_err(|e| {
            HandlerError::Permanent(format!("when {when_src:?} does not parse: {e}"))
        })?;
        let then_verb = arg_string(args, "then_verb")?.to_string();
        let then_host = arg_string(args, "then_host")?.to_string();
        let then_args = arg_string(args, "then_args")?
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(Self {
            verb,
            pattern,
            when_src,
            when,
            then_verb,
            then_host,
            then_args,
        })
    }
}

/// PURE: the verdict a recorded output carries — the LAST line the
/// pattern matches, as (the line, its named groups as JSON). A group
/// that parses as an integer is one, so `k = 0` compares numbers; any
/// other group is a string. `None` when no line matches.
pub(crate) fn verdict_groups(
    pattern: &regex::Regex,
    output: &str,
) -> Option<(String, serde_json::Value)> {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|l| pattern.is_match(l))?;
    let caps = pattern.captures(line)?;
    let groups: serde_json::Map<String, serde_json::Value> = pattern
        .capture_names()
        .flatten()
        .filter_map(|name| {
            let m = caps.name(name)?;
            let v = match m.as_str().parse::<i64>() {
                Ok(i) => json!(i),
                Err(_) => json!(m.as_str()),
            };
            Some((name.to_string(), v))
        })
        .collect();
    Some((line.to_string(), serde_json::Value::Object(groups)))
}

/// PURE: does the rule's `when` hold over these groups? A predicate that
/// evaluates to something other than a bool (a bare identifier, a
/// missing group compared as absent is fine — that is false) is rule
/// authoring, so `Permanent`.
pub(crate) fn when_holds(
    when: &expr::Expr,
    when_src: &str,
    groups: &serde_json::Value,
) -> Result<bool, HandlerError> {
    let ctx = expr::Context {
        payload: groups,
        helpers: &expr::NoHelpers,
    };
    match expr::eval(when, &ctx) {
        Ok(v) => v.as_bool().ok_or_else(|| {
            HandlerError::Permanent(format!(
                "when {when_src:?} evaluated to {} over {groups}, not a bool",
                v.kind()
            ))
        }),
        Err(e) => Err(HandlerError::Permanent(format!(
            "when {when_src:?} failed over {groups}: {e}"
        ))),
    }
}

/// PURE: the follow-on ops-request, in the shape the ops-runner reads
/// (`metadata.host`, `metadata.verb`, `metadata.args` as a JSON array of
/// strings) plus the links a reader follows back.
pub(crate) fn follow_on_body(
    j: &Judgement,
    judged_id: &str,
    judged: &serde_json::Value,
    verdict: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let short = &judged_id[..judged_id.len().min(8)];
    let args = j.then_args.join(" ");
    let mut metadata = json!({
        "host": j.then_host,
        "verb": j.then_verb,
        "args": j.then_args,
        FOR_CHECK: judged_id,
        "judged_verdict": verdict,
        "spawned_by_rule": ctx.rule_name,
        "triggered_by_event_id": ctx.triggering_event_id,
        "triggered_by_topic": ctx.triggering_topic,
    });
    if let (Some(fc), Some(m)) = (meta_str(judged, "for_converge"), metadata.as_object_mut()) {
        m.insert("for_converge".into(), json!(fc));
    }
    json!({
        "kind": "ops-request",
        "title": format!(
            "{} {args} on {} — filed on {}'s answer (ops-request {short})",
            j.then_verb, j.then_host, j.verb
        ),
        "subject": {"subject_kind": "custom", "id": j.then_host},
        "owner_id": format!("rule:{}", ctx.rule_name),
        "priority": "standard",
        "status": "open",
        "tags": ["dispatcher-spawned"],
        "metadata": metadata,
    })
}

/// PURE: is `open` the follow-on this judgement already filed for
/// `judged` — same verb, linked by `for_check` or by a shared
/// `for_converge`?
pub(crate) fn already_filed(
    j: &Judgement,
    judged_id: &str,
    judged: &serde_json::Value,
    open: &serde_json::Value,
) -> bool {
    if meta_str(open, "verb") != Some(&j.then_verb) {
        return false;
    }
    if meta_str(open, FOR_CHECK) == Some(judged_id) {
        return true;
    }
    match meta_str(judged, "for_converge") {
        Some(fc) => meta_str(open, "for_converge") == Some(fc),
        None => false,
    }
}

fn meta_str<'a>(job: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    job.get("metadata")?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn meta_args(job: &serde_json::Value) -> Vec<String> {
    job.get("metadata")
        .and_then(|m| m.get("args"))
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

pub struct OpsJudge {
    client: reqwest::Client,
    jobs_base: String,
}

impl OpsJudge {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Tests point the client at a local stand-in for jobs-api.
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn job(&self, id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{id}", self.base()),
            rule,
        )
        .await?;
        Ok(job.get("data").cloned().unwrap_or(job))
    }

    /// POST the follow-on and read the id the jobs API minted for it —
    /// the one thing `common::post_json` does not return, and the thing
    /// the judged request's `judged` note names.
    async fn file(&self, body: &serde_json::Value, rule: &str) -> Result<String, HandlerError> {
        let url = format!("{}/api/jobs", self.base());
        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", super::common::dispatcher_actor_header(rule))
            .header("x-sim-origin", super::common::sim_origin_value())
            .json(body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                HandlerError::Permanent(format!("POST {url} returned {status}: {text}"))
            } else {
                HandlerError::Downstream(format!("POST {url} returned {status}: {text}"))
            });
        }
        let created: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url} answer not JSON: {e}")))?;
        created
            .get("id")
            .or_else(|| created.get("data").and_then(|d| d.get("id")))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                HandlerError::Downstream(format!("POST {url} answered no id: {created}"))
            })
    }

    async fn annotate(
        &self,
        judged_id: &str,
        note: serde_json::Value,
        rule: &str,
    ) -> Result<(), HandlerError> {
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{judged_id}/metadata", self.base()),
            &note,
            rule,
        )
        .await
    }
}

#[async_trait]
impl Handler for OpsJudge {
    fn name(&self) -> &'static str {
        "ops.judge"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let j = Judgement::from_args(args)?;
        let rule = ctx.rule_name.as_str();

        // The close marker names the packet. A malformed marker is not
        // something a redelivery can fix, so it is a no-op, not an error.
        let Some(judged_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        // 1. The judged request — this rule's only if it answered the
        //    verb this rule reads, and is not the packet this rule files.
        let judged = self.job(judged_id, rule).await?;
        if meta_str(&judged, "verb") != Some(&j.verb) {
            return Ok(());
        }
        if j.verb == j.then_verb && meta_args(&judged) == j.then_args {
            tracing::debug!(rule = %rule, judged = %judged_id, "answered {} is the follow-on this rule files, not the request it judges", j.verb);
            return Ok(());
        }

        // 2. Judged by an earlier delivery: the record is true.
        if meta_str(&judged, JUDGED).is_some() {
            return Ok(());
        }

        // 3. The verdict, off the runner's recorded output.
        let output = step_by_slug(&judged, REPORT_STEP)
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("output"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        let Some((verdict, groups)) = verdict_groups(&j.pattern, output) else {
            tracing::warn!(
                rule = %rule,
                judged = %judged_id,
                "{} answered without a line matching {:?} — the verb predates the verdict, or was cut short; nothing judged, nothing filed",
                j.verb, j.pattern.as_str()
            );
            return Ok(());
        };

        // 4. The decision.
        if !when_holds(&j.when, &j.when_src, &groups)? {
            self.annotate(
                judged_id,
                json!({
                    JUDGED: format!(
                        "{rule}: nothing filed — {} is false over {groups}; verdict: {verdict}",
                        j.when_src
                    ),
                    "judged_verdict": verdict,
                }),
                rule,
            )
            .await?;
            tracing::info!(rule = %rule, judged = %judged_id, "{verdict} — {} is false over {groups}; nothing filed", j.when_src);
            return Ok(());
        }

        let open = open_jobs_of_kind(&self.client, self.base(), "ops-request", rule).await?;
        let filed_id = match open
            .iter()
            .find(|o| already_filed(&j, judged_id, &judged, o))
            .and_then(|o| o.get("id").and_then(|v| v.as_str()))
        {
            // Filed by an earlier delivery whose note never landed, or
            // by the same converge's other check: the record still owes
            // the judged request its note.
            Some(existing) => existing.to_string(),
            None => {
                let body = follow_on_body(&j, judged_id, &judged, &verdict, ctx);
                self.file(&body, rule).await?
            }
        };
        self.annotate(
            judged_id,
            json!({
                JUDGED: format!(
                    "{rule}: filed {} {} on {} as ops-request {filed_id} — {} is true over {groups}; verdict: {verdict}",
                    j.then_verb,
                    j.then_args.join(" "),
                    j.then_host,
                    j.when_src
                ),
                "judged_verdict": verdict,
                "filed": filed_id,
            }),
            rule,
        )
        .await?;
        tracing::info!(rule = %rule, judged = %judged_id, filed = %filed_id, "{verdict} — filed {} {} on {}", j.then_verb, j.then_args.join(" "), j.then_host);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const CHECK: &str = "11111111-1111-1111-1111-111111111111";
    const CONVERGE: &str = "33333333-3333-3333-3333-333333333333";
    const RULE: &str = "publish-drift-for-real-on-clean-check";
    /// The rule's pattern, as the rule file spells it (`[0-9]+`, not
    /// `\d+` — see the module doc).
    const PATTERN: &str = "publish-drift: would publish (?P<n>[0-9]+), skipped (?P<m>[0-9]+) equal, refused (?P<k>[0-9]+)";
    const WHEN: &str = "k = 0 AND n >= 1";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            // The close marker in the shape all three emit sites produce:
            // every key present, and NO step metadata.
            event_payload: json!({
                "id": CHECK,
                "closed_on": "2026-09-18",
                "kind": "ops-request",
                "outcome": "answered",
                "title": "publish-drift --check on boss-gcp — its checkout moved",
                "subject_id": "boss-gcp",
                "parent_step_id": null,
            }),
        }
    }

    fn args() -> Vec<(String, Value)> {
        [
            ("verb", "publish-drift"),
            ("verdict_pattern", PATTERN),
            ("when", WHEN),
            ("then_verb", "publish-drift"),
            ("then_host", "boss-gcp"),
            ("then_args", "--for-real"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.into())))
        .collect()
    }

    /// The answered request, as the ops-runner completes it.
    fn request(id: &str, verb: &str, req_args: &[&str], output: &str) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "ops-request",
            "status": "closed",
            "metadata": { "host": "boss-gcp", "verb": verb, "args": req_args, "for_converge": CONVERGE },
            "steps": [
                { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "r-execute", "spec_slug": "execute", "status": "completed",
                  "completed_at": "2026-09-18T15:02:00Z",
                  "metadata": { "disposition": "answered", "exit_code": "0", "runner_host": "boss-gcp",
                                "output": output, "authority_role": "platform-admin" } },
                { "id": "r-answered", "spec_slug": "answered", "status": "completed", "metadata": {} },
            ],
        })
    }

    /// Every write the handler made: (method, path, body), in order.
    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: `GET /api/jobs/{id}` serves one, `GET
    /// /api/jobs?kind=&status=open` lists the open ones, `POST /api/jobs`
    /// mints an id and keeps the row open, `PATCH /api/jobs/{id}/metadata`
    /// merges — so a second delivery reads what the first wrote.
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        ));
        let (g, l, pj, pm) = (by_id.clone(), by_id.clone(), by_id.clone(), by_id.clone());
        let (wj, wm) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = g.clone();
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
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = l.clone();
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
                })
                .post(move |Json(body): Json<serde_json::Value>| {
                    let (w, by_id) = (wj.clone(), pj.clone());
                    async move {
                        w.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/jobs".into(), body.clone()));
                        let mut map = by_id.lock().unwrap();
                        let id = format!("f0000000-0000-0000-0000-{:012}", map.len() + 1);
                        let mut row = body;
                        row["id"] = json!(id);
                        map.insert(id.clone(), row);
                        (axum::http::StatusCode::CREATED, Json(json!({ "id": id })))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let (w, by_id) = (wm.clone(), pm.clone());
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/metadata"),
                                body.clone(),
                            ));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id)
                                && let (Some(m), Some(b)) =
                                    (job["metadata"].as_object_mut(), body.as_object())
                            {
                                for (k, v) in b {
                                    m.insert(k.clone(), v.clone());
                                }
                            }
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    /// The verb's answer, as publish-drift.sh prints it (the parked car
    /// f1b2822a): a table, then ONE verdict line with a trailing
    /// parenthetical the pattern does not need to consume.
    const CLEAN_OUTPUT: &str = "publish-drift: checkout cb053ed6 on boss-gcp\n\nkind\tfrom\tto\tresult\nmaintenance-sweep\tv3\ttree\twould publish\n\npublish-drift: would publish 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const REFUSED_OUTPUT: &str = "kind\tfrom\tto\tresult\nbacklog-item\tv9\t-\tREFUSED: live v9 carries what the tree never said\n\npublish-drift: would publish 3, skipped 19 equal, refused 1 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const NOTHING_AHEAD_OUTPUT: &str = "publish-drift: would publish 0, skipped 23 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet 11111111)\n";
    const FOR_REAL_OUTPUT: &str = "publish-drift: published 3, skipped 20 equal, refused 0 (checkout cb053ed6, 23 kind(s), packet f0000000)\n";

    fn posts(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        w.iter()
            .filter(|(m, _, _)| m == "POST")
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    /// A clean check files the follow-on with its args as the runner's
    /// JSON array, linked back by `for_check`, and notes on the check
    /// what was filed.
    #[tokio::test]
    async fn a_clean_check_files_the_follow_on_with_args_and_for_check() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], CLEAN_OUTPUT)]).await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "one spawn + one note, nothing else: {w:?}");
        let filed = &posts(&w)[0];
        assert_eq!(filed["kind"], "ops-request");
        assert_eq!(
            filed["subject"],
            json!({"subject_kind": "custom", "id": "boss-gcp"})
        );
        let m = &filed["metadata"];
        assert_eq!(m["host"], "boss-gcp");
        assert_eq!(m["verb"], "publish-drift");
        assert_eq!(
            m["args"],
            json!(["--for-real"]),
            "the runner's argv contract: a JSON array of strings"
        );
        assert_eq!(m[FOR_CHECK], CHECK, "the evidence link back to the check");
        assert_eq!(
            m["for_converge"], CONVERGE,
            "the check's own link rides through"
        );
        assert_eq!(m["spawned_by_rule"], RULE);
        assert!(
            m["judged_verdict"]
                .as_str()
                .is_some_and(|v| v
                    .starts_with("publish-drift: would publish 3, skipped 20 equal, refused 0")),
            "the verdict line, copied: {m}"
        );
        assert_eq!(filed["owner_id"], format!("rule:{RULE}"));
        assert_eq!(
            (w[1].0.as_str(), w[1].1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CHECK}/metadata"))
        );
        let note = w[1].2[JUDGED].as_str().unwrap_or("");
        assert!(
            note.contains("filed publish-drift --for-real on boss-gcp as ops-request f0000000-0000-0000-0000-000000000002"),
            "the note names what was filed: {note}"
        );
        assert_eq!(w[1].2["filed"], "f0000000-0000-0000-0000-000000000002");
    }

    /// A refusal files nothing: the check is annotated with the
    /// predicate that was false and the line it was false over, and the
    /// decision stays a person's at the Drift tab.
    #[tokio::test]
    async fn a_refused_check_files_nothing_and_annotates_the_check() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], REFUSED_OUTPUT)]).await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "one note, no spawn: {w:?}");
        assert_eq!(
            (w[0].0.as_str(), w[0].1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CHECK}/metadata"))
        );
        let note = w[0].2[JUDGED].as_str().unwrap_or("");
        assert!(note.contains("nothing filed"), "{note}");
        assert!(note.contains(WHEN), "names the predicate: {note}");
        assert!(
            note.contains("\"k\":1"),
            "and the groups it was false over: {note}"
        );
        assert!(note.contains("refused 1"), "and the line: {note}");
        assert!(w[0].2.get("filed").is_none());
    }

    /// Nothing ahead is not a refusal, but `n >= 1` is false: nothing to
    /// publish, nothing filed, and the check says so.
    #[tokio::test]
    async fn a_check_with_nothing_ahead_files_nothing() {
        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &[],
            NOTHING_AHEAD_OUTPUT,
        )])
        .await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!(w[0].0, "PATCH");
        assert!(posts(&w).is_empty());
    }

    /// The verb predates the verdict (or was killed before its last
    /// line): nothing is written and nothing is guessed.
    #[tokio::test]
    async fn an_answer_without_a_matching_line_judges_nothing() {
        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &[],
            "not yet: checkout at cb053ed6, main at 9a1b2c3d\n",
        )])
        .await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(writes.lock().unwrap().is_empty());
    }

    /// At-least-once delivery: the second delivery of one close finds
    /// the check already judged and writes nothing more — and even with
    /// the note lost, an open follow-on carrying `for_check` is found
    /// before a twin is filed.
    #[tokio::test]
    async fn a_redelivery_files_one_follow_on() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "publish-drift", &[], CLEAN_OUTPUT)]).await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base.clone());
        h.invoke(&args(), &ctx()).await.unwrap();
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(
            posts(&w).len(),
            1,
            "one follow-on across two deliveries: {w:?}"
        );
        assert_eq!(w.len(), 2, "and no second note: {w:?}");

        // The note never landed (the PATCH failed after the POST), but
        // the follow-on is open and linked: the redelivery files no
        // twin and writes the note it still owes.
        let mut check = request(CHECK, "publish-drift", &[], CLEAN_OUTPUT);
        let mut filed = request(
            "f0000000-0000-0000-0000-00000000000f",
            "publish-drift",
            &["--for-real"],
            "",
        );
        filed["status"] = json!("open");
        filed["metadata"][FOR_CHECK] = json!(CHECK);
        check["metadata"].as_object_mut().unwrap().remove(JUDGED);
        let (base, writes) = mock_jobs(vec![check, filed]).await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert!(posts(&w).is_empty(), "no twin: {w:?}");
        assert_eq!(w.len(), 1, "the owed note: {w:?}");
        assert_eq!(w[0].2["filed"], "f0000000-0000-0000-0000-00000000000f");
    }

    /// A rule judges one verb. Another verb's answer is not its
    /// business, and neither is the packet the rule itself files — the
    /// follow-on's own answer (`published N`) closes `answered` through
    /// the same event and must not chain.
    #[tokio::test]
    async fn another_verb_and_the_rules_own_follow_on_are_not_judged() {
        let (base, writes) =
            mock_jobs(vec![request(CHECK, "disk-report", &[], CLEAN_OUTPUT)]).await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(writes.lock().unwrap().is_empty(), "wrong verb");

        let (base, writes) = mock_jobs(vec![request(
            CHECK,
            "publish-drift",
            &["--for-real"],
            FOR_REAL_OUTPUT,
        )])
        .await;
        let h = OpsJudge::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx()).await.unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "the rule's own follow-on"
        );
    }

    #[test]
    fn the_verdict_is_the_last_matching_line_and_its_groups_are_numbers() {
        let re = regex::Regex::new(PATTERN).unwrap();
        let (line, groups) = verdict_groups(&re, CLEAN_OUTPUT).expect("matches");
        assert!(line.starts_with("publish-drift: would publish 3, skipped 20 equal, refused 0 ("));
        assert_eq!(groups, json!({"n": 3, "m": 20, "k": 0}));
        // The ops-runner merges streams and may append a marker after
        // the verdict: the LAST matching line wins, trailing text does
        // not hide it.
        let two = "publish-drift: would publish 1, skipped 1 equal, refused 1\npublish-drift: would publish 2, skipped 2 equal, refused 0\n[ops-runner: output truncated]\n";
        assert_eq!(verdict_groups(&re, two).unwrap().1["n"], 2);
        assert!(verdict_groups(&re, "no verdict here\n").is_none());
        assert!(
            verdict_groups(&re, FOR_REAL_OUTPUT).is_none(),
            "`published N` is not `would publish N`"
        );
    }

    /// THE MEASUREMENT the module doc claims: boss-expr evaluates the
    /// rule's `when` over the groups as JSON — one predicate language.
    #[test]
    fn when_is_boss_expr_over_the_groups() {
        let when = expr::parse(WHEN).unwrap();
        assert!(when_holds(&when, WHEN, &json!({"n": 3, "m": 20, "k": 0})).unwrap());
        assert!(!when_holds(&when, WHEN, &json!({"n": 3, "m": 19, "k": 1})).unwrap());
        assert!(!when_holds(&when, WHEN, &json!({"n": 0, "m": 23, "k": 0})).unwrap());
        // A group the pattern did not capture is absent, which is false
        // in comparison — not an error that pins the rule.
        assert!(!when_holds(&when, WHEN, &json!({"n": 3})).unwrap());
        // A predicate that is not a predicate is rule authoring.
        let bare = expr::parse("n").unwrap();
        assert!(matches!(
            when_holds(&bare, "n", &json!({"n": 3})),
            Err(HandlerError::Permanent(_))
        ));
    }

    /// A bad regex or a bad predicate is a permanent error naming the
    /// arg; a missing arg is a missing arg.
    #[tokio::test]
    async fn bad_rule_args_are_permanent() {
        let h = OpsJudge::with_client(reqwest::Client::new(), "http://unused");
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::MissingArg(ref a) if a == "verb"),
            "{err:?}"
        );

        let mut a = args();
        a.iter_mut()
            .find(|(k, _)| k == "verdict_pattern")
            .unwrap()
            .1 = Value::String("(?P<n>[0-9]+".into());
        let err = h.invoke(&a, &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("verdict_pattern")),
            "{err:?}"
        );

        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "when").unwrap().1 = Value::String("k = = 0".into());
        let err = h.invoke(&a, &ctx()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("when")),
            "{err:?}"
        );
        assert!(err.is_permanent());
    }

    #[test]
    fn then_args_split_on_whitespace_into_the_runners_array() {
        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "then_args").unwrap().1 =
            Value::String("  --for-real   forge ".into());
        let j = Judgement::from_args(&a).unwrap();
        assert_eq!(j.then_args, vec!["--for-real", "forge"]);
        let mut a = args();
        a.iter_mut().find(|(k, _)| k == "then_args").unwrap().1 = Value::String("".into());
        assert!(Judgement::from_args(&a).unwrap().then_args.is_empty());
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = OpsJudge::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "ops.judge");
        let emits = boss_dispatcher::cascade::handler_emits()
            .get("ops.judge")
            .cloned()
            .expect("the cascade table knows this handler");
        for topic in ["jobs.job.created", "jobs.job.updated"] {
            assert!(emits.contains(&topic), "emits {topic}: {emits:?}");
        }
    }
}
