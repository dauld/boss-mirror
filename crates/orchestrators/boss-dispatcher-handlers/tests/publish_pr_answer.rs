//! `jobs.complete_linked_step` under the rule
//! complete-publish-pr-step-on-publish-github-pr-answered — both legs,
//! against ops-request c98a782f's EXACT recorded output (backlog
//! f47861a5, measured 2026-09-19 on publish 254177e2).
//!
//! The defect: the verb printed `publish-github-pr: FAILED — pushing
//! publish/2026-09-18 to the forge (…) as david: fatal: detected
//! dubious ownership …`, exited 1, the runner completed `execute` with
//! `exit_code: "1"`, the request closed `answered` (the verb ran) and
//! nothing read how it went: the publish's open-pr step sat `ready` for
//! five hours, no packet was filed, and the yard drew the publish like
//! one in progress. Here the rule's args are read from its file, the
//! handler is driven the way the dispatcher drives it, and every write
//! it makes is recorded by a stand-in jobs API.
//!
//! Lives under `tests/` rather than beside the handler because the
//! recorded output names the forge by address, and the-estate-address-
//! lives-once allows a fixture to spell what it stubs only here.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_dispatcher::rules::registry::{Registry, match_event};
use boss_dispatcher_handlers::handlers::jobs_complete_linked_step::JobsCompleteLinkedStep;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const RULE: &str = "complete-publish-pr-step-on-publish-github-pr-answered";
/// The request, the publish, and the step — the ids the measurement
/// was made on.
const REQUEST: &str = "c98a782f-adb4-40a9-860a-456063cfe66a";
const PUBLISH: &str = "254177e2-6c1a-4b7e-9d2f-3a4b5c6d7e8f";
const OPEN_PR: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
const MINTED: &str = "0f1e2d3c-4b5a-4968-8776-655443322110";
const OWNER: &str = "emp-owner";

/// c98a782f's execute-step `output`, byte for byte (the em dashes, the
/// tab git prints before its safe.directory hint, the trailing
/// newline).
const FAILED_OUTPUT: &str = "publish-github-pr: packet 254177e2 — open-pr ready; publishing forge main as publish/2026-09-18\n\
publish-github-pr: snapshot 1a904069d2fa2003c7f731bd987212751e910d02 (tree 01186def6e50833f23d324088d6d0d0002ce4848 of forge 11e3541c2eac3b5ddf0118cce271496c4abc102f, parent mirror 158553d75eef681e2ff0d101e751cb83e0366a10)\n\
publish-github-pr: fork dauld/boss-mirror confirmed in algedonic-dev/boss's network (fork=true parent=algedonic-dev/boss source=algedonic-dev/boss private=?)\n\
publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge (http://10.20.0.15:3000/david/boss.git) as david: fatal: detected dubious ownership in repository at '/var/lib/boss-publish/boss.git' To add an exception for this directory, call:  \tgit config --global --add safe.directory /var/lib/boss-publish/boss.git . Without it on the forge, the push mirror prunes the PR's head at the next train\n";

/// The FAILED line alone — what the step and the alert must carry.
const FAILED_LINE: &str = "publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge (http://10.20.0.15:3000/david/boss.git) as david: fatal: detected dubious ownership in repository at '/var/lib/boss-publish/boss.git' To add an exception for this directory, call:  \tgit config --global --add safe.directory /var/lib/boss-publish/boss.git . Without it on the forge, the push mirror prunes the PR's head at the next train";

const PR_URL: &str = "https://github.com/algedonic-dev/boss/pull/241";

/// The verb's happy path, as infra/forge/publish-github-pr.sh prints it.
fn opened_output() -> String {
    format!(
        "publish-github-pr: packet 254177e2 — open-pr ready; publishing forge main as publish/2026-09-18\n\
         publish-github-pr: pushed publish/2026-09-18 to the forge as david — the mirror carries it\n\
         publish-github-pr: pushed dauld:publish/2026-09-18\n\
         publish-github-pr: opened {PR_URL}\n\
         publish-github-pr: done — {PR_URL} (open-pr on 254177e2 completed; the merge is David's)\n"
    )
}

