//! `maintenance.chore.file_reds` — a chore that closed red opens one
//! backlog-item per red route it recorded.
//!
//! The gap this closes (backlog ac3270c7, design 0e07ce64, 2026-09-18).
//! The nightly playground crawl records its verdict on its `run` step
//! through boss-chore.sh — `result=failed` and the check's own output,
//! one `RED <route> <kind>: <error>` line per surface that threw or
//! never painted — and its packet closes `failed`. That is the whole
//! of what happened: the chore records, and nothing opened. A red route
//! lived in a closed packet's step metadata, found by whoever thought
//! to open it, which on the chore's first rehearsal was the operator by
//! hand (`/it/registry/dispatcher` throwing on the playground's real
//! rule rows). The chore's own header says whose job the opening is:
//! "the daily judge's, not this run's". This is that judge, in the
//! shape `ops.judge` and `maintenance.sweep.judge` landed the same day
//! (#448, #452): fire on the close, read the recorded step, write the
//! consequence, note on the judged packet what was done.
//!
//! ## The rule's args
//!
//! - `step` — the slug of the step the chore's output was recorded on
//!   (`run` for every boss-chore.sh chore).
//! - `design` — the design id every filed item carries as
//!   `metadata.design`, so the queue reads the items as one thread.
//! - `area` — `metadata.area` on each item, the queue's grouping key.
//!
//! Which chore kind and which outcome ride the rule's `when`; this
//! handler reads whatever closed and finds RED lines or does not.
//!
//! ## What it does
//!
//! On `jobs.job.closed`:
//!
//! 1. Read the closed chore. A packet already carrying
//!    `metadata.judged` was judged by an earlier delivery (JetStream is
//!    at-least-once): nothing more.
//! 2. Parse the RED lines off the `step`'s recorded `output`, grouped
//!    by route — a route that threw twice is one finding with two
//!    errors, and ONE item. No RED line (a red for another reason: the
//!    clone refused, the browser never started) files nothing and says
//!    so on the packet, so the closed chore still reads as judged.
//!    boss-chore.sh carries head 20 + tail 40 lines of the capture;
//!    the crawl prints its RED lines last, so up to ~38 red routes
//!    ride the packet and an omitted-lines marker means the pod log
//!    holds more.
//! 3. Dedup by route while one is open: every open backlog-item
//!    carrying `metadata.red_route` (the paged walk `ops.judge` reads
//!    the open board with, `common::open_jobs_of_kind`) — a route with
//!    an open item files nothing, whichever night raised it. A route
//!    whose item was CLOSED files again: a recurrence after a fix is a
//!    new fact, the same posture `estate.alarm` takes.
//! 4. File one backlog-item per remaining route, carrying the route,
//!    the kind(s), the error text copied not retyped, the design id,
//!    and `source` = the chore packet's id so a reader follows the
//!    evidence to the run that saw it.
//! 5. Note `judged` on the chore's packet naming what was filed and
//!    what was already open.

use super::common::{api_client, get_json, open_jobs_of_kind, owner_for_filing, write_json};
use super::jobs_complete_linked_step::step_by_slug;
use super::ops_judge::JUDGED;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use serde_json::json;
use std::sync::Arc;

/// The line shape the crawl prints per red surface
/// (apps/web/tests/live/playground-crawl.spec.ts, pinned by
/// the_playground_is_crawled_nightly.rs): `RED <route> <kind>: <error>`.
pub(crate) const RED_PREFIX: &str = "RED ";
/// The dedup key a filed item carries — the route, and nothing else,
/// because "dedup by route while one is open" is the whole rule.
pub(crate) const RED_ROUTE: &str = "red_route";

/// One red route as the crawl reported it: every `(kind, error)` line
/// it printed for that route, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedRoute {
    pub route: String,
    pub findings: Vec<(String, String)>,
}

