//! `jobs.complete_linked_step` — a closing Job completes the open step
//! it was authorized by, on the Job its declared edge names.
//!
//! The gap this closes: `ship-a-change` declares a `backlog_item` job
//! edge at the Job's metadata (migration 104, dialed to `abort` by
//! 105), pointing at the `user-feedback` packet the change answers.
//! Nothing read it. So a change could be scoped, built, gated,
//! reviewed, merged, deployed and observed working while the feedback
//! packet that asked for it sat at `submitted` — sixteen of them, on
//! 2026-08-11. The loop closed only when a person remembered.
//!
//! David ratified the rule this implements: "Once the user feedback
//! results in either a shipped change or some other terminal state, it
//! can be closed without the filer approving." So the system completes
//! the branch step triage opened, and the Workflow's own `ready_when`
//! does the rest — `closed` fires off `steps.investigate.done OR
//! steps.design-review.done OR steps.build.done OR
//! steps.needs-info.done`. Nothing here knows the feedback Job closes;
//! it completes a step, and the state machine draws the conclusion.
//!
//! ## Why this is a handler and not feedback-specific code
//!
//! Every noun is a rule arg. The handler knows "follow the edge named
//! by `link`, complete whichever of the steps named by `steps` is open
//! on the far end, stamp the evidence under `evidence_key`" — which is
//! a shape, not a policy. Which edge, which steps, and which Workflows
//! it applies to are the rule row's business (migration 117). Point it
//! at a different edge and it answers a different obligation.
//!
//! Rule shape:
//! ```toml
//! [[rule]]
//! on_event = "jobs.job.closed"
//! when = "kind = \"ship-a-change\" AND outcome = \"merged\""
//! [[rule.do]]
//! handler = "jobs.complete_linked_step"
//! args = { link = "\"backlog_item\"", steps = "\"investigate,design-review,build\"" }
//! ```
//!
//! ## Idempotence
//!
//! JetStream is at-least-once and the close marker is emitted from
//! three sites, so this WILL run more than once for one merge. Three
//! guards, cheapest first:
//!
//! 1. The linked Job is already closed → nothing open to complete.
//! 2. No step named by `steps` is `ready`/`active` → the branch was
//!    already completed (by us on the first delivery, or by a person).
//!    A completed step is never re-completed, so no second
//!    `step.done.*` marker fires and no second re-evaluation runs.
//! 3. The step already carries this car's id under `evidence_key` →
//!    belt to (2)'s braces, and the stamp that makes the write
//!    self-describing about which delivery wrote it.

use super::common::{dispatcher_actor_header, dispatcher_reader_header, sim_origin_value};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use serde_json::json;
use std::sync::Arc;

/// Default metadata key the arrival evidence lands under on the
/// completed step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "arrived_from";

/// Step statuses that mean "open" — the branch triage actually opened
/// is `ready` (nobody claimed it) or `active` (someone did). A
/// `pending` branch is one the disposition did NOT open, and
/// completing it would fabricate work that was never routed.
const OPEN_STATUSES: [&str; 2] = ["ready", "active"];

pub struct JobsCompleteLinkedStep {
    client: reqwest::Client,
    jobs_base: String,
}

impl JobsCompleteLinkedStep {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
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

    /// Record on the CAR that its `backlog_item` pointed somewhere the
    /// obligation could not act.
    ///
    /// The write goes through `PATCH /api/jobs/{id}/metadata` — the
    /// door built for a partial metadata write, merge semantics, and
    /// it works on a closed packet (the car always IS closed here).
    /// The first version PUT `/api/jobs/{id}` with a metadata-only
    /// body, which `Json<Job>` 422s at the extractor for its ten
    /// missing required fields — so the note never landed once, and
    /// the failure drowned in a dispatcher warn (c65110d6).
    async fn note_on_car(
        &self,
        car_id: &str,
        packet_id: &str,
        why: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        // Idempotent under redelivery: the same car, the same packet,
        // the same note. The PATCH merge would write the same value
        // harmlessly, but each write is an audit event — one is truth,
        // three are noise.
        let car = self.get_job(car_id, rule).await?;
        if car
            .get("metadata")
            .and_then(|m| m.get("obligation_noop"))
            .and_then(|n| n.get("packet"))
            .and_then(|v| v.as_str())
            == Some(packet_id)
        {
            return Ok(());
        }
        let url = format!(
            "{}/api/jobs/{}/metadata",
            self.jobs_base.trim_end_matches('/'),
            car_id
        );
        let resp = self
            .client
            .patch(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({
                "obligation_noop": { "packet": packet_id, "rule": rule, "why": why }
            }))
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PATCH {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PATCH {url} returned {status}: {text}"
            )));
        }
        Ok(())
    }

    async fn get_job(&self, job_id: &str, rule: &str) -> Result<serde_json::Value, HandlerError> {
        let url = format!(
            "{}/api/jobs/{}",
            self.jobs_base.trim_end_matches('/'),
            job_id
        );
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} (rule {rule}): {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} response not JSON: {e}")))
    }
}