type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

fn ctx() -> InvocationContext {
    ctx_for(RULE, REQUEST)
}

/// The same close marker for any rule and any answered request — the
/// release leg (v7) closes a different request under a different rule.
fn ctx_for(rule: &str, request_id: &str) -> InvocationContext {
    InvocationContext {
        rule_name: rule.into(),
        triggering_event_id: format!("evt-close-{}", &request_id[..8]),
        triggering_topic: "jobs.job.closed".into(),
        event_payload: json!({
            "id": request_id, "kind": "ops-request", "outcome": "answered",
            "closed_on": "2026-09-18", "parent_step_id": null,
        }),
    }
}

/// The rule's args as the dispatcher hands them over — read from the
/// file, matched against the close marker, never retyped.
fn rule_args() -> Vec<(String, Value)> {
    rule_args_of(RULE)
}

/// The same, for any rule file that reacts to an answered ops-request
/// with this handler — the release rule (v7) is driven from its own
/// file, unedited, because that is the claim: a rule nobody touched
/// inherits the failure mode.
fn rule_args_of(rule: &str) -> Vec<(String, Value)> {
    let path = boss_testing::dispatcher_rules_dir().join(format!("{rule}.toml"));
    let toml =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let reg = Registry::from_toml(&toml).expect("the rule file parses");
    let matched = match_event(&reg, "jobs.job.closed", &ctx().event_payload, &NoHelpers).matched;
    assert_eq!(
        matched.len(),
        1,
        "an answered ops-request fires the rule once"
    );
    let inv = &matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    inv.args.clone()
}

/// The answered request as the runner leaves it: the verb's output and
/// exit on `execute` — the one place the exit is recorded (50fede8b) —
/// and the publish's id under `for_publish` (the filing rule's v2).
fn request(exit: &str, output: &str) -> serde_json::Value {
    json!({
        "id": REQUEST,
        "kind": "ops-request",
        "title": "publish the mirror PR from the forge — a publish was approved",
        "status": "closed",
        "subject": { "subject_kind": "custom", "id": "forge" },
        "metadata": { "host": "forge", "verb": "publish-github-pr",
                      "for_publish": PUBLISH, "outcome": "answered",
                      "spawned_by_rule": "publish-github-pr-on-open-pr-ready" },
        "steps": [
            { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
            { "id": "r-execute", "spec_slug": "execute", "status": "completed",
              "metadata": { "authority_role": "platform-admin", "disposition": "answered",
                            "exit_code": exit, "output": output, "runner_host": "forge" } },
            { "id": "r-answered", "spec_slug": "answered", "status": "completed",
              "metadata": { "outcome_kind": "completed" } },
        ],
    })
}

/// The publish packet at its open-pr step.
fn publish(open_pr_status: &str, open_pr_metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "id": PUBLISH,
        "kind": "publish-to-github",
        "title": "Publish to the public GitHub mirror — 2026-09-18",
        "status": "open",
        "subject": { "subject_kind": "custom", "id": "github-mirror" },
        "metadata": {},
        "steps": [
            { "id": "s-approve", "spec_slug": "approve", "status": "completed",
              "metadata": { "decision": "approved" } },
            { "id": OPEN_PR, "spec_slug": "open-pr", "status": open_pr_status,
              "metadata": open_pr_metadata },
            { "id": "s-pr-opened", "spec_slug": "pr-opened", "status": "pending", "metadata": {} },
        ],
    })
}