/// PURE: the RED lines of a recorded output, grouped by route in
/// first-seen order. A line is `RED <route> <kind>: <error>` — route
/// and kind carry no whitespace, the error is the rest of the line.
/// A line that starts `RED ` but lacks that shape is not a finding.
pub(crate) fn red_routes(output: &str) -> Vec<RedRoute> {
    output
        .lines()
        .map(str::trim)
        .filter_map(|l| {
            let rest = l.strip_prefix(RED_PREFIX)?;
            let (route, rest) = rest.split_once(' ')?;
            let (kind, error) = rest.split_once(':')?;
            if route.is_empty() || kind.is_empty() || kind.contains(char::is_whitespace) {
                return None;
            }
            Some((
                route.to_string(),
                kind.to_string(),
                error.trim().to_string(),
            ))
        })
        .fold(
            Vec::new(),
            |mut acc: Vec<RedRoute>, (route, kind, error)| {
                match acc.iter_mut().find(|r| r.route == route) {
                    Some(r) => r.findings.push((kind, error)),
                    None => acc.push(RedRoute {
                        route,
                        findings: vec![(kind, error)],
                    }),
                }
                acc
            },
        )
}

/// PURE: the routes already carried by an open backlog-item — the
/// dedup set, over the open listing.
pub(crate) fn open_routes(open_jobs: &[serde_json::Value]) -> Vec<String> {
    open_jobs
        .iter()
        .filter_map(|j| j.get("metadata")?.get(RED_ROUTE)?.as_str())
        .map(str::to_string)
        .collect()
}

/// PURE: the backlog-item one red route becomes.
pub(crate) fn item_body(
    red: &RedRoute,
    chore_id: &str,
    chore_kind: &str,
    chore_title: &str,
    design: &str,
    area: &str,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let mut kinds: Vec<&str> = red.findings.iter().map(|(k, _)| k.as_str()).collect();
    kinds.dedup();
    let kinds = kinds.join(",");
    let errors: Vec<String> = red
        .findings
        .iter()
        .map(|(k, e)| format!("{k}: {e}"))
        .collect();
    let short = &chore_id[..chore_id.len().min(8)];
    json!({
        "kind": "backlog-item",
        "title": format!("{chore_title} red: {} ({kinds})", red.route),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "standard",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": area,
            "design": design,
            RED_ROUTE: red.route,
            "red_kind": kinds,
            "red_error": errors.join(" | "),
            "source": chore_id,
            "source_kind": chore_kind,
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog ac3270c7, design {design}): the {chore_kind} packet \
                 {short} closed red with this route on its own RED line. {} The chore's run \
                 step holds the whole excerpt; the pod log holds the whole output. One item \
                 per route while it is open — a second red night for the same route files \
                 nothing until this one closes.",
                ctx.rule_name,
                errors.join(" | ")
            ),
        },
    })
}

pub struct ChoreFileReds {
    client: reqwest::Client,
    jobs_base: String,
    /// Who the items this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl ChoreFileReds {
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

    /// Tests point the client at a local stand-in for jobs-api.
    pub fn with_client(
        client: reqwest::Client,
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
            owner,
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

    async fn annotate(
        &self,
        chore_id: &str,
        note: serde_json::Value,
        rule: &str,
    ) -> Result<(), HandlerError> {
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{}/api/jobs/{chore_id}/metadata", self.base()),
            &note,
            rule,
        )
        .await
    }
}

fn str_field<'a>(job: &'a serde_json::Value, key: &str) -> &'a str {
    job.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

#[async_trait]
impl Handler for ChoreFileReds {
    fn name(&self) -> &'static str {
        "maintenance.chore.file_reds"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let step = arg_string(args, "step")?;
        let design = arg_string(args, "design")?;
        let area = arg_string(args, "area")?;
        let rule = ctx.rule_name.as_str();

        // The close marker names the packet. A malformed marker is not
        // something a redelivery can fix, so it is a no-op, not an error.
        let Some(chore_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        // 1. The closed chore; judged by an earlier delivery means the
        //    record is true.
        let chore = self.job(chore_id, rule).await?;
        if chore
            .get("metadata")
            .and_then(|m| m.get(JUDGED))
            .and_then(|v| v.as_str())
            .is_some()
        {
            return Ok(());
        }
        let chore_kind = str_field(&chore, "kind");
        let chore_title = str_field(&chore, "title");

        // 2. The RED lines, off the recorded output.
        let output = step_by_slug(&chore, step)
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("output"))
            .and_then(|o| o.as_str())
            .unwrap_or("");
        let reds = red_routes(output);
        if reds.is_empty() {
            self.annotate(
                chore_id,
                json!({
                    JUDGED: format!(
                        "{rule}: nothing filed — no `{RED_PREFIX}<route> <kind>: <error>` line \
                         on step `{step}`; a red for another reason (the clone, the browser) \
                         is read on this packet, not opened per route"
                    ),
                }),
                rule,
            )
            .await?;
            tracing::info!(rule = %rule, chore = %chore_id, "closed red with no RED line on step {step}; nothing filed");
            return Ok(());
        }