/// The generation a train carried, out of its `deployed` step's
/// evidence (`main@<sha>; …`). `None` when the summary is absent or
/// shaped differently — evidence never guesses.
///
/// The `main@<sha>` shape is the conductor's, and `boss-cli`'s
/// arrival report parses it the same way. Two readers of one written
/// format that cannot be collapsed today (the other lives in a binary
/// crate's private fn), so it is pinned by a test here instead
/// (CLAUDE.md §9a).
fn deployed_generation(summary: &str) -> Option<&str> {
    summary
        .strip_prefix("main@")
        .and_then(|rest| rest.split([';', ' ']).next())
        .filter(|sha| !sha.is_empty())
}

/// Why did the obligation complete nothing, and is that worth saying?
///
/// Falling through GUARD 2 covers two situations that look identical
/// from inside the handler and want opposite treatment.
///
/// A REDELIVERY finds the branch it already completed. JetStream is
/// at-least-once and `jobs.job.closed` has three emit sites, so this
/// is the common case and must stay silent — a warning per redelivery
/// is a warning nobody reads.
///
/// A CAR NAMING A PACKET WITH NO ACTIONABLE STEP is a different fact
/// and currently produces nothing at all. `complete-feedback-branch-on-car-merged`
/// names investigate / design-review / build; `triage` is deliberately
/// not among them, because choosing a disposition is the routing
/// decision the whole protocol exists to record and an obligation must
/// not make it. So a car whose `backlog_item` points at an untriaged
/// packet merges, does its work, and the packet stays exactly where it
/// was — with no record on either side that anything was attempted.
/// Observed on 3adc2c49, David's own directive to retire
/// emp-bootstrap-admin: car 80345764 named it, merged in PR40, and the
/// packet was still sitting at `triage` an hour later (c80f08b8).
///
/// The distinction is mechanical: if any named step exists and has
/// already been completed, the work was done. If none of the named
/// steps is open AND the packet still has some OTHER step open, the
/// link pointed somewhere the obligation cannot act.
fn noop_reason(target: &serde_json::Value, allowed: &[&str]) -> Option<String> {
    let named: Vec<&serde_json::Value> = allowed
        .iter()
        .filter_map(|slug| step_by_slug(target, slug))
        .collect();

    // A named step is OPEN: there was something to do and the caller
    // does it, so reaching here would be a bug in the caller rather
    // than a dead link. Checked first so this function is honest on
    // its own — the caller's control flow is not part of its contract,
    // and a predicate that only tells the truth from one call site is
    // one that lies at the next.
    if named.iter().any(|s| {
        s.get("status")
            .and_then(|v| v.as_str())
            .is_some_and(|st| OPEN_STATUSES.contains(&st))
    }) {
        return None;
    }

    // The work was already done — by an earlier delivery, or by a
    // person. Nothing to say.
    if named
        .iter()
        .any(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
    {
        return None;
    }

    // What IS open on the packet? If nothing, the packet is between
    // states and a later event will carry this; silence is right.
    let open: Vec<&str> = target
        .get("steps")
        .and_then(|v| v.as_array())
        .map(|steps| {
            steps
                .iter()
                .filter(|s| {
                    s.get("status")
                        .and_then(|v| v.as_str())
                        .is_some_and(|st| OPEN_STATUSES.contains(&st))
                })
                .filter_map(|s| s.get("spec_slug").and_then(|v| v.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if open.is_empty() {
        return None;
    }

    Some(format!(
        "no actionable step: this obligation completes one of [{}], and the packet's open \
         step{} [{}] — most often because nobody has triaged it yet, and triage is a routing \
         decision an obligation must not make",
        allowed.join(", "),
        if open.len() == 1 { " is" } else { "s are" },
        open.join(", "),
    ))
}

/// Find a Job's step by `spec_slug` — the stable machine-facing
/// identifier, distinct from the rendered `title`.
fn step_by_slug<'a>(job: &'a serde_json::Value, slug: &str) -> Option<&'a serde_json::Value> {
    job.get("steps")?
        .as_array()?
        .iter()
        .find(|s| s.get("spec_slug").and_then(|v| v.as_str()) == Some(slug))
}

/// Can this string possibly name a Job, or is following it a wasted
/// request that ends in a dead letter?
///
/// `GET /api/jobs/{id}` requires a full UUID and answers anything else
/// with `400 invalid job id`. A 400 is not a transient failure, so the
/// runner's redelivery does not help: it NAKs eight times and drops the
/// event — and dropping it takes every OTHER handler's effect on that
/// event with it, which is a large blast radius for one bad string.
///
/// This is not hypothetical. `job_edges` documents that a stored edge
/// may be a `>= 8-char` prefix, and seven cars stored one. When train
/// 20260815-0621 merged, car bc6c061a's `backlog_item` — the string
/// `bb86d687` — did exactly the above (finding `d99b310d`).
///
/// So an unusable link is SKIPPED and said out loud, not retried. The
/// obligation cannot be discharged against a Job nobody can identify,
/// and pretending a retry might fix it only delays the same answer by
/// eight deliveries. 136-job-edges-backfill.sql normalises the stored
/// rows and 125's trigger stops new ones; this makes the handler
/// correct on its own rather than merely protected by them.
pub(crate) fn unusable_link(id: &str) -> Option<String> {
    let ok = id.len() == 36
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    (!ok).then(|| {
        format!(
            "link {id:?} is not a full Job id ({} chars) — skipping rather than              dead-lettering the event; the edge needs normalising",
            id.len()
        )
    })
}

#[async_trait]
impl Handler for JobsCompleteLinkedStep {
    fn name(&self) -> &'static str {
        "jobs.complete_linked_step"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let link = arg_string(args, "link")?;
        let allowed: Vec<&str> = arg_string(args, "steps")?
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if allowed.is_empty() {
            return Err(HandlerError::MissingArg("steps".to_string()));
        }
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };

        // The `jobs.job.closed` payload carries the closing Job's id.
        // A malformed marker is not something a redelivery can fix, so
        // it is a no-op rather than an error that retries forever.
        let Some(closing_id) = ctx.event_payload.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        let closing = self.get_job(closing_id, &ctx.rule_name).await?;
        let closing_meta = closing.get("metadata").cloned().unwrap_or(json!({}));

        // No declared edge → no obligation. This is the legacy /
        // free-text case: a car whose motivating item is named only in
        // `backlog_text` prose, or one filed against nothing at all.
        // Both ship exactly as before.
        let Some(target_id) = closing_meta
            .get(link)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };

        // A link that cannot name a Job is a skip, not a failure —
        // see `unusable_link`. Retrying a 400 costs eight deliveries
        // and then drops the event for every handler on it.
        if let Some(why) = unusable_link(target_id) {
            tracing::warn!(rule = %ctx.rule_name, link = %link, "{why}");
            return Ok(());
        }

        let target = self.get_job(target_id, &ctx.rule_name).await?;

        // GUARD 1 — a packet that already reached a terminal is
        // untouched. Its filer got their answer from whatever closed
        // it; re-opening that decision is not this handler's business.
        let target_status = target.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(target_status, "closed" | "cancelled") {
            return Ok(());
        }

        // GUARD 2 — the open branch, or nothing. Exactly one of the
        // named steps is open on a live packet (the fork's `ready_when`
        // guarantees it: each branch gates on a different disposition
        // value). A re-delivery finds the branch already `completed`
        // and falls out here.
        let Some(step) = allowed
            .iter()
            .filter_map(|slug| step_by_slug(&target, slug))
            .find(|s| {
                s.get("status")
                    .and_then(|v| v.as_str())
                    .is_some_and(|st| OPEN_STATUSES.contains(&st))
            })
        else {
            // Silent on a redelivery; loud when the link pointed at a
            // packet this obligation cannot act on. See `noop_reason`.
            if let Some(why) = noop_reason(&target, &allowed) {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    car = %closing_id,
                    packet = %target_id,
                    "obligation completed nothing — {why}"
                );
                // A dispatcher log line is not something a car author
                // reads, so the note also lands on the car — the side
                // that made the claim, and the side a reviewer opens.
                // Best-effort: failing to annotate must not fail the
                // obligation, which has already done all it can.
                if let Err(e) = self
                    .note_on_car(closing_id, target_id, &why, &ctx.rule_name)
                    .await
                {
                    tracing::warn!(rule = %ctx.rule_name, car = %closing_id,
                        "could not record the no-op note: {e}");
                }
            }
            return Ok(());
        };
        let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };

        // PATCH-on-PUT replaces top-level `metadata` wholesale, so the
        // write merges into the step's existing keys — `authority_role`
        // lives there and is what keeps the step gated.
        let mut merged = match step.get("metadata").cloned() {
            Some(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };

        // GUARD 3 — already stamped by this same car. Cheap, and it
        // makes the write self-describing about which delivery wrote
        // it.
        if merged
            .get(evidence_key)
            .and_then(|e| e.get("car"))
            .and_then(|v| v.as_str())
            == Some(closing_id)
        {
            return Ok(());
        }

        // The rule row's translation of "this shipped" into the step
        // kind's own completion vocabulary (0ab5fa3a, accepted (a)).
        // user-feedback v11 makes design-review an `answer-question`
        // step, whose `verdict` + `answer` are required at done — and
        // this handler used to write only evidence, so its completion
        // would 400 and the feedback loop would break exactly where it
        // was fixed. `done_metadata` is a JSON object on the rule row:
        // the TRANSLATION IS DATA, the handler stays generic. String
        // values substitute {branch}/{car}/{title} from facts already
        // in hand. Fills ABSENT keys only — metadata a person already
        // wrote is their record, not this obligation's to overwrite.
        if let Some(Value::String(tpl)) = arg(args, "done_metadata") {
            match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(tpl) {
                Ok(done) => {
                    let branch = closing_meta
                        .get("branch")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no branch recorded)");
                    let title = closing.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    for (k, v) in done {
                        if merged.contains_key(&k) {
                            continue;
                        }
                        let v = match v {
                            serde_json::Value::String(s) => serde_json::Value::String(
                                s.replace("{branch}", branch)
                                    .replace("{car}", closing_id)
                                    .replace("{title}", title),
                            ),
                            other => other,
                        };
                        merged.insert(k, v);
                    }
                }
                // Bad rule authoring is permanent — redelivery cannot
                // fix a malformed template, and dying here would also
                // kill the evidence write below.
                Err(e) => {
                    tracing::warn!(
                        rule = %ctx.rule_name,
                        "done_metadata is not a JSON object ({e}) — completing with evidence only"
                    );
                }
            }
        }

        // The evidence. "The work you asked for shipped" is only worth
        // saying if it names WHAT shipped — an id and a title a reader
        // can go look at, plus the train that carried it and the
        // generation that generation landed in when those are
        // reachable. Absent facts are null, never invented.
        let train_id = closing_meta
            .get("train")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        // Best-effort: a train we cannot read costs us the generation,
        // not the obligation. The packet still gets completed.
        let generation = match train_id {
            Some(t) => self
                .get_job(t, &ctx.rule_name)
                .await
                .ok()
                .as_ref()
                .and_then(|train| step_by_slug(train, "deployed"))
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("deployed"))
                .and_then(|v| v.as_str())
                .and_then(deployed_generation)
                .map(str::to_string),
            None => None,
        };
        merged.insert(
            evidence_key.to_string(),
            json!({
                "car": closing_id,
                "title": closing.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                // The car's BRANCH, from its metadata — not its
                // subject. Until 2026-08-17 this read `subject.id`, so
                // the evidence on every closed packet named the wrong
                // thing plausibly enough to read past: `9150dc6b` and
                // `865992c1` both closed saying `branch: "bosspipeline"`
                // (the subject) instead of `fix/publish-car-real-head`
                // and `fix/gate-headroom-guard`, and `bcaf4a54` said
                // `infra/forge/reap-dead-ci-jobs.sh` because that
                // packet's subject was a file path. A wrong-but-
                // believable evidence field is worse than a missing
                // one: closing on evidence is supposed to save the
                // next reader from re-deriving it.
                "branch": closing_meta.get("branch").and_then(|v| v.as_str()),
                "outcome": ctx.event_payload.get("outcome").and_then(|v| v.as_str()),
                "closed_on": ctx.event_payload.get("closed_on").cloned(),
                "train": train_id,
                "generation": generation,
                "by_rule": ctx.rule_name,
            }),
        );

        let step_url = format!(
            "{}/api/jobs/{}/steps/{}",
            self.jobs_base.trim_end_matches('/'),
            target_id,
            step_id,
        );
        let body = json!({
            "status": "completed",
            "metadata": serde_json::Value::Object(merged),
        });
        let resp = self
            .client
            .put(&step_url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(&ctx.rule_name))
            .header("x-sim-origin", sim_origin_value())
            .json(&body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("PUT {step_url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "PUT {step_url} returned {status}: {text}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, routing::get};
    use std::sync::Mutex;

    #[test]
    fn a_full_uuid_is_followed() {
        assert_eq!(unusable_link("bb86d687-4e08-4a55-8b0e-1e0e63d6bb5f"), None);
    }

    #[test]
    fn an_eight_char_prefix_is_skipped_not_retried() {
        // The real one: car bc6c061a's backlog_item, which NAKed eight
        // times and dead-lettered the event when train 20260815-0621
        // merged.
        let why = unusable_link("bb86d687").expect("a prefix cannot name a Job");
        assert!(why.contains("skipping"), "the message says what it did");
        assert!(why.contains("normalis"), "and what would fix it");
    }

    #[test]
    fn near_misses_are_skipped_too() {
        // Right length, wrong shape; and right shape, wrong alphabet.
        assert!(unusable_link("bb86d687-4e08-4a55-8b0e-1e0e63d6bb5").is_some());
        assert!(unusable_link("zz86d687-4e08-4a55-8b0e-1e0e63d6bb5f").is_some());
        assert!(unusable_link("").is_some());
    }

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "complete-feedback-branch-on-car-merged".into(),
            triggering_event_id: "evt-close-1".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    fn args() -> Vec<(String, Value)> {
        vec![
            ("link".to_string(), Value::String("backlog_item".into())),
            (
                "steps".to_string(),
                Value::String("investigate,design-review,build".into()),
            ),
        ]
    }

    /// v2 rule args (migration 150): the completion vocabulary rides
    /// as data.
    fn args_with_done_metadata() -> Vec<(String, Value)> {
        let mut a = args();
        a.push((
            "done_metadata".to_string(),
            Value::String(
                r#"{"verdict": "approved", "answer": "shipped: {branch} — {title}"}"#.into(),
            ),
        ));
        a
    }

    const CAR: &str = "11111111-1111-1111-1111-111111111111";
    const PACKET: &str = "22222222-2222-2222-2222-222222222222";
    const TRAIN: &str = "33333333-3333-3333-3333-333333333333";
    const BRANCH_STEP: &str = "44444444-4444-4444-4444-444444444444";

    fn car(metadata: serde_json::Value) -> serde_json::Value {
        json!({
            "id": CAR,
            "kind": "ship-a-change",
            "title": "Close the feedback loop",
            "status": "closed",
            // The subject is NOT the branch, and this fixture used to
            // pretend it was — its subject id read "feat/feedback-
            // obligation", which made the evidence binding look right
            // while it read `subject.id`. A fixture whose two fields
            // are indistinguishable cannot catch them being confused.
            "subject": { "subject_kind": "custom", "id": "bosspipeline" },
            "metadata": metadata,
            "steps": [],
        })
    }

    /// A live feedback packet whose triage routed to `build`, so the
    /// `build` branch is ready and the others stayed pending.
    fn packet(build_status: &str) -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "title": "Feedback on /system/flow",
            "status": if build_status == "completed" { "closed" } else { "open" },
            "metadata": { "submitted_by": "emp-bootstrap-admin" },
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                  "metadata": { "disposition": "build" } },
                { "id": "s-investigate", "spec_slug": "investigate", "status": "pending",
                  "metadata": {} },
                { "id": BRANCH_STEP, "spec_slug": "build", "status": build_status,
                  "metadata": { "authority_role": "platform-admin" } },
            ],
        })
    }

    fn train() -> serde_json::Value {
        json!({
            "id": TRAIN,
            "kind": "pr-train",
            "title": "train/2026-08-13-pm",
            "status": "closed",
            "steps": [
                { "id": "t-deployed", "spec_slug": "deployed", "status": "completed",
                  "metadata": { "deployed": "main@abc1234; playground" } },
            ],
        })
    }

    type Puts = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: serves the three Jobs by id, records
    /// every step PUT, and records every job-metadata PATCH — the
    /// noop-note write the first mock had no route for, which is how
    /// a write that 422'd in production passed every test (c65110d6).
    async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Puts, Puts) {
        let patches: Puts = Arc::new(Mutex::new(Vec::new()));
        let puts: Puts = Arc::new(Mutex::new(Vec::new()));
        let by_id: std::collections::HashMap<String, serde_json::Value> = jobs
            .into_iter()
            .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
            .collect();

        let get_puts = puts.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id.clone();
                    async move {
                        by_id
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
                    move |Path((_id, step_id)): Path<(String, String)>,
                          Json(body): Json<serde_json::Value>| {
                        let puts = get_puts.clone();
                        async move {
                            puts.lock().unwrap().push((step_id, body));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route("/api/jobs/{id}/metadata", {
                let patches = patches.clone();
                axum::routing::patch(
                    move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                        let patches = patches.clone();
                        async move {
                            patches.lock().unwrap().push((id, body));
                            axum::http::StatusCode::NO_CONTENT
                        }
                    },
                )
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts, patches)
    }

    fn close_marker() -> serde_json::Value {
        json!({
            "id": CAR,
            "kind": "ship-a-change",
            "outcome": "merged",
            "closed_on": "2026-08-13",
            "parent_step_id": null,
        })
    }

    /// A live packet nobody has triaged: the routing step is open and
    /// every branch this obligation may complete is still `pending`.
    fn untriaged_packet() -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "backlog-item",
            "title": "A defect a car claims to fix",
            "status": "open",
            "metadata": {},
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "ready", "metadata": {} },
                { "id": "s-investigate", "spec_slug": "investigate", "status": "pending",
                  "metadata": {} },
                { "id": BRANCH_STEP, "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// c65110d6: the note saying "this obligation completed nothing"
    /// must actually LAND on the car. It never did — the first
    /// `note_on_car` PUT `/api/jobs/{id}` with a metadata-only body,
    /// which the real extractor 422s for ten missing Job fields, and
    /// the first mock had no job-PUT route, so no test watched the
    /// write fail. The note rides the metadata PATCH door now, and
    /// this test is the route's first witness.
    #[tokio::test]
    async fn an_untriaged_packet_notes_the_noop_on_the_car() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            untriaged_packet(),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");

        assert!(
            puts.lock().unwrap().is_empty(),
            "no step completed — triage is a routing decision an obligation must not make"
        );
        let patches = patches.lock().unwrap().clone();
        assert_eq!(
            patches.len(),
            1,
            "the noop note lands exactly once: {patches:?}"
        );
        let (id, body) = &patches[0];
        assert_eq!(
            id, CAR,
            "the note lands on the CAR — the side that made the claim"
        );
        assert_eq!(body["obligation_noop"]["packet"], PACKET);
        assert!(
            body["obligation_noop"]["why"]
                .as_str()
                .unwrap_or_default()
                .contains("triage"),
            "the why names the open step a reader should look at"
        );
    }

    /// The obligation itself: a merged car completes the branch its
    /// packet's triage opened, carrying evidence that names the car.
    #[tokio::test]
    async fn a_merged_car_completes_the_open_branch_with_its_evidence() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "feat/feedback-obligation" })),
            packet("ready"),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "exactly one step completed: {calls:?}");
        let (step_id, body) = &calls[0];
        assert_eq!(step_id, BRANCH_STEP, "the `build` branch is the open one");
        assert_eq!(body["status"], "completed");

        let evidence = &body["metadata"]["arrived_from"];
        assert_eq!(evidence["car"], CAR, "evidence must name the car");
        assert_eq!(
            evidence["title"], "Close the feedback loop",
            "evidence must carry a title a reader can act on"
        );
        assert_eq!(evidence["train"], TRAIN);
        assert_eq!(
            evidence["branch"], "feat/feedback-obligation",
            "evidence must name the car's BRANCH from its metadata, not its subject — \
             three packets closed in one day naming the subject instead"
        );
        assert_eq!(
            evidence["generation"], "abc1234",
            "the generation the train carried is reachable: {evidence:#}"
        );
        // The step's own metadata survives the write — PATCH-on-PUT
        // replaces `metadata` wholesale, and `authority_role` living
        // there is what keeps the step gated.
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
    }

    /// v2's `done_metadata` (0ab5fa3a): the completion carries the
    /// step kind's required vocabulary, with the car's facts
    /// substituted — a v11 `answer-question` design-review would 400
    /// an evidence-only completion.
    #[tokio::test]
    async fn done_metadata_fills_the_kinds_vocabulary_with_the_cars_facts() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "feat/feedback-obligation" })),
            packet("ready"),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args_with_done_metadata(), &ctx(close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        let (_, body) = &calls[0];
        assert_eq!(body["metadata"]["verdict"], "approved");
        assert_eq!(
            body["metadata"]["answer"],
            "shipped: feat/feedback-obligation — Close the feedback loop",
            "the answer names WHAT shipped, substituted from the car"
        );
        // The evidence write is unchanged beside it.
        assert_eq!(body["metadata"]["arrived_from"]["car"], CAR);
    }

    /// Absent keys only: a verdict a person already recorded is their
    /// decision, and the obligation must not restate it.
    #[tokio::test]
    async fn done_metadata_never_overwrites_what_a_person_wrote() {
        let mut p = packet("ready");
        // The open branch already carries an operator's own verdict.
        let steps = p["steps"].as_array_mut().unwrap();
        for s in steps.iter_mut() {
            if s["id"] == BRANCH_STEP {
                s["metadata"]["verdict"] = json!("declined");
            }
        }
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "feat/x" })),
            p,
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args_with_done_metadata(), &ctx(close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        let (_, body) = &calls[0];
        assert_eq!(
            body["metadata"]["verdict"], "declined",
            "the person's verdict survives the obligation"
        );
        assert_eq!(
            body["metadata"]["answer"], "shipped: feat/x — Close the feedback loop",
            "keys the person did NOT write still fill"
        );
    }

    /// A car with no linked feedback is a no-op — the legacy /
    /// free-text case (`backlog_text` prose, or nothing at all) ships
    /// exactly as it did before.
    #[tokio::test]
    async fn a_merged_car_with_no_linked_packet_is_a_no_op() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_text": "David asked for this in chat" })),
            packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "nothing to complete without a declared edge"
        );
    }

    /// A packet that already reached a terminal is untouched. Its
    /// filer got their answer from whatever closed it.
    #[tokio::test]
    async fn an_already_terminal_packet_is_untouched() {
        let mut closed = packet("ready");
        closed["status"] = json!("closed");
        let (base, puts, _) = mock_jobs(vec![car(json!({ "backlog_item": PACKET })), closed]).await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(puts.lock().unwrap().is_empty(), "a closed packet is done");
    }

    /// Redelivery is at-least-once and the close marker has three emit
    /// sites, so this WILL run twice. The second run finds the branch
    /// already completed and writes nothing — no second `step.done`
    /// marker, no second re-evaluation.
    #[tokio::test]
    async fn a_rerun_against_a_completed_branch_writes_nothing() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET })),
            packet("completed"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "a completed branch is never re-completed"
        );
    }

    /// The evidence stamp is the second guard: a branch still showing
    /// open but already carrying THIS car's stamp (a redelivery that
    /// raced the projection) writes nothing either.
    #[tokio::test]
    async fn a_branch_already_stamped_by_this_car_writes_nothing() {
        let mut stamped = packet("ready");
        stamped["steps"][2]["metadata"]["arrived_from"] = json!({ "car": CAR });
        let (base, puts, _) =
            mock_jobs(vec![car(json!({ "backlog_item": PACKET })), stamped]).await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(puts.lock().unwrap().is_empty(), "already stamped by us");
    }

    /// Only the branch triage OPENED gets completed. A `pending`
    /// branch is one the disposition did not route to, and completing
    /// it would fabricate work that was never assigned.
    #[tokio::test]
    async fn a_pending_branch_is_never_completed() {
        let mut nothing_open = packet("pending");
        nothing_open["status"] = json!("open");
        let (base, puts, _) =
            mock_jobs(vec![car(json!({ "backlog_item": PACKET })), nothing_open]).await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "no branch is open; nothing was routed here"
        );
    }

    /// An unreachable train costs the generation, never the
    /// obligation. The packet still gets its answer.
    #[tokio::test]
    async fn an_unreachable_train_still_completes_the_branch() {
        // The train Job is simply absent from the mock's roster.
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN })),
            packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base);
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "the branch still completes");
        assert!(
            calls[0].1["metadata"]["arrived_from"]["generation"].is_null(),
            "an unreadable generation is null, never guessed"
        );
    }

    #[tokio::test]
    async fn a_rule_missing_its_link_arg_is_a_permanent_error() {
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1");
        let res = h
            .invoke(
                &[("steps".to_string(), Value::String("build".into()))],
                &ctx(close_marker()),
            )
            .await;
        assert!(matches!(res, Err(HandlerError::MissingArg(_))));
    }

    #[tokio::test]
    async fn a_rule_with_an_empty_steps_arg_is_a_permanent_error() {
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1");
        let res = h
            .invoke(
                &[
                    ("link".to_string(), Value::String("backlog_item".into())),
                    ("steps".to_string(), Value::String(" , ".into())),
                ],
                &ctx(close_marker()),
            )
            .await;
        assert!(matches!(res, Err(HandlerError::MissingArg(_))));
    }

    #[tokio::test]
    async fn a_close_marker_with_no_id_is_a_no_op() {
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1");
        // Unreachable base URL: a no-op is the only outcome that
        // cannot error here, which is what proves nothing was fetched.
        let res = h
            .invoke(&args(), &ctx(json!({ "closed_on": "2026-08-13" })))
            .await;
        assert!(
            res.is_ok(),
            "a malformed marker retries into nothing: {res:?}"
        );
    }

    /// The `main@<sha>` shape is the conductor's; `boss-cli`'s arrival
    /// report reads the same written format. Two readers of one fact
    /// that cannot be collapsed today, so the parse is pinned
    /// (CLAUDE.md §9a).
    #[test]
    fn the_generation_parse_matches_the_conductors_written_shape() {
        assert_eq!(
            deployed_generation("main@abc1234; playground"),
            Some("abc1234")
        );
        assert_eq!(deployed_generation("main@abc1234"), Some("abc1234"));
        assert_eq!(
            deployed_generation("main@abc1234 playground"),
            Some("abc1234")
        );
        assert_eq!(deployed_generation("deployed by hand"), None);
        assert_eq!(deployed_generation("main@"), None);
    }
}