/// A stand-in jobs API: GET by id, the open listing (for the alert's
/// dedup), and every write recorded as (method, path, body).
async fn mock_jobs(
    jobs: Vec<serde_json::Value>,
    open_items: Vec<serde_json::Value>,
) -> (String, Writes) {
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    let writes: Writes = Arc::new(Mutex::new(Vec::new()));
    let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
        jobs.into_iter()
            .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
            .collect(),
    ));
    let open = Arc::new(open_items);

    let app = Router::new()
        .route("/api/jobs", {
            let open = open.clone();
            let writes = writes.clone();
            get(move |Query(q): Query<HashMap<String, String>>| {
                let open = open.clone();
                async move {
                    let kind = q.get("kind").cloned().unwrap_or_default();
                    let rows: Vec<serde_json::Value> =
                        open.iter().filter(|j| j["kind"] == kind).cloned().collect();
                    Json(json!({ "data": rows, "total": rows.len() }))
                }
            })
            .post(move |Json(body): Json<serde_json::Value>| {
                let writes = writes.clone();
                async move {
                    writes
                        .lock()
                        .unwrap()
                        .push(("POST".into(), "/api/jobs".into(), body));
                    (
                        axum::http::StatusCode::CREATED,
                        Json(json!({ "id": MINTED })),
                    )
                }
            })
        })
        .route("/api/jobs/{id}", {
            let by_id = by_id.clone();
            get(move |Path(id): Path<String>| {
                let by_id = by_id.clone();
                async move {
                    by_id
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .map(Json)
                        .ok_or(axum::http::StatusCode::NOT_FOUND)
                }
            })
        })
        .route("/api/jobs/{id}/metadata", {
            let writes = writes.clone();
            axum::routing::patch(
                move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().push((
                            "PATCH".into(),
                            format!("/api/jobs/{id}/metadata"),
                            body,
                        ));
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            )
        })
        .route("/api/jobs/{id}/steps/{step_id}", {
            let writes = writes.clone();
            axum::routing::put(
                move |Path((id, step_id)): Path<(String, String)>,
                      Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().push((
                            "PUT".into(),
                            format!("/api/jobs/{id}/steps/{step_id}"),
                            body,
                        ));
                        Json(json!({ "ok": true }))
                    }
                },
            )
        })
        .route("/api/jobs/{id}/steps/{step_id}/metadata", {
            let writes = writes.clone();
            axum::routing::patch(
                move |Path((id, step_id)): Path<(String, String)>,
                      Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().push((
                            "PATCH".into(),
                            format!("/api/jobs/{id}/steps/{step_id}/metadata"),
                            body,
                        ));
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            )
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), writes)
}

fn handler(base: String) -> Arc<JobsCompleteLinkedStep> {
    JobsCompleteLinkedStep::with_client(
        reqwest::Client::new(),
        base,
        Arc::new(boss_core::platform_owner::Fixed(OWNER.into())),
    )
}

fn step_patch(writes: &[(String, String, serde_json::Value)]) -> serde_json::Value {
    step_patch_on(writes, PUBLISH, OPEN_PR)
}

fn step_patch_on(
    writes: &[(String, String, serde_json::Value)],
    job: &str,
    step: &str,
) -> serde_json::Value {
    let path = format!("/api/jobs/{job}/steps/{step}/metadata");
    let found: Vec<_> = writes
        .iter()
        .filter(|(m, p, _)| m == "PATCH" && *p == path)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one annotation on the open step: {writes:?}"
    );
    found[0].2.clone()
}