        // 3. Dedup by route while one is open.
        let open = open_jobs_of_kind(&self.client, self.base(), "backlog-item", rule).await?;
        let already = open_routes(&open);
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;

        // 4. One item per route not already open.
        let mut filed: Vec<(String, String)> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for red in &reds {
            if already.iter().any(|r| r == &red.route) {
                skipped.push(red.route.clone());
                continue;
            }
            let body = item_body(
                red,
                chore_id,
                chore_kind,
                chore_title,
                design,
                area,
                &owner,
                ctx,
            );
            let id = super::common::post_json_minted_id(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                &body,
                rule,
            )
            .await?;
            filed.push((red.route.clone(), id));
        }

        // 5. The note on the judged chore.
        let filed_ids: Vec<&str> = filed.iter().map(|(_, id)| id.as_str()).collect();
        let filed_text: Vec<String> = filed
            .iter()
            .map(|(route, id)| format!("{route} → {}", &id[..id.len().min(8)]))
            .collect();
        self.annotate(
            chore_id,
            json!({
                JUDGED: format!(
                    "{rule}: {} red route(s) on step `{step}`; filed {} backlog-item(s) [{}]; \
                     already open [{}]",
                    reds.len(),
                    filed.len(),
                    filed_text.join(", "),
                    skipped.join(", ")
                ),
                "filed": filed_ids,
            }),
            rule,
        )
        .await?;
        tracing::info!(rule = %rule, chore = %chore_id, filed = filed.len(), already_open = skipped.len(), "{} red route(s) judged", reds.len());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const CRAWL: &str = "22222222-2222-2222-2222-222222222222";
    const RULE: &str = "file-backlog-items-on-playground-crawl-red";
    const DESIGN: &str = "0e07ce64";