#[cfg(test)]
mod noop_reason_tests {
    use super::noop_reason;
    use serde_json::json;

    const ALLOWED: [&str; 3] = ["investigate", "design-review", "build"];

    fn packet(steps: &[(&str, &str)]) -> serde_json::Value {
        json!({
            "steps": steps
                .iter()
                .map(|(slug, st)| json!({"spec_slug": slug, "status": st}))
                .collect::<Vec<_>>()
        })
    }

    // The case that produced nothing at all. Car 80345764 named
    // 3adc2c49, merged, and the packet stayed at `triage` — the
    // obligation cannot complete triage because choosing a disposition
    // is the routing decision the protocol exists to record.
    #[test]
    fn an_untriaged_packet_is_worth_saying_out_loud() {
        let why = noop_reason(
            &packet(&[("triage", "ready"), ("build", "pending")]),
            &ALLOWED,
        )
        .expect("reported");
        assert!(why.contains("triage"), "{why}");
        assert!(why.contains("no actionable step"), "{why}");
    }

    // The half that matters just as much. JetStream is at-least-once
    // and jobs.job.closed has three emit sites, so this is the COMMON
    // path — a warning here would be a warning per redelivery, which
    // is a warning nobody reads.
    #[test]
    fn a_redelivery_says_nothing() {
        assert_eq!(
            noop_reason(
                &packet(&[("triage", "completed"), ("build", "completed")]),
                &ALLOWED
            ),
            None
        );
    }