/// THE FAILED LEG — c98a782f. The step is annotated, never completed;
/// an urgent packet names the verb, the request and the line; and the
/// annotation names the packet, so the step points at its alert.
#[tokio::test]
async fn a_failed_publish_annotates_the_open_step_and_files_an_urgent_item() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "a failed verb completes nothing: {w:?}"
    );

    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one alert filed: {w:?}");
    let item = &posts[0].2;
    assert_eq!(item["kind"], "backlog-item");
    assert_eq!(item["priority"], "urgent");
    assert_eq!(
        item["owner_id"], OWNER,
        "filed to the platform owner the port answers"
    );
    let title = item["title"].as_str().unwrap_or("");
    assert!(
        title.contains("publish-github-pr") && title.contains("FAILED"),
        "the title names the verb and that it failed: {title}"
    );
    assert_eq!(
        item["metadata"]["for_request"], REQUEST,
        "the dedup key: the request judged"
    );
    assert_eq!(item["metadata"]["for_packet"], PUBLISH);
    assert_eq!(item["metadata"]["verb"], "publish-github-pr");
    assert_eq!(item["metadata"]["exit"], "1");
    assert_eq!(
        item["metadata"]["failed"], FAILED_LINE,
        "the line, verbatim"
    );
    let detail = item["metadata"]["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("dubious ownership") && detail.contains("open-pr"),
        "the detail carries the cause and names the step left open: {detail}"
    );

    let note = step_patch(&w);
    assert_eq!(note["failed"], FAILED_LINE);
    assert_eq!(note["failed_exit"], "1");
    assert_eq!(note["failed_source"], REQUEST);
    assert_eq!(
        note["alert"], MINTED,
        "the step points at the packet filed for it"
    );
    assert!(
        !w.iter()
            .any(|(_, p, _)| p == &format!("/api/jobs/{REQUEST}/metadata")),
        "the failure is not a dead-link noop, so no obligation_noop note: {w:?}"
    );
}

/// THE OPENED LEG. The verb completes the step itself on its happy
/// path; when it has not (its own PUT failed after the PR opened, or a
/// redelivery races it), the rule completes open-pr with the url copied
/// from the verb's `opened` line.
#[tokio::test]
async fn an_opened_pr_completes_the_open_pr_step_with_its_url() {
    let (base, writes) = mock_jobs(
        vec![request("0", &opened_output()), publish("ready", json!({}))],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    let puts: Vec<_> = w.iter().filter(|(m, _, _)| m == "PUT").collect();
    assert_eq!(puts.len(), 1, "exactly the open-pr step completed: {w:?}");
    assert_eq!(puts[0].1, format!("/api/jobs/{PUBLISH}/steps/{OPEN_PR}"));
    let body = &puts[0].2;
    assert_eq!(body["status"], "completed");
    assert_eq!(
        body["metadata"]["pr_url"], PR_URL,
        "copied from the verb's line"
    );
    assert_eq!(body["metadata"]["published_by"]["car"], REQUEST);
    assert!(
        !w.iter().any(|(m, _, _)| m == "POST"),
        "nothing to alert: {w:?}"
    );
}

/// A redelivery finds the step already annotated from this request
/// and writes nothing more — no second alert, no second note.
#[tokio::test]
async fn a_failure_already_written_is_not_written_twice() {
    let (base, writes) = mock_jobs(
        vec![
            request("1", FAILED_OUTPUT),
            publish(
                "ready",
                json!({ "failed": FAILED_LINE, "failed_exit": "1",
                        "failed_source": REQUEST, "alert": MINTED }),
            ),
        ],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    assert!(
        writes.lock().unwrap().is_empty(),
        "{:?}",
        writes.lock().unwrap()
    );
}

/// A redelivery after the alert was filed but before the annotation
/// landed (JetStream is at-least-once, and the two writes are two
/// calls) reuses the open alert instead of filing a twin.
#[tokio::test]
async fn a_failure_whose_alert_is_already_open_reuses_it() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![
            json!({ "id": MINTED, "kind": "backlog-item", "status": "open",
                     "metadata": { "for_request": REQUEST } }),
        ],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    let w = writes.lock().unwrap().clone();
    assert!(!w.iter().any(|(m, _, _)| m == "POST"), "no twin: {w:?}");
    assert_eq!(step_patch(&w)["alert"], MINTED);
}

/// The publish already closed (superseded, declined) or its open-pr
/// step never opened: nothing to annotate, nothing to alert — the
/// step the failure would have troubled is not there.
#[tokio::test]
async fn a_failure_against_a_step_that_is_not_open_writes_nothing_on_it() {
    let mut closed = publish("pending", json!({}));
    closed["status"] = json!("closed");
    let (base, writes) = mock_jobs(vec![request("1", FAILED_OUTPUT), closed], vec![]).await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    assert!(
        writes.lock().unwrap().is_empty(),
        "{:?}",
        writes.lock().unwrap()
    );
}

/// A mode this handler does not know is rule authoring — the same on
/// every redelivery, so Permanent.
#[tokio::test]
async fn an_unknown_on_failure_mode_is_a_permanent_error() {
    let (base, _) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let mut a = rule_args();
    for (k, v) in a.iter_mut() {
        if k == "on_failure" {
            *v = Value::String("retry-forever".into());
        }
    }
    let err = handler(base)
        .invoke(&a, &ctx())
        .await
        .expect_err("an unknown mode cannot be retried into a known one");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
}

/// THE DEFAULT (v7). The mode was opt-in when it landed, so it covered
/// the two rules whose author had just been burned by the silence and
/// no other — a rule that asks for nothing still left its step ready
/// and said nothing. Here the publish rule's own args are handed over
/// with `on_failure` REMOVED, and the failed leg must answer exactly as
/// it does with the arg present: the annotation on the step, the urgent
/// packet, and no completion.
#[tokio::test]
async fn a_rule_that_asks_for_no_mode_still_troubles_the_step_it_left_open() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let args: Vec<(String, Value)> = rule_args()
        .into_iter()
        .filter(|(k, _)| k != "on_failure")
        .collect();
    assert!(
        !args.iter().any(|(k, _)| k == "on_failure"),
        "the claim is about a rule that does not name the mode"
    );
    handler(base).invoke(&args, &ctx()).await.expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "a failed verb completes nothing: {w:?}"
    );
    assert_eq!(
        w.iter().filter(|(m, _, _)| m == "POST").count(),
        1,
        "the alert is filed without the rule asking: {w:?}"
    );
    let note = step_patch(&w);
    assert_eq!(note["failed"], FAILED_LINE);
    assert_eq!(note["failed_source"], REQUEST);
}