    /// The crawl's output as boss-chore.sh carries it onto the run step
    /// (the summary line, the reported console noise, then the RED
    /// lines the spec prints last).
    const RED_OUTPUT: &str = "\n> boss-web@0.0.0 test:live\n> playwright test -c playwright.live.config.ts\n\nRunning 1 test using 1 worker\n\ncrawled 55 routes at http://boss-gateway.boss-playground.svc.cluster.local: 3 red, 2 console.error\n  console.error [/it] Failed to load resource: the server responded with a status of 404 (Not Found)\n  console.error [/it/codebase] Failed to load resource: the server responded with a status of 403 (Forbidden)\nRED /it/registry/dispatcher pageerror: Cannot read properties of null (reading 'why') | at DispatcherRules.svelte:42\nRED /it/registry/dispatcher pageerror: Cannot read properties of null (reading 'why') | at DispatcherRules.svelte:42\nRED /ux/catalog/models/m-1 no-shell: Timed out 20000ms waiting for expect(locator).toBeVisible()\n  1) every catalogued route renders the instance as a guest\n";
    const NO_RED_OUTPUT: &str =
        "fatal: unable to access the forge: The requested URL returned error: 401\n";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: RULE.into(),
            triggering_event_id: "evt-close-9".into(),
            triggering_topic: "jobs.job.closed".into(),
            // The close marker in the shape every emit site produces:
            // every key present, and NO step metadata.
            event_payload: json!({
                "id": CRAWL,
                "closed_on": "2026-09-19",
                "kind": "maintenance-playground-crawl",
                "outcome": "failed",
                "title": "Playground crawl",
                "subject_id": "infra/maintenance-playground-crawl",
                "parent_step_id": null,
            }),
        }
    }

    fn args() -> Vec<(String, Value)> {
        [("step", "run"), ("design", DESIGN), ("area", "web")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.into())))
            .collect()
    }

    /// The closed chore, as boss-chore.sh completed its run step.
    fn crawl(output: &str) -> serde_json::Value {
        json!({
            "id": CRAWL,
            "kind": "maintenance-playground-crawl",
            "title": "Playground crawl",
            "status": "closed",
            "metadata": {},
            "steps": [
                { "id": "c-scheduled", "spec_slug": "scheduled", "status": "completed", "metadata": {} },
                { "id": "c-run", "spec_slug": "run", "status": "completed",
                  "metadata": { "result": "failed", "exit_status": "1", "output": output } },
                { "id": "c-failed", "spec_slug": "failed", "status": "completed", "metadata": {} },
            ],
        })
    }

    /// An item a previous night filed, as the jobs API lists it back.
    fn open_item(id: &str, route: &str) -> serde_json::Value {
        json!({
            "id": id,
            "kind": "backlog-item",
            "status": "open",
            "metadata": { "area": "web", "design": DESIGN, RED_ROUTE: route },
            "steps": [],
        })
    }

    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// Stand-in for jobs-api, the shape `ops_judge`'s tests use: `GET
    /// /api/jobs/{id}` serves one, `GET /api/jobs?kind=&status=open`
    /// lists the open ones, `POST /api/jobs` mints an id and keeps the
    /// row open, `PATCH /api/jobs/{id}/metadata` merges — so a second
    /// delivery reads what the first wrote.
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

    fn handler(base: String) -> Arc<ChoreFileReds> {
        ChoreFileReds::with_client(
            reqwest::Client::new(),
            base,
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn posts(w: &[(String, String, serde_json::Value)]) -> Vec<serde_json::Value> {
        w.iter()
            .filter(|(m, _, _)| m == "POST")
            .map(|(_, _, b)| b.clone())
            .collect()
    }

    // -- the pure parser -------------------------------------------------

    /// Three RED lines, two routes: the route that threw twice is ONE
    /// finding with both errors, in the order printed.
    #[test]
    fn red_lines_group_by_route_in_first_seen_order() {
        let reds = red_routes(RED_OUTPUT);
        assert_eq!(reds.len(), 2, "{reds:?}");
        assert_eq!(reds[0].route, "/it/registry/dispatcher");
        assert_eq!(reds[0].findings.len(), 2);
        assert_eq!(reds[0].findings[0].0, "pageerror");
        assert!(
            reds[0].findings[0]
                .1
                .starts_with("Cannot read properties of null (reading 'why')"),
            "the error text copied, not retyped: {:?}",
            reds[0].findings[0].1
        );
        assert_eq!(reds[1].route, "/ux/catalog/models/m-1");
        assert_eq!(
            reds[1].findings,
            vec![(
                "no-shell".to_string(),
                "Timed out 20000ms waiting for expect(locator).toBeVisible()".to_string()
            )]
        );
    }

    /// The summary line, the console.error lines and a clone refusal
    /// are not findings; neither is a `RED` line without the shape.
    #[test]
    fn only_shaped_red_lines_are_findings() {
        assert!(red_routes(NO_RED_OUTPUT).is_empty());
        assert!(red_routes("").is_empty());
        assert!(
            red_routes("REDACTED /x y: z\nRED\nRED /only-route\nRED /r kind with space: e\n")
                .is_empty()
        );
        // Indented (a reporter that nests test stdout) still counts.
        assert_eq!(red_routes("   RED /a pageerror: boom\n")[0].route, "/a");
        // An error with a colon of its own keeps it.
        assert_eq!(
            red_routes("RED /a pageerror: x: y\n")[0].findings[0].1,
            "x: y"
        );
    }

    /// The item names the route on its title and carries the route,
    /// kind, error, design id and the chore as evidence.
    #[test]
    fn the_item_carries_route_error_design_and_source() {
        let red = &red_routes(RED_OUTPUT)[0];
        let body = item_body(
            red,
            CRAWL,
            "maintenance-playground-crawl",
            "Playground crawl",
            DESIGN,
            "web",
            "emp-owner",
            &ctx(),
        );
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(
            body["title"],
            "Playground crawl red: /it/registry/dispatcher (pageerror)"
        );
        assert_eq!(body["priority"], "standard");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m["area"], "web");
        assert_eq!(m["design"], DESIGN);
        assert_eq!(m[RED_ROUTE], "/it/registry/dispatcher");
        assert_eq!(m["red_kind"], "pageerror");
        assert!(
            m["red_error"]
                .as_str()
                .is_some_and(|e| e.starts_with("pageerror: Cannot read properties of null")),
            "{m}"
        );
        assert_eq!(m["source"], CRAWL);
        assert_eq!(m["source_kind"], "maintenance-playground-crawl");
        assert_eq!(m["reporter"], RULE);
        assert!(
            m["detail"].as_str().is_some_and(
                |d| d.contains("0e07ce64") && d.contains("Cannot read properties of null")
            ),
            "{m}"
        );
    }

    // -- the handler -----------------------------------------------------

    /// A red night files one item per route (two, from three lines) and
    /// notes on the chore what was filed.
    #[tokio::test]
    async fn a_red_crawl_files_one_item_per_route_and_notes_the_chore() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 2, "one item per ROUTE, not per line: {w:?}");
        let routes: Vec<&str> = filed
            .iter()
            .map(|f| f["metadata"][RED_ROUTE].as_str().unwrap())
            .collect();
        assert_eq!(
            routes,
            ["/it/registry/dispatcher", "/ux/catalog/models/m-1"]
        );
        assert!(
            filed.iter().all(|f| f["metadata"]["design"] == DESIGN),
            "every item carries the design id"
        );
        let note = w.last().unwrap();
        assert_eq!(
            (note.0.as_str(), note.1.as_str()),
            ("PATCH", &*format!("/api/jobs/{CRAWL}/metadata"))
        );
        let text = note.2[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("2 red route(s)") && text.contains("filed 2 backlog-item(s)"),
            "{text}"
        );
        assert!(
            text.contains("/it/registry/dispatcher → f0000000"),
            "names each filed item by route: {text}"
        );
        assert_eq!(
            note.2["filed"],
            json!([
                "f0000000-0000-0000-0000-000000000002",
                "f0000000-0000-0000-0000-000000000003"
            ])
        );
    }

    /// A route with an open item files nothing for that route — dedup by
    /// route while one is open — and the note says which was skipped.
    #[tokio::test]
    async fn an_open_item_for_the_route_dedups_it() {
        let (base, writes) = mock_jobs(vec![
            crawl(RED_OUTPUT),
            open_item(
                "aaaaaaaa-0000-0000-0000-000000000001",
                "/it/registry/dispatcher",
            ),
        ])
        .await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        let filed = posts(&w);
        assert_eq!(filed.len(), 1, "{w:?}");
        assert_eq!(filed[0]["metadata"][RED_ROUTE], "/ux/catalog/models/m-1");
        let text = w.last().unwrap().2[JUDGED].as_str().unwrap_or("");
        assert!(
            text.contains("filed 1 backlog-item(s)")
                && text.contains("already open [/it/registry/dispatcher]"),
            "{text}"
        );
    }

    /// JetStream is at-least-once: the second delivery reads the note the
    /// first wrote and files nothing; a redelivery whose note never landed
    /// still files nothing, because the routes are open.
    #[tokio::test]
    async fn a_redelivery_files_nothing_twice() {
        let (base, writes) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        let h = handler(base);
        h.invoke(&args(), &ctx()).await.unwrap();
        h.invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(posts(&w).len(), 2, "two routes, filed once: {w:?}");
        assert_eq!(
            w.len(),
            3,
            "two POSTs + one note; the redelivery wrote nothing: {w:?}"
        );
    }

    /// A red for another reason — the clone refused — has no RED line:
    /// nothing is filed, and the chore is still noted as judged so the
    /// closed packet does not read as unread.
    #[tokio::test]
    async fn a_red_without_red_lines_files_nothing_and_says_so() {
        let (base, writes) = mock_jobs(vec![crawl(NO_RED_OUTPUT)]).await;
        handler(base).invoke(&args(), &ctx()).await.unwrap();
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1, "one note, no item: {w:?}");
        assert_eq!(w[0].0, "PATCH");
        let text = w[0].2[JUDGED].as_str().unwrap_or("");
        assert!(text.contains("nothing filed"), "{text}");
        assert!(
            text.contains("no `RED <route> <kind>: <error>` line"),
            "{text}"
        );
    }

    /// Missing args are rule authoring: permanent, never retried.
    #[tokio::test]
    async fn missing_args_are_permanent() {
        let (base, _) = mock_jobs(vec![crawl(RED_OUTPUT)]).await;
        let err = handler(base)
            .invoke(&args()[..2], &ctx())
            .await
            .unwrap_err();
        assert!(err.is_permanent(), "{err}");
    }
}