    // A packet mid-transition has nothing open; a later event carries
    // it. Silence is right rather than a note that ages badly.
    #[test]
    fn a_packet_with_nothing_open_says_nothing() {
        assert_eq!(
            noop_reason(
                &packet(&[("triage", "pending"), ("build", "pending")]),
                &ALLOWED
            ),
            None
        );
    }

    // Guard against the report firing on the path that WORKS: if a
    // named step is open the handler completes it and never reaches
    // this function, but the predicate must not claim otherwise.
    #[test]
    fn an_actionable_packet_is_not_reported_as_a_noop() {
        // `build` is open and named — the caller completes it, so this
        // must not claim there was nothing to do.
        assert_eq!(noop_reason(&packet(&[("build", "ready")]), &ALLOWED), None);
        // Even alongside another open step, which is the shape that
        // first tripped this: `triage` open is not evidence of a dead
        // link when `build` is open too.
        assert_eq!(
            noop_reason(
                &packet(&[("triage", "ready"), ("build", "ready")]),
                &ALLOWED
            ),
            None
        );
    }

    #[test]
    fn the_message_names_what_the_obligation_can_complete() {
        let why = noop_reason(&packet(&[("needs-info", "active")]), &ALLOWED).expect("reported");
        for slug in ALLOWED {
            assert!(why.contains(slug), "{why} is missing {slug}");
        }
        assert!(why.contains("needs-info"), "{why}");
    }
}