/// The opt-out. A rule whose linked step is not troubled by its verb
/// failing says so in one word, and gets v5's dead-link note instead:
/// nothing on the step, no alert, the noop recorded on both ends.
#[tokio::test]
async fn a_rule_that_opts_out_keeps_the_dead_link_note() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let mut args = rule_args();
    for (k, v) in args.iter_mut() {
        if k == "on_failure" {
            *v = Value::String("note".into());
        }
    }
    handler(base).invoke(&args, &ctx()).await.expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "POST" || m == "PUT"),
        "no alert, no completion: {w:?}"
    );
    for id in [REQUEST, PUBLISH] {
        assert!(
            w.iter().any(|(m, p, b)| m == "PATCH"
                && p == &format!("/api/jobs/{id}/metadata")
                && b.get("obligation_noop").is_some()),
            "the noop is noted on {id}: {w:?}"
        );
    }
}

/// The release leg's ids and fixtures — a second verb, a second
/// protocol, one shape.
const RELEASE_RULE: &str = "complete-release-tag-on-tag-release-answered";
const TAG_REQUEST: &str = "3a7c1b95-2d4e-4f60-8a1b-7c6d5e4f3a2b";
const RELEASE: &str = "5f4e3d2c-1b0a-4998-8877-665544332211";
const TAG_STEP: &str = "8c7b6a59-4837-4261-95a4-b3c2d1e0f9a8";

/// An answered tag-release request whose verb exited 1, linked to the
/// release packet by the `release` edge its rule follows.
fn tag_request(output: &str) -> serde_json::Value {
    json!({
        "id": TAG_REQUEST, "kind": "ops-request", "status": "closed",
        "title": "tag-release v1.4.0 on forge",
        "subject": { "subject_kind": "custom", "id": "forge" },
        "metadata": { "host": "forge", "verb": "tag-release", "release": RELEASE,
                      "outcome": "answered" },
        "steps": [
            { "id": "t-execute", "spec_slug": "execute", "status": "completed",
              "metadata": { "disposition": "answered", "exit_code": "1",
                            "output": output, "runner_host": "forge" } },
        ],
    })
}

