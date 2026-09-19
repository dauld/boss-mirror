//! `ops.file_tag_release` — a release packet's `tag` step becoming
//! ready files the `tag-release` ops-request the forge answers.
//!
//! THE LAST HAND ACT ON A RELEASE (backlog 89c95245, left by the
//! builder of 05d301be, 2026-09-18). That car gave the forge a bounded
//! `tag-release` verb (infra/forge/tag-release.sh: version, sha,
//! release packet; one annotated tag, read back) and the rule
//! complete-release-tag-on-tag-release-answered, which copies the
//! read-back onto the cut-a-release packet's `tag` step. What was
//! left was FILING the request: version, sha and packet id typed by
//! the founder into an ops-request — the act David's 2026-09-16 rule
//! ("nothing by hand unless absolutely required") says is the
//! machine's, because every word of it is already on the record.
//!
//! WHY A HANDLER AND NOT `jobs.spawn`. Measured before this was
//! written: the `step.ready.task` payload carries `job_id`, `step_id`,
//! the step KIND and the step's own metadata (the tenant's `procedure`
//! text) — no workflow kind, no spec_slug, no packet metadata — so no
//! `when` can name the release packet, and the version lives on the
//! PACKET (`metadata.version`, "1.2.3", required at admission), not on
//! the step. The sha is the newest closed pr-train's `merge_ref`, a
//! read of a different packet altogether. `jobs.spawn` reads only its
//! args and the payload, and cannot carry an args LIST (its
//! `metadata.*` args are scalars). So this handler: it fetches the
//! packet the step belongs to, checks it IS the packet kind the rule
//! names and the step IS the slug (self-filtering, the way
//! dns.observe requires its packet kind), reads the newest merged
//! train, and files the request with everything the ops-runner needs.
//!
//! WHAT IT FILES. One `ops-request` for host `forge`, verb
//! `tag-release`, args `["v" + version, <merge_ref>, <release packet
//! id>]` and `metadata.release` = the same id — the edge the
//! completing rule follows (`jobs.complete_linked_step`, link
//! `release`). The version is passed with its `v` because the packet
//! records `1.2.3` and the verb names `v1.2.3` (a `v` already there
//! is kept, not doubled). The sha is the record's twelve-char
//! merge_ref, COPIED: the verb resolves it in the converged checkout
//! and refuses a prefix that names none or more than one commit,
//! which is why the verb accepts a prefix at all (same car).
//!
//! WHICH TRAIN. The newest CLOSED pr-train whose `merged` step
//! completed with a `merge_ref` — newest by that step's
//! `completed_at`, the stamp the deploy-convergence sweep reads for
//! the same "when did it land" question. A closed train is one the
//! conductor has finished with; the verb's own bounds (a closed
//! train's merge_ref by prefix-equality, an ancestor of the converged
//! main) hold whatever this picks, so the honest failure of a wrong
//! pick is a refusal on the request, visible on both packets, never a
//! tag in the wrong place. A record with NO such train files nothing
//! and says so as a permanent error, because a release with nothing
//! landed is not a release and a silent skip would leave the step
//! looking like the founder's again.
//!
//! WHAT IT LEAVES ALONE. A task step on any other packet kind (the
//! shared `step.ready.task` topic carries every task step on the
//! board); a `tag` step already answered by an open tag-release
//! request for the same packet (JetStream is at-least-once); a step
//! no longer open.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};

use super::common::{api_client, get_json, open_jobs_of_kind, post_json};

/// The allowlisted verb (infra/ops/verbs/tag-release.json) and the
/// host that answers it.
pub const VERB: &str = "tag-release";
pub const HOST: &str = "forge";
/// The metadata key the completing rule follows back to the release
/// packet (`link = "release"` on
/// complete-release-tag-on-tag-release-answered).
pub const LINK: &str = "release";

pub struct OpsFileTagRelease {
    client: reqwest::Client,
    jobs_base: String,
}

