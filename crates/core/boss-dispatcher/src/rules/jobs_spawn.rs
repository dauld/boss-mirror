//! `jobs.spawn` handler — the load-bearing event-routed spawn handler.
//!
//! Spawning a Job in reaction to an event is a data-driven rule, not a
//! hardcoded branch: the rule registry composes this handler, which
//! turns args into the body of `POST /api/jobs` and stamps actor
//! provenance per the design D2 actor model.
//!
//! Rule shape:
//! ```toml
//! do = [{ handler = "jobs.spawn", args = {
//!   kind = "\"ingredient-restock\"",
//!   subject_kind = "\"vendor\"",
//!   subject = "vendor_for(part_sku)",
//! }}]
//! ```
//!
//! A `metadata.<field>` arg parameterizes the spawned Job's metadata,
//! and may be a LIST (4d53fae2):
//! ```toml
//! do = [{ handler = "jobs.spawn", args = {
//!   kind = "\"ops-request\"",
//!   "metadata.verb" = "\"tag-release\"",
//!   "metadata.args" = "[\"v\" , version, job_id]",
//! }}]
//! ```
//! An ops-request is a verb plus its args, so until the DSL had a list
//! literal a request with args could not be filed by rule at all and
//! was filed by a handler written for the purpose.
//!
//! Required args: `kind`, `subject_kind`, `subject`.
//! Optional args: `title` (defaults to "Auto-spawn from rule
//! `<rule-name>`"), `priority` (defaults to "normal"),
//! `parent_step_id` (the delegate-subjob parent — D7).
//!
//! ## Delegate-subjob linkage (D7)
//!
//! When `parent_step_id` is supplied (the `step.ready.delegate-subjob`
//! rule passes it), the handler:
//!   1. Stamps `parent_step_id` into the spawned (child) Job's
//!      `metadata` — the reverse link the `jobs.subjob_resolve` handler
//!      reads to find the parent step when the child closes.
//!   2. After the spawn POST succeeds, reads the new Job's `id` from
//!      the response and `PUT`s the parent step's `embedded_job` to it
//!      — the forward `Step.embedded_job` link the SPA descends into.
//! The parent *Job* id needed for that PUT comes from the triggering
//! event payload's `job_id` (the `step.ready` marker carries it).