/// The release packet at its `tag` step, open and waiting.
fn release_packet() -> serde_json::Value {
    json!({
        "id": RELEASE, "kind": "cut-a-release", "status": "open",
        "title": "Cut release v1.4.0",
        "subject": { "subject_kind": "custom", "id": "algedonic" },
        "metadata": {},
        "steps": [
            { "id": TAG_STEP, "spec_slug": "tag", "status": "ready", "metadata": {} },
        ],
    })
}

/// THE SECOND VERB, AND THE POINT OF THE DEFAULT (v7). The release
/// rule was written before the failure mode existed and names none, so
/// a tag-release that ran and failed left the release packet's `tag`
/// step `ready` with nothing on it — the measured shape of f47861a5 on
/// another verb, another protocol and another step. Driven from the
/// release rule's own file, unedited.
#[tokio::test]
async fn a_failed_tag_release_troubles_the_release_it_was_filed_for() {
    let failed = "tag-release: forge: no tag v1.4.0 on remote origin\n\
                  tag-release: converged checkout: 7f3a1c2 resolves to 7f3a1c2d4e5f60718293a4b5c6d7e8f901a2b3c4\n\
                  tag-release: FAILED — pushing refs/tags/v1.4.0 to remote origin (as david): remote: the account may not write to this repository. The local tag was removed; the forge holds nothing\n";
    let (base, writes) = mock_jobs(vec![tag_request(failed), release_packet()], vec![]).await;
    handler(base)
        .invoke(
            &rule_args_of(RELEASE_RULE),
            &ctx_for(RELEASE_RULE, TAG_REQUEST),
        )
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "the tag step is not completed — the forge holds nothing: {w:?}"
    );
    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one urgent packet: {w:?}");
    let item = &posts[0].2;
    assert_eq!(item["priority"], "urgent");
    assert_eq!(item["metadata"]["verb"], "tag-release");
    assert_eq!(item["metadata"]["for_request"], TAG_REQUEST);
    assert_eq!(item["metadata"]["step"], "tag");
    let failed_line = item["metadata"]["failed"].as_str().unwrap_or("");
    assert!(
        failed_line.contains("may not write to this repository"),
        "the alert names what failed: {failed_line}"
    );
    let note = step_patch_on(&w, RELEASE, TAG_STEP);
    assert_eq!(note["failed_exit"], "1");
    assert_eq!(note["failed_source"], TAG_REQUEST);
}

/// A REFUSAL BY THE VERB IS A FAILURE OF THE REQUEST TOO — and its
/// verdict must name the refusal, not the epilogue. The forge verbs'
/// `refuse()` prints the reason and then `  Nothing was written.`, and
/// exits 1 like `fail()` does, so the runner records an answered
/// request whose step says exit 1. Picking the last non-empty line
/// would hand the alert `Nothing was written.` — true, and no verdict
/// at all (CLAUDE.md §Diagnosis: a verdict must name what failed).
#[tokio::test]
async fn a_refused_tag_release_is_alerted_by_its_reason_not_its_epilogue() {
    let refused = "tag-release: forge: no tag v1.4.0 on remote origin\n\
                   tag-release: REFUSED — sha e4d5d9816d34 is not the merge commit of any of the 57 closed pr-train packets read\n\
                   tag-release:   Nothing was written.\n";
    let (base, writes) = mock_jobs(vec![tag_request(refused), release_packet()], vec![]).await;
    handler(base)
        .invoke(
            &rule_args_of(RELEASE_RULE),
            &ctx_for(RELEASE_RULE, TAG_REQUEST),
        )
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one urgent packet: {w:?}");
    let failed_line = posts[0].2["metadata"]["failed"].as_str().unwrap_or("");
    assert!(
        failed_line.contains("is not the merge commit"),
        "the alert names the refusal: {failed_line}"
    );
    assert!(
        !failed_line.contains("Nothing was written"),
        "and not the epilogue after it: {failed_line}"
    );
    assert_eq!(step_patch_on(&w, RELEASE, TAG_STEP)["failed"], failed_line);
}