impl OpsFileTagRelease {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

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

fn md<'a>(job: &'a Value, key: &str) -> Option<&'a str> {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn step_by_slug<'a>(job: &'a Value, slug: &str) -> Option<&'a Value> {
    job.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
}

/// PURE: the version the request names — `v` + the packet's
/// `metadata.version`, a `v` already there kept rather than doubled.
pub(crate) fn tag_version(version: &str) -> String {
    let v = version.trim();
    match v.strip_prefix('v') {
        Some(rest) => format!("v{rest}"),
        None => format!("v{v}"),
    }
}

/// PURE: is this the step the rule is for — the packet is of
/// `workflow_kind`, the step `step_id` is its `step_slug`, and it is
/// still open? Answers the packet's version when so.
pub(crate) fn release_wanting_a_tag<'a>(
    job: &'a Value,
    step_id: &str,
    workflow_kind: &str,
    step_slug: &str,
) -> Option<&'a str> {
    if job.get("kind").and_then(Value::as_str) != Some(workflow_kind) {
        return None;
    }
    let step = step_by_slug(job, step_slug)?;
    if step.get("id").and_then(Value::as_str) != Some(step_id) {
        return None;
    }
    if !step
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| matches!(s, "ready" | "active"))
    {
        return None;
    }
    md(job, "version")
}

/// PURE: the newest closed train's merge — `(train id, merge_ref)` —
/// newest by the `merged` step's `completed_at`; a train whose merged
/// step did not complete, or recorded no merge_ref, is not a landing.
pub(crate) fn newest_merged_train(trains: &[Value]) -> Option<(String, String)> {
    trains
        .iter()
        .filter(|t| t.get("status").and_then(Value::as_str) == Some("closed"))
        .filter_map(|t| {
            let merged = step_by_slug(t, "merged")?;
            if merged.get("status").and_then(Value::as_str) != Some("completed") {
                return None;
            }
            let merge_ref = merged
                .pointer("/metadata/merge_ref")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|r| r.len() >= 7 && r.chars().all(|c| c.is_ascii_hexdigit()))?;
            let at = merged
                .pointer("/metadata/completed_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let id = t.get("id").and_then(Value::as_str)?.to_string();
            Some((at, id, merge_ref.to_string()))
        })
        // RFC 3339 UTC stamps order as strings; the max is the newest.
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, id, merge_ref)| (id, merge_ref))
}

/// PURE: an open tag-release request already linked to this release.
pub(crate) fn already_filed(open_requests: &[Value], release_id: &str) -> bool {
    open_requests
        .iter()
        .any(|r| md(r, "verb") == Some(VERB) && md(r, LINK) == Some(release_id))
}

/// PURE: the ops-request body — everything the forge's ops-runner
/// needs (`host`/`verb`/`args`) and the edge the completing rule
/// follows (`release`), plus the train it read for a reader.
pub(crate) fn tag_request(
    version: &str,
    merge_ref: &str,
    train_id: &str,
    release_id: &str,
    rule_name: &str,
    event_id: &str,
    topic: &str,
) -> Value {
    let tag = tag_version(version);
    json!({
        "kind": "ops-request",
        "title": format!("tag-release {tag} at {merge_ref} — release {}", &release_id[..release_id.len().min(8)]),
        "subject": {"subject_kind": "custom", "id": HOST},
        "owner_id": format!("rule:{rule_name}"),
        "priority": "standard",
        "status": "open",
        "tags": ["dispatcher-spawned"],
        "metadata": {
            "host": HOST,
            "verb": VERB,
            "args": [tag, merge_ref, release_id],
            LINK: release_id,
            "train": train_id,
            "spawned_by_rule": rule_name,
            "triggered_by_event_id": event_id,
            "triggered_by_topic": topic,
        },
    })
}