use super::expr::Value;
use super::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct JobsSpawn {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsSpawn {
    /// Construct with the jobs-api base URL (e.g. `http://127.0.0.1:7900`).
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: reqwest::Client::new(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests use this to point
    /// at a wiremock server; production passes a fresh client).
    pub fn with_client(client: reqwest::Client, jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }
}

/// The JSON a `metadata.<field>` arg lands as, or `None` for a value
/// with no JSON form.
///
/// A LIST is the reason this is a function (backlog 4d53fae2). An
/// ops-request is a verb plus its args — `["tag-release", "v1.2.3",
/// "<packet>"]` — and while `metadata.*` took scalars only, a rule
/// could not file one, so the request was filed by a handler written
/// for the purpose. The DSL now has a list literal, and this is where
/// it reaches the spawned Job's metadata.
///
/// `Absent` and `Null` answer `None` and the field is left off
/// entirely, which is what the absent-arg guard in `match_event`
/// already refuses one level up: a key present with a hole in it
/// would read as a real value downstream.
fn metadata_arg_json(v: &Value) -> Option<serde_json::Value> {
    match v {
        Value::String(s) => Some(json!(s)),
        Value::Int(i) => Some(json!(i)),
        Value::Float(x) => Some(json!(x)),
        Value::Bool(b) => Some(json!(b)),
        Value::List(items) => Some(serde_json::Value::Array(
            items.iter().filter_map(metadata_arg_json).collect(),
        )),
        Value::Null | Value::Absent => None,
    }
}

/// The id of the child a delegate-subjob step opens, derived from that
/// step and nothing else (backlog 558396ff).
///
/// The spawn is two writes — POST the child, then PUT the parent step's
/// `embedded_job` — and since car 88123ae0 the PUT is refused 409 when it
/// races a metadata write, writing nothing. The handler errs, the event
/// is NAKed and redelivered, and the POST runs again: with an id the
/// server minted, every refusal was one more child job (conservation).
/// Keyed on the parent step, the re-sent POST names the packet the first
/// one opened, and the jobs API answers it rather than admitting it
/// twice. The step, not the delivery: one delegate-subjob step owns one
/// child, however many times its `step.ready` is read.
fn delegated_child_id(parent_step_id: &str) -> String {
    /// A fixed namespace for these ids, so the same step derives the
    /// same child on every dispatcher and every redelivery.
    const DELEGATED_CHILD: uuid::Uuid =
        uuid::Uuid::from_u128(0x5a5a_d7c1_0000_4000_8000_5583_96ff_0001);
    uuid::Uuid::new_v5(
        &DELEGATED_CHILD,
        format!("delegate-subjob-child:{parent_step_id}").as_bytes(),
    )
    .to_string()
}

#[async_trait]
impl Handler for JobsSpawn {
    fn name(&self) -> &'static str {
        "jobs.spawn"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let subject_kind = arg_string(args, "subject_kind")?;
        let subject = arg_string(args, "subject")?;

        // Optional delegate-subjob (D7) parent link. When present the
        // spawned Job is a child of a delegate-subjob step; we stamp
        // the reverse link into its metadata and later set the parent
        // step's forward `embedded_job` pointer.
        let parent_step_id = match arg(args, "parent_step_id") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                return Err(HandlerError::BadArgType {
                    arg: "parent_step_id".to_string(),
                    expected: "string",
                    got: other.kind(),
                });
            }
        };

        // Build the actor identity per the rule-as-actor model. This
        // string lands in audit_log via the gateway's x-boss-user
        // header → jobs-api → events.JOB_OPENED.
        let actor_id = format!("rule:{}", ctx.rule_name);
        let user_header = json!({
            "id": actor_id,
            "role": "system",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();

        let mut metadata = json!({
            "spawned_by_rule": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "triggered_by_topic": ctx.triggering_topic,
        });
        if let (Some(psid), Some(map)) = (&parent_step_id, metadata.as_object_mut()) {
            // The reverse link: jobs.subjob_resolve gates on this key's
            // presence and uses it to PUT the parent step back to done.
            map.insert("parent_step_id".to_string(), json!(psid));
            // Also stamp the parent *Job* id (carried on the triggering
            // `step.ready` payload) so the resolve handler can address
            // PUT /api/jobs/{parent_job}/steps/{parent_step} without a
            // search — the child knows both ends of the link.
            if let Some(parent_job_id) = ctx.event_payload.get("job_id").and_then(|v| v.as_str()) {
                map.insert("parent_job_id".to_string(), json!(parent_job_id));
            }
        }
        // Merge any `metadata.<field>` args into the Job metadata so a rule
        // can parameterize the spawned Job — e.g. the reorder rule passes
        // `metadata.part_sku` so the restock buys one ingredient and the
        // per-SKU dedup can match an in-flight restock for that SKU.
        if let Some(map) = metadata.as_object_mut() {
            for (k, v) in args {
                if let Some(field) = k.strip_prefix("metadata.")
                    && let Some(jv) = metadata_arg_json(v)
                {
                    map.insert(field.to_string(), jv);
                }
            }
        }

        // The docstring always promised an optional `title` arg; the
        // body ignored it until the design-review spawn needed one (a
        // review titled "Auto-spawn from rule …" in an operator's
        // queue is a label, not a title).
        let title = match arg(args, "title") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => format!("Auto-spawn from rule {}", ctx.rule_name),
        };
        let mut body = json!({
            "kind": kind,
            "subject": {
                "subject_kind": subject_kind,
                "id": subject,
            },
            "title": title,
            "owner_id": actor_id,
            "priority": "standard",
            "status": "open",
            "metadata": metadata,
            "tags": ["dispatcher-spawned"]
        });
        // A delegate-subjob child carries an id derived from its parent
        // step, so the redelivery that follows a refused link below
        // re-sends the SAME packet and the jobs API answers it instead
        // of admitting a second child (558396ff). An ordinary spawn has
        // no step to key on and keeps the server-minted id.
        if let (Some(psid), Some(map)) = (&parent_step_id, body.as_object_mut()) {
            map.insert("id".to_string(), json!(delegated_child_id(psid)));
        }

        let url = format!("{}/api/jobs", self.jobs_base.trim_end_matches('/'));
        // The spawned Job inherits the triggering event's sim-ness.
        // Without this header jobs-api sees no sim chain and records
        // the Job as real — and because a Job's `simulated` bit is
        // immutable once written, a restock spawned by the simulated
        // brewery became a permanent real Job that the epoch trim
        // would never collect.
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-boss-user", user_header.as_str())
            .header("x-sim-origin", crate::dispatcher::sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {body}"
            )));
        }

        // D7 forward link: set the parent step's `embedded_job` to the
        // Job we just created so traversal descends from the
        // delegate-subjob step into the child's step graph. Skipped
        // entirely for ordinary spawns (no parent_step_id arg).
        if let Some(parent_step_id) = parent_step_id {
            // Parse `{ "id": "<uuid>" }` out of the create-job response.
            let created: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| HandlerError::Downstream(format!("spawn response not JSON: {e}")))?;
            let child_job_id = created
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| HandlerError::Downstream("spawn response missing `id`".to_string()))?
                .to_string();

            // The parent *Job* id rides on the triggering event payload
            // (`step.ready.<kind>` carries `job_id`); we need it to
            // address the PUT /api/jobs/{job}/steps/{step}.
            let parent_job_id = ctx
                .event_payload
                .get("job_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    HandlerError::Downstream(
                        "step.ready payload missing job_id; cannot link parent step".to_string(),
                    )
                })?;

            // PATCH-on-PUT: send only `embedded_job`; the jobs-api
            // overlays it onto the current step row and keeps every
            // other field intact (status stays Ready — the step is
            // *waiting* on the child, not done yet).
            let step_url = format!(
                "{}/api/jobs/{}/steps/{}",
                self.jobs_base.trim_end_matches('/'),
                parent_job_id,
                parent_step_id,
            );
            let put_body = json!({ "embedded_job": child_job_id });
            let put_resp = self
                .client
                .put(&step_url)
                .header("Content-Type", "application/json")
                .header("x-boss-user", user_header.as_str())
                .header("x-sim-origin", crate::dispatcher::sim_origin_value())
                .json(&put_body)
                .send()
                .await
                .map_err(|e| HandlerError::Downstream(format!("PUT {step_url}: {e}")))?;
            if !put_resp.status().is_success() {
                let status = put_resp.status();
                let put_text = put_resp.text().await.unwrap_or_default();
                return Err(HandlerError::Downstream(format!(
                    "PUT {step_url} returned {status}: {put_text}"
                )));
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::Value;
    use super::*;

    #[tokio::test]
    async fn rejects_missing_kind_arg() {
        let h = JobsSpawn::new("http://127.0.0.1:1");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "x".into(),
            event_payload: serde_json::json!({}),
        };
        let res = h
            .invoke(
                &[("subject_kind".to_string(), Value::String("vendor".into()))],
                &ctx,
            )
            .await;
        assert!(matches!(res, Err(HandlerError::MissingArg(_))));
    }

    #[tokio::test]
    async fn rejects_wrong_type_arg() {
        let h = JobsSpawn::new("http://127.0.0.1:1");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "x".into(),
            event_payload: serde_json::json!({}),
        };
        let res = h
            .invoke(
                &[
                    ("kind".to_string(), Value::Int(42)),
                    ("subject_kind".to_string(), Value::String("vendor".into())),
                    ("subject".to_string(), Value::String("vnd-1".into())),
                ],
                &ctx,
            )
            .await;
        assert!(matches!(res, Err(HandlerError::BadArgType { .. })));
    }

    #[test]
    fn a_metadata_arg_carries_a_list_whole() {
        // 4d53fae2. An ops-request is a verb plus an args LIST, and a
        // `metadata.*` arg took scalars only — `_ => continue` dropped
        // anything else SILENTLY, so a rule that tried would have
        // filed a request with no args at all. Both halves are pinned:
        // the list arrives as a JSON array, and a shape the DSL cannot
        // produce here is still skipped rather than guessed at.
        assert_eq!(
            metadata_arg_json(&Value::List(vec![
                Value::String("tag-release".into()),
                Value::String("v1.2.3".into()),
                Value::Int(3),
            ])),
            Some(json!(["tag-release", "v1.2.3", 3])),
        );
        assert_eq!(
            metadata_arg_json(&Value::List(Vec::new())),
            Some(json!([])),
            "an empty args list is a verb with no args, not a skip"
        );
        assert_eq!(
            metadata_arg_json(&Value::String("x".into())),
            Some(json!("x"))
        );
        assert_eq!(metadata_arg_json(&Value::Absent), None);
    }

    /// A jobs API that holds packets by id the way the real one does
    /// since 558396ff (an id already held is answered, not re-created),
    /// and refuses the FIRST step PUT the way a PUT racing a metadata
    /// write is refused since car 88123ae0: 409, nothing written.
    #[derive(Default)]
    struct FakeJobsApi {
        children: std::sync::Mutex<Vec<String>>,
        embedded: std::sync::Mutex<Vec<String>>,
    }

    async fn serve(api: Arc<FakeJobsApi>) -> String {
        use axum::extract::{Json, State};
        use axum::http::StatusCode;
        use axum::routing::{post, put};

        async fn create(
            State(api): State<Arc<FakeJobsApi>>,
            Json(body): Json<serde_json::Value>,
        ) -> (StatusCode, Json<serde_json::Value>) {
            // No id on the wire is a fresh packet, as serde's default
            // mints one server-side.
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("minted-{}", api.children.lock().unwrap().len()));
            let mut children = api.children.lock().unwrap();
            if children.contains(&id) {
                return (StatusCode::OK, Json(json!({ "id": id })));
            }
            children.push(id.clone());
            (StatusCode::CREATED, Json(json!({ "id": id })))
        }
        async fn link(
            State(api): State<Arc<FakeJobsApi>>,
            Json(body): Json<serde_json::Value>,
        ) -> StatusCode {
            let mut embedded = api.embedded.lock().unwrap();
            embedded.push(
                body["embedded_job"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
            if embedded.len() == 1 {
                StatusCode::CONFLICT
            } else {
                StatusCode::NO_CONTENT
            }
        }

        let app = axum::Router::new()
            .route("/api/jobs", post(create))
            .route("/api/jobs/{job}/steps/{step}", put(link))
            .with_state(api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_redelivered_delegate_spawn_opens_one_child() {
        // Backlog 558396ff, from the review of car 88123ae0. The spawn
        // POSTs the child, then PUTs the parent step's `embedded_job`.
        // A PUT that races a metadata write is refused 409 and writes
        // nothing; the handler errs, the event is NAKed, JetStream
        // redelivers it, and the POST runs again. With an id the server
        // minted, that was a second child every time.
        let api = Arc::new(FakeJobsApi::default());
        let h = JobsSpawn::new(serve(api.clone()).await);
        let ctx = InvocationContext {
            rule_name: "spawn-subjob-on-delegate-subjob-step-ready".into(),
            triggering_event_id: "evt-ready-1".into(),
            triggering_topic: "step.ready.delegate-subjob".into(),
            event_payload: json!({ "job_id": "parent-job-1", "step_id": "parent-step-1" }),
        };
        let args = [
            ("kind".to_string(), Value::String("equipment-repair".into())),
            ("subject_kind".to_string(), Value::String("asset".into())),
            ("subject".to_string(), Value::String("SYS-42".into())),
            (
                "parent_step_id".to_string(),
                Value::String("parent-step-1".into()),
            ),
        ];

        let first = h.invoke(&args, &ctx).await;
        assert!(
            first.is_err(),
            "precondition: the refused link is an error, so the event is redelivered"
        );
        h.invoke(&args, &ctx)
            .await
            .expect("the redelivery links the child");

        let children = api.children.lock().unwrap().clone();
        assert_eq!(
            children.len(),
            1,
            "one delegate-subjob step, one child — got {children:?}"
        );
        let embedded = api.embedded.lock().unwrap().clone();
        assert_eq!(
            embedded,
            vec![children[0].clone(), children[0].clone()],
            "both link attempts name the one child"
        );
    }

    #[test]
    fn a_child_id_is_derived_from_its_parent_step_alone() {
        // The key is the STEP, not the delivery: a redelivery carries
        // the same event id, but so would nothing else that re-announces
        // the step, and one delegate-subjob step owns one child.
        let a = delegated_child_id("parent-step-1");
        assert_eq!(a, delegated_child_id("parent-step-1"));
        assert_ne!(a, delegated_child_id("parent-step-2"));
        assert!(
            uuid::Uuid::parse_str(&a).is_ok(),
            "a job id is a uuid on the wire: {a}"
        );
    }

    #[tokio::test]
    async fn a_list_is_not_accepted_where_a_string_is_required() {
        // `kind` names a workflow. A list there is an authoring
        // mistake, and the handler must say so rather than stringify
        // it — `arg_string` already refuses every non-string, and this
        // pins that the new variant did not open a hole in it.
        let h = JobsSpawn::new("http://127.0.0.1:1");
        let ctx = InvocationContext {
            rule_name: "test".into(),
            triggering_event_id: "evt-1".into(),
            triggering_topic: "x".into(),
            event_payload: serde_json::json!({}),
        };
        let res = h
            .invoke(
                &[
                    ("kind".to_string(), Value::List(vec![Value::Int(1)])),
                    ("subject_kind".to_string(), Value::String("vendor".into())),
                    ("subject".to_string(), Value::String("vnd-1".into())),
                ],
                &ctx,
            )
            .await;
        assert!(matches!(res, Err(HandlerError::BadArgType { .. })));
    }
}