#[async_trait]
impl Handler for OpsFileTagRelease {
    fn name(&self) -> &'static str {
        "ops.file_tag_release"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let workflow_kind = arg_string(args, "workflow_kind")?;
        let step_slug = arg_string(args, "step")?;
        let (Some(job_id), Some(step_id)) = (
            ctx.event_payload.get("job_id").and_then(Value::as_str),
            ctx.event_payload.get("step_id").and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{job_id}", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let Some(version) = release_wanting_a_tag(&job, step_id, workflow_kind, step_slug) else {
            return Ok(());
        };
        let open_requests =
            open_jobs_of_kind(&self.client, self.base(), "ops-request", &ctx.rule_name).await?;
        if already_filed(&open_requests, job_id) {
            return Ok(());
        }
        let trains = get_json(
            &self.client,
            &format!(
                "{}/api/jobs?kind=pr-train&status=closed&limit=200",
                self.base()
            ),
            &ctx.rule_name,
        )
        .await?;
        let trains: Vec<Value> = trains
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let Some((train_id, merge_ref)) = newest_merged_train(&trains) else {
            return Err(HandlerError::Permanent(format!(
                "ops.file_tag_release: no closed pr-train with a completed merged step and a \
                 merge_ref among {} read, so there is no landed commit to tag {} at; the \
                 release packet {job_id}'s {step_slug} step stays open",
                trains.len(),
                tag_version(version)
            )));
        };
        let body = tag_request(
            version,
            &merge_ref,
            &train_id,
            job_id,
            &ctx.rule_name,
            &ctx.triggering_event_id,
            &ctx.triggering_topic,
        );
        post_json(
            &self.client,
            &format!("{}/api/jobs", self.base()),
            &body,
            &ctx.rule_name,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_dispatcher::rules::expr::Value as ExprValue;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const RELEASE: &str = "7d3a9c1e-2b4f-4e6a-9c8d-1f2e3a4b5c6d";
    const TAG_STEP: &str = "step-tag-1";

    fn release(version: &str, tag_status: &str) -> Value {
        json!({
            "id": RELEASE, "kind": "cut-a-release", "status": "open",
            "title": format!("Release {version}"),
            "metadata": {"version": version},
            "steps": [
                {"id": "step-changelog-1", "spec_slug": "changelog", "status": "completed"},
                {"id": TAG_STEP, "spec_slug": "tag", "status": tag_status},
                {"id": "step-mirror-1", "spec_slug": "mirror", "status": "pending"},
            ]
        })
    }

    fn train(id: &str, status: &str, merged_at: Option<&str>, merge_ref: Option<&str>) -> Value {
        let mut m = json!({});
        if let Some(at) = merged_at {
            m["completed_at"] = json!(at);
        }
        if let Some(r) = merge_ref {
            m["merge_ref"] = json!(r);
        }
        json!({
            "id": id, "kind": "pr-train", "status": status,
            "steps": [
                {"spec_slug": "assemble", "status": "completed"},
                {"spec_slug": "merged", "status": if merged_at.is_some() { "completed" } else { "ready" }, "metadata": m},
            ]
        })
    }

    #[test]
    fn the_version_is_named_with_one_v() {
        assert_eq!(tag_version("1.2.3"), "v1.2.3");
        assert_eq!(tag_version("v1.2.3"), "v1.2.3");
        assert_eq!(tag_version(" 10.0.42 "), "v10.0.42");
    }

    /// THE FILTER: only the release packet's own open `tag` step
    /// answers a version; a task step on any other packet, another
    /// step on the same packet, or a step already done, is left alone.
    #[test]
    fn only_the_release_packets_open_tag_step_wants_a_tag() {
        let r = release("1.2.3", "ready");
        assert_eq!(
            release_wanting_a_tag(&r, TAG_STEP, "cut-a-release", "tag"),
            Some("1.2.3")
        );
        assert_eq!(
            release_wanting_a_tag(
                &release("1.2.3", "active"),
                TAG_STEP,
                "cut-a-release",
                "tag"
            ),
            Some("1.2.3")
        );
        assert_eq!(
            release_wanting_a_tag(&r, "step-mirror-1", "cut-a-release", "tag"),
            None,
            "another step on the packet"
        );
        assert_eq!(
            release_wanting_a_tag(&r, TAG_STEP, "publish-to-github", "tag"),
            None,
            "another packet kind"
        );
        assert_eq!(
            release_wanting_a_tag(
                &release("1.2.3", "completed"),
                TAG_STEP,
                "cut-a-release",
                "tag"
            ),
            None,
            "a step already done"
        );
        let mut no_version = release("1.2.3", "ready");
        no_version["metadata"] = json!({});
        assert_eq!(
            release_wanting_a_tag(&no_version, TAG_STEP, "cut-a-release", "tag"),
            None,
            "a packet with no version cannot be tagged"
        );
    }

    /// THE SELECTION: the newest landing by the merged step's stamp,
    /// whatever order the listing came in; an open train, a closed
    /// train that never merged, and one with no usable merge_ref are
    /// not landings.
    #[test]
    fn the_newest_closed_merged_train_is_the_one_tagged() {
        let trains = [
            train(
                "older",
                "closed",
                Some("2026-09-18T20:02:11Z"),
                Some("1e7d7935abcd"),
            ),
            train(
                "open-newer",
                "open",
                Some("2026-09-19T01:00:00Z"),
                Some("ffffffffffff"),
            ),
            train(
                "newest",
                "closed",
                Some("2026-09-19T00:03:40Z"),
                Some("91e3d219f0ab"),
            ),
            train("cancelled", "closed", None, None),
            train("no-ref", "closed", Some("2026-09-19T00:30:00Z"), None),
            train(
                "short-ref",
                "closed",
                Some("2026-09-19T00:40:00Z"),
                Some("abc"),
            ),
        ];
        assert_eq!(
            newest_merged_train(&trains),
            Some(("newest".to_string(), "91e3d219f0ab".to_string()))
        );
        assert_eq!(newest_merged_train(&[]), None);
        assert_eq!(
            newest_merged_train(&[train("cancelled", "closed", None, None)]),
            None
        );
    }

    #[test]
    fn an_open_request_for_the_same_release_is_not_filed_twice() {
        let open = [
            json!({"metadata": {"verb": "tag-release", "release": RELEASE}}),
            json!({"metadata": {"verb": "run-car-probe", "car": "c1"}}),
        ];
        assert!(already_filed(&open, RELEASE));
        assert!(!already_filed(&open[1..], RELEASE));
        assert!(!already_filed(&open, "other-release"));
    }

    /// THE REQUEST: shaped for the ops-runner (host/verb/args) and for
    /// the completing rule (release = the packet id, the same id as
    /// the third arg), with the train it read.
    #[test]
    fn the_request_carries_the_verbs_three_args_and_the_release_edge() {
        let r = tag_request(
            "1.2.3",
            "91e3d219f0ab",
            "train-1",
            RELEASE,
            "file-tag-release-on-release-tag-ready",
            "ev-1",
            "step.ready.task",
        );
        assert_eq!(r["kind"], "ops-request");
        assert_eq!(r["subject"]["id"], "forge");
        assert_eq!(r["metadata"]["host"], "forge");
        assert_eq!(r["metadata"]["verb"], "tag-release");
        assert_eq!(
            r["metadata"]["args"],
            json!(["v1.2.3", "91e3d219f0ab", RELEASE])
        );
        assert_eq!(r["metadata"]["release"], RELEASE);
        assert_eq!(r["metadata"]["train"], "train-1");
        assert_eq!(
            r["metadata"]["spawned_by_rule"],
            "file-tag-release-on-release-tag-ready"
        );
        assert_eq!(r["owner_id"], "rule:file-tag-release-on-release-tag-ready");
        assert!(r["title"].as_str().unwrap().contains("v1.2.3"));
    }

    // --- the handler against a jobs API stand-in -------------------

    type Posts = Arc<Mutex<Vec<Value>>>;

    async fn mock_jobs(jobs: Vec<Value>) -> (String, Posts) {
        use axum::extract::{Path, Query};
        use axum::routing::get;
        use axum::{Json, Router};
        let posts: Posts = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<HashMap<String, Value>> = Arc::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                .collect(),
        );
        let list = by_id.clone();
        let one = by_id.clone();
        let log = posts.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let by_id = list.clone();
                    async move {
                        let rows: Vec<Value> = by_id
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
                .post(move |Json(body): Json<Value>| {
                    let log = log.clone();
                    async move {
                        log.lock().unwrap().push(body);
                        Json(json!({ "id": "req-1" }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = one.clone();
                    async move {
                        by_id
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), posts)
    }

    fn ctx(job_id: &str, step_id: &str) -> InvocationContext {
        InvocationContext {
            rule_name: "file-tag-release-on-release-tag-ready".into(),
            triggering_event_id: "evt-ready-1".into(),
            triggering_topic: "step.ready.task".into(),
            event_payload: json!({
                "job_id": job_id, "step_id": step_id, "kind": "task",
                "subject_kind": "custom", "subject_id": "boss",
                "assignee_id": "emp-david",
                "metadata": {"procedure": "..."},
            }),
        }
    }

    fn args() -> Vec<(String, ExprValue)> {
        vec![
            (
                "workflow_kind".to_string(),
                ExprValue::String("cut-a-release".into()),
            ),
            ("step".to_string(), ExprValue::String("tag".into())),
        ]
    }

    /// END TO END: the `tag` step going ready on a release packet
    /// files ONE tag-release request carrying v<version>, the newest
    /// closed train's merge_ref and the packet id, linked back by
    /// `release`.
    #[tokio::test]
    async fn the_tag_step_going_ready_files_the_tag_release_request() {
        let (base, posts) = mock_jobs(vec![
            release("1.2.3", "ready"),
            train(
                "older",
                "closed",
                Some("2026-09-18T20:02:11Z"),
                Some("1e7d7935abcd"),
            ),
            train(
                "newest",
                "closed",
                Some("2026-09-19T00:03:40Z"),
                Some("91e3d219f0ab"),
            ),
        ])
        .await;
        let h = OpsFileTagRelease::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(RELEASE, TAG_STEP))
            .await
            .expect("runs");
        let filed = posts.lock().unwrap().clone();
        assert_eq!(filed.len(), 1, "{filed:?}");
        assert_eq!(filed[0]["metadata"]["verb"], "tag-release");
        assert_eq!(
            filed[0]["metadata"]["args"],
            json!(["v1.2.3", "91e3d219f0ab", RELEASE])
        );
        assert_eq!(filed[0]["metadata"]["release"], RELEASE);
        assert_eq!(filed[0]["metadata"]["train"], "newest");
    }

    /// The shared topic carries every task step: another packet's
    /// task, and the release's OTHER task steps, file nothing.
    #[tokio::test]
    async fn a_task_step_on_another_packet_or_another_slug_files_nothing() {
        let (base, posts) = mock_jobs(vec![
            release("1.2.3", "ready"),
            json!({"id": "pub-1", "kind": "publish-to-github", "status": "open",
                   "metadata": {}, "steps": [{"id": "s-open-pr", "spec_slug": "open-pr", "status": "ready"}]}),
            train("newest", "closed", Some("2026-09-19T00:03:40Z"), Some("91e3d219f0ab")),
        ])
        .await;
        let h = OpsFileTagRelease::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx("pub-1", "s-open-pr"))
            .await
            .expect("runs");
        h.invoke(&args(), &ctx(RELEASE, "step-mirror-1"))
            .await
            .expect("runs");
        assert!(posts.lock().unwrap().is_empty());
    }

    /// At-least-once delivery: a redelivered ready marker finds the
    /// open request it already filed and files no twin.
    #[tokio::test]
    async fn a_redelivered_marker_does_not_file_a_twin() {
        let (base, posts) = mock_jobs(vec![
            release("1.2.3", "ready"),
            json!({"id": "req-open", "kind": "ops-request", "status": "open",
                   "metadata": {"host": "forge", "verb": "tag-release", "release": RELEASE}}),
            train(
                "newest",
                "closed",
                Some("2026-09-19T00:03:40Z"),
                Some("91e3d219f0ab"),
            ),
        ])
        .await;
        let h = OpsFileTagRelease::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(RELEASE, TAG_STEP))
            .await
            .expect("runs");
        assert!(posts.lock().unwrap().is_empty());
    }

    /// No landed train is a PERMANENT error that names the packet,
    /// never a silent skip: the step would otherwise look like the
    /// founder's again with nothing saying why.
    #[tokio::test]
    async fn no_landed_train_is_a_named_error_not_a_silent_skip() {
        let (base, posts) = mock_jobs(vec![
            release("1.2.3", "ready"),
            train(
                "open-only",
                "open",
                Some("2026-09-19T00:03:40Z"),
                Some("91e3d219f0ab"),
            ),
        ])
        .await;
        let h = OpsFileTagRelease::with_client(reqwest::Client::new(), base);
        let err = h
            .invoke(&args(), &ctx(RELEASE, TAG_STEP))
            .await
            .expect_err("no train to tag at");
        assert!(
            matches!(err, HandlerError::Permanent(ref m) if m.contains("no closed pr-train") && m.contains(RELEASE)),
            "{err:?}"
        );
        assert!(posts.lock().unwrap().is_empty());
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = OpsFileTagRelease::with_client(reqwest::Client::new(), "http://unused");
        assert_eq!(h.name(), "ops.file_tag_release");
        assert_eq!(
            boss_dispatcher::cascade::handler_emits()
                .get("ops.file_tag_release")
                .cloned(),
            Some(vec!["jobs.job.created"]),
            "the cascade table knows what this handler emits"
        );
    }
}
