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
//! ## Routing (v3, dda0713c)
//!
//! A car parked with `--park-backlog-item` names a `backlog-item`
//! whose triage nobody has done — the route asks an operator to
//! measure a claim, and a car that landed and was PROVEN (ship-a-
//! change's `merged` terminal is `ready_when = "steps.proven.done"`,
//! so this close is that moment) is the measurement. Five items were
//! closed by hand that way in one night. The optional `route` arg is
//! that routing as data: `{"kind", "step", "metadata"}` — the kind
//! whose triage vocabulary it speaks, the routing step, and what that
//! step requires at done. When none of `steps` is open and the
//! routing step is, the handler completes it, re-reads the packet,
//! and completes the branch the fork opened. Any other kind keeps the
//! v2 answer (a noop note): a filer's decision is theirs.
//!
//! ## One route per kind (v4, 1c704bb8)
//!
//! `route` is a LIST — a JSON array of the object above, one entry per
//! kind — and the single object is still accepted as a list of one. v3
//! named `backlog-item` alone and left `user-feedback` to its filer;
//! that held for feedback nobody had acted on, and inverted the moment
//! a car was PARKED AGAINST the packet: the build decision was made by
//! whoever linked the car, the landed and proven car is the evidence,
//! and David ratified exactly this close ("once the user feedback
//! results in a shipped change it can be closed without the filer
//! approving"). On 2026-09-14 his own feedback 9827c699 had its car
//! landed and proven inside seventy minutes and stayed open at triage
//! while the obligation wrote a noop on the car — closed by hand ten
//! minutes after the proof, the act this rule exists to remove. The
//! handler matches the packet's kind against the list; a kind no entry
//! names still keeps the v2 answer.
//!
//! ## Saying so on BOTH ends (ca76d8f9)
//!
//! When the obligation can act on neither the branch nor the route, the
//! note lands on the car AND on the item. It used to land only on the
//! car, so the ITEM — the side that still reads as unfinished work, and
//! the side whoever triages it next opens — carried no trace that a
//! change answering it had shipped. "A component that waited instead of
//! speaking" is the shape CLAUDE.md §Diagnosis names, and the item is
//! where that silence was paid for. The common case is fixed upstream
//! of this handler now: a park states the item's route when it files the
//! car (`boss_jobs::car::triage_on_park`), so what reaches here is the
//! residue that remains — an item a person routed somewhere this
//! obligation cannot act, or a kind whose triage is not its to make.
//!
//! ## Reading the answer (v5, 05d301be)
//!
//! A closing packet may carry the FACTS the step needs — an ops-request
//! whose verb read something back from a host — and until this version
//! the only substitutions were the car's own (`{branch}`, `{car}`,
//! `{title}`). The tag-release verb prints `tag-release: v1.2.3 at
//! <sha> (packet <id>)` as its last line, the release packet's `tag`
//! step requires exactly `tag` and `sha`, and "never a sha you did not
//! read from git" is that step's own procedure. Two optional args, in
//! `ops.judge`'s vocabulary so a rule author learns one:
//!
//! - `verb` — the closing packet's `metadata.verb` must equal it, or
//!   the close is not this rule's (every answered ops-request fires a
//!   `kind = "ops-request" AND outcome = "answered"` rule; the closed
//!   marker carries no metadata to select on).
//! - `verdict_pattern` — a regex with NAMED groups over the closing
//!   packet's `execute` step `output`; the LAST matching line is the
//!   answer, and each group substitutes `{name}` in `done_metadata`
//!   beside the car facts. No line matching means the verb did not
//!   answer (a refusal prints `REFUSED`, which no answer pattern
//!   matches): the obligation completes nothing and notes why on both
//!   ends, the way a dead link is noted. A pattern that is not a regex
//!   is rule authoring — `Permanent`, never retried.
//!
//! ## Reading the failure (v6, f47861a5)
//!
//! An ops-request closes `answered` when its verb RAN — the runner
//! records how it went as `exit_code` on the execute step, and nothing
//! in v5 read it. Measured 2026-09-19 on publish 254177e2: the
//! publish-github-pr verb printed `FAILED — … dubious ownership …`,
//! exited 1, and its request closed `answered`; v5 found no answer
//! line, noted the noop on both ends, and the publish's open-pr step
//! sat `ready` for five hours with nothing on it and no packet filed —
//! the yard drew a healthy publish. One optional arg:
//!
//! - `on_failure = "annotate-and-alert"` — when the closing packet's
//!   execute step records a non-zero exit, the step on the far end is
//!   NOT completed (it is not done) and NOT left alone: the verb's
//!   last FAILED line lands on it as `failed` (with `failed_exit`,
//!   `failed_source` = the request, and `alert`), and an URGENT
//!   backlog-item naming the verb, the request and the line is filed
//!   to the platform owner through the door every alarm handler uses.
//!   One alert per failed request (`for_request` dedups while open).
//!
//! ## The mode is the DEFAULT (v7, f47861a5)
//!
//! v6 shipped it opt-in, and opt-in covered the two rules whose author
//! had just watched the silence — and no other. The release rule
//! (`complete-release-tag-on-tag-release-answered`, landed the day
//! before) names no mode, so a `tag-release` that ran and FAILED
//! matched no answer line, wrote v5's dead-link note on both ends, and
//! left the release packet's `tag` step `ready` with nothing on it: the
//! measured shape of this packet on another verb, another protocol and
//! another step. A behaviour every rule author must remember to ask for
//! is the defect (CLAUDE.md §9a), and the default that makes a failure
//! loud is the one to make free. So `on_failure` is now an OPT-OUT: a
//! rule whose linked step is genuinely not troubled by its verb failing
//! writes `on_failure = "note"` and gets v5's answer; everything else,
//! including every rule written from here on, troubles the step it
//! would have completed. Only a closing packet with an `execute` step
//! recording a non-zero exit is a failure at all, so the rules that
//! react to a car, a gate-run or a design are untouched.
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
    /// Who the failure alert (v6) is filed to — the platform owner as
    /// the port answers it, resolved per invocation by
    /// `common::owner_for_filing`; never a literal (3c23662d).
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsCompleteLinkedStep {
    pub fn new(
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: crate::handlers::common::api_client(),
            jobs_base: jobs_base.into(),
            owner,
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local stand-in for jobs-api).
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

    /// Record that the obligation could not act — on BOTH ends of the
    /// dead link, `on` naming the Job annotated and `counterpart_key` /
    /// `counterpart` naming the other.
    ///
    /// ONE DEFINITION, TWO DIRECTIONS. The note went only on the CAR
    /// until now: the side that made the claim, and the side a reviewer
    /// opens. But the ITEM is the side that looks like unfinished work,
    /// and the side whoever eventually triages it is reading — and it
    /// said nothing at all, so a packet whose change was built, landed
    /// and proven carried no trace of any of it (backlog ca76d8f9; the
    /// `obligation_noop` key is now written from both ends, `packet` on
    /// the car and `car` on the item).
    ///
    /// The write goes through `PATCH /api/jobs/{id}/metadata` — the
    /// door built for a partial metadata write, merge semantics, and
    /// it works on a closed packet (the car always IS closed here).
    /// The first version PUT `/api/jobs/{id}` with a metadata-only
    /// body, which `Json<Job>` 422s at the extractor for its ten
    /// missing required fields — so the note never landed once, and
    /// the failure drowned in a dispatcher warn (c65110d6).
    async fn note_noop(
        &self,
        on: &str,
        counterpart_key: &str,
        counterpart: &str,
        why: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        // Idempotent under redelivery: the same pair, the same note.
        // The PATCH merge would write the same value harmlessly, but
        // each write is an audit event — one is truth, three are noise.
        let job = self.get_job(on, rule).await?;
        if job
            .get("metadata")
            .and_then(|m| m.get("obligation_noop"))
            .and_then(|n| n.get(counterpart_key))
            .and_then(|v| v.as_str())
            == Some(counterpart)
        {
            return Ok(());
        }
        let url = format!(
            "{}/api/jobs/{}/metadata",
            self.jobs_base.trim_end_matches('/'),
            on
        );
        let resp = self
            .client
            .patch(&url)
            .header("content-type", "application/json")
            .header("x-boss-user", dispatcher_actor_header(rule))
            .header("x-sim-origin", sim_origin_value())
            .json(&json!({
                "obligation_noop": { counterpart_key: counterpart, "rule": rule, "why": why }
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
///
/// Shared with `jobs_complete_step_matching`, along with `is_open`,
/// `is_unset` and `template_arg`: the two handlers complete a step the
/// same way and differ only in how they find the packet (c34583cb).
pub(crate) fn step_by_slug<'a>(
    job: &'a serde_json::Value,
    slug: &str,
) -> Option<&'a serde_json::Value> {
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
        // Parsed before any read: a bad regex is the same on every
        // delivery, and dying on it after the reads wastes them.
        let answer = AnswerSpec::from_args(args)?;

        // The `jobs.job.closed` payload carries the closing Job's id;
        // a `step.done.<kind>` marker carries the same Job under
        // `job_id` (backlog 8f83cade: a design DECIDES its linked
        // packet at its review's completion, minutes to hours before
        // it closes, and the rule that says so listens on the step).
        // A malformed marker is not something a redelivery can fix, so
        // it is a no-op rather than an error that retries forever.
        let Some(closing_id) = ctx
            .event_payload
            .get("id")
            .or_else(|| ctx.event_payload.get("job_id"))
            .and_then(|v| v.as_str())
        else {
            return Ok(());
        };

        let closing = self.get_job(closing_id, &ctx.rule_name).await?;
        let closing_meta = closing.get("metadata").cloned().unwrap_or(json!({}));

        // Not this rule's verb: another answered request on the same
        // topic, possibly carrying the same edge. Silent — it is
        // somebody else's close, not a dead link.
        if let Some(verb) = &answer.verb
            && closing_meta.get("verb").and_then(|v| v.as_str()) != Some(verb.as_str())
        {
            return Ok(());
        }

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

        // THE FAILURE (v6, f47861a5; the default since v7). A verb that
        // RAN and exited non-zero is an answered request — the outcome
        // says the verb ran, the execute step says how it went — and
        // the failure mode is read here, before the answer pattern is
        // consulted: a FAILED verb has no answer line, and v5's "no
        // line matched" note on both ends is what left the publish step
        // ready and silent for five hours.
        if let (Some(OnFailure::AnnotateAndAlert), Some(failure)) =
            (answer.on_failure, verb_failure(&closing))
        {
            return self
                .annotate_and_alert(
                    closing_id,
                    &closing_meta,
                    target_id,
                    &target,
                    &allowed,
                    &failure,
                    ctx,
                )
                .await;
        }

        // THE ANSWER (v5). A rule that asked for one gets it or gets
        // nothing: a closing packet whose recorded output carries no
        // line the pattern matches did not answer — the verb refused,
        // or was killed before its last line — and the step it would
        // have completed stays with its person. Said on both ends, like
        // a dead link; idempotent under redelivery like it too.
        let answer_groups = match answer.groups(&closing) {
            Ok(groups) => groups,
            Err(why) => {
                tracing::warn!(
                    rule = %ctx.rule_name,
                    car = %closing_id,
                    packet = %target_id,
                    "obligation completed nothing — {why}"
                );
                for (on, key, counterpart) in [
                    (closing_id, "packet", target_id),
                    (target_id, "car", closing_id),
                ] {
                    if let Err(e) = self
                        .note_noop(on, key, counterpart, &why, &ctx.rule_name)
                        .await
                    {
                        tracing::warn!(rule = %ctx.rule_name, job = %on,
                            "could not record the no-op note: {e}");
                    }
                }
                return Ok(());
            }
        };

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
        let done_metadata = template_arg(args, "done_metadata", &ctx.rule_name);
        let routes = parse_routes(args, &ctx.rule_name);

        // GUARD 2 — the open branch, the route to one, or nothing.
        // Exactly one of the named steps is open on a live packet (the
        // fork's `ready_when` guarantees it: each branch gates on a
        // different disposition value). A re-delivery finds the branch
        // already `completed` and falls out here.
        let mut shipped: Option<Shipped> = None;
        let step = match open_step(&target, &allowed).cloned() {
            Some(step) => step,
            None => {
                // THE ROUTE (v3, dda0713c). No branch is open because
                // nobody has triaged the packet — and for a kind the
                // rule row names, a landed, proven car IS the
                // measurement triage was waiting for. Complete the
                // routing step with the row's disposition + evidence,
                // re-read, and the fork's own `ready_when` has opened
                // the branch this obligation completes. Scoped to the
                // kinds the list names (v4, 1c704bb8: one entry per
                // kind): every other kind keeps the answer below — a
                // filer's routing decision stays theirs.
                let routable = routes
                    .iter()
                    .find(|r| target.get("kind").and_then(|v| v.as_str()) == Some(r.kind.as_str()))
                    .and_then(|r| {
                        step_by_slug(&target, &r.step)
                            .filter(|s| is_open(s))
                            .map(|s| (r, s.clone()))
                    });
                let Some((route, routing_step)) = routable else {
                    // Silent on a redelivery; loud when the link
                    // pointed at a packet this obligation cannot act
                    // on. See `noop_reason`.
                    if let Some(why) = noop_reason(&target, &allowed) {
                        tracing::warn!(
                            rule = %ctx.rule_name,
                            car = %closing_id,
                            packet = %target_id,
                            "obligation completed nothing — {why}"
                        );
                        // A dispatcher log line is not something a car
                        // author or a triager reads, so the note lands
                        // on BOTH ends of the link: the car — the side
                        // that made the claim, and the side a reviewer
                        // opens — and the ITEM, the side that still
                        // looks like unfinished work and the side
                        // whoever triages it next is reading. Until now
                        // the item got nothing at all, which is the
                        // "waited instead of speaking" shape CLAUDE.md
                        // names (backlog ca76d8f9). Best-effort, each
                        // independently: failing to annotate must not
                        // fail the obligation, which has already done
                        // all it can, and one end failing must not cost
                        // the other.
                        for (on, key, counterpart) in [
                            (closing_id, "packet", target_id),
                            (target_id, "car", closing_id),
                        ] {
                            if let Err(e) = self
                                .note_noop(on, key, counterpart, &why, &ctx.rule_name)
                                .await
                            {
                                tracing::warn!(rule = %ctx.rule_name, job = %on,
                                    "could not record the no-op note: {e}");
                            }
                        }
                    }
                    return Ok(());
                };
                let facts = self
                    .shipped(closing_id, &closing, &closing_meta, &answer_groups, ctx)
                    .await;
                self.complete_step(
                    target_id,
                    &routing_step,
                    Some(&route.metadata),
                    &facts,
                    evidence_key,
                    &ctx.rule_name,
                )
                .await?;
                shipped = Some(facts);
                let routed = self.get_job(target_id, &ctx.rule_name).await?;
                match open_step(&routed, &allowed).cloned() {
                    Some(step) => step,
                    // The route wrote a disposition none of `steps`
                    // answers to — rule authoring, pinned by
                    // feedback_obligation_rules.rs, and not something
                    // a redelivery fixes. The routing step's write is
                    // a true record either way.
                    None => {
                        tracing::warn!(
                            rule = %ctx.rule_name,
                            car = %closing_id,
                            packet = %target_id,
                            "routed through `{}` but no branch in [{}] opened",
                            route.step,
                            allowed.join(", ")
                        );
                        return Ok(());
                    }
                }
            }
        };

        // GUARD 3 — already stamped by this same car. Cheap, and it
        // makes the write self-describing about which delivery wrote
        // it.
        if stamped_by(&step, evidence_key, closing_id) {
            return Ok(());
        }

        let facts = match shipped {
            Some(f) => f,
            None => {
                self.shipped(closing_id, &closing, &closing_meta, &answer_groups, ctx)
                    .await
            }
        };
        self.complete_step(
            target_id,
            &step,
            done_metadata.as_ref(),
            &facts,
            evidence_key,
            &ctx.rule_name,
        )
        .await
    }
}

/// What shipped, read once from the car (and its train) and written
/// wherever the obligation completes a step: the substitution facts a
/// template names, and the evidence object itself.
struct Shipped {
    car: String,
    branch: String,
    title: String,
    evidence: serde_json::Value,
    /// The named groups of the closing packet's answer line (v5) —
    /// empty when the rule asked for none.
    answer: serde_json::Map<String, serde_json::Value>,
}

/// What a rule asked the obligation to READ off the closing packet
/// (v5, 05d301be): which verb's answers are its business, and the
/// pattern whose named groups are the facts. Both optional; the
/// pattern's spelling is `ops.judge`'s.
struct AnswerSpec {
    verb: Option<String>,
    pattern: Option<regex::Regex>,
    /// What to do when the closing packet's verb FAILED (v6) — the
    /// DEFAULT since v7, `None` only where a rule opted out.
    on_failure: Option<OnFailure>,
}

/// The failure mode (v6, f47861a5): the verb's last FAILED line is
/// written onto the still-open step and an urgent backlog-item is
/// filed for it. A second mode is a new variant here and a new word in
/// `from_args`, never a string compared elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnFailure {
    AnnotateAndAlert,
}

/// The word a rule file spells the mode as — the default, so a rule
/// says it only to be explicit.
const ANNOTATE_AND_ALERT: &str = "annotate-and-alert";

/// The word a rule spells the OPT-OUT as: keep v5's dead-link note and
/// leave the step alone. Nothing in the tree asks for it today; it
/// exists so a rule whose linked step is genuinely not troubled by its
/// verb failing can say so in one word instead of by silence.
const NOTE: &str = "note";

impl AnswerSpec {
    fn from_args(args: &[(String, Value)]) -> Result<Self, HandlerError> {
        let verb = match arg(args, "verb") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        let pattern = match arg(args, "verdict_pattern") {
            Some(Value::String(src)) if !src.is_empty() => {
                Some(regex::Regex::new(src).map_err(|e| {
                    HandlerError::Permanent(format!("verdict_pattern {src:?} is not a regex: {e}"))
                })?)
            }
            _ => None,
        };
        // THE MODE IS THE DEFAULT (v7, f47861a5). v6 made it opt-in,
        // which covered the two rules whose author had just been burned
        // and no other: the release rule (written a day earlier) still
        // answered a FAILED tag-release with a noop note on both ends,
        // leaving the release packet's `tag` step `ready` and silent —
        // the same defect the mode exists to refuse, one verb over. A
        // behaviour every rule must remember to ask for is the defect
        // (CLAUDE.md §9a), so the rules stop asking and a rule that
        // does not want it says `note`.
        let on_failure = match arg(args, "on_failure") {
            None => Some(OnFailure::AnnotateAndAlert),
            Some(Value::String(s)) if s.is_empty() || s == ANNOTATE_AND_ALERT => {
                Some(OnFailure::AnnotateAndAlert)
            }
            Some(Value::String(s)) if s == NOTE => None,
            // Rule authoring, identical on every redelivery.
            Some(other) => {
                return Err(HandlerError::Permanent(format!(
                    "on_failure {other:?} is not a mode this handler knows; the modes are \
                     {ANNOTATE_AND_ALERT:?} (the default) and {NOTE:?}"
                )));
            }
        };
        Ok(Self {
            verb,
            pattern,
            on_failure,
        })
    }

    /// PURE over the closing packet: the answer's named groups, or why
    /// there is no answer. No pattern asked for means no groups and no
    /// complaint. The line is the LAST one the pattern matches in the
    /// `execute` step's recorded `output` (`ops_judge::verdict_groups`,
    /// one definition), because the ops-runner records the two streams
    /// merged and a verb says its answer last.
    fn groups(
        &self,
        closing: &serde_json::Value,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let Some(pattern) = &self.pattern else {
            return Ok(serde_json::Map::new());
        };
        let output = step_by_slug(closing, super::ops_judge::REPORT_STEP)
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("output"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match super::ops_judge::verdict_groups(pattern, output) {
            Some((_, serde_json::Value::Object(groups))) => Ok(groups),
            _ => Err(format!(
                "no line of the closing packet's {} output matches the answer pattern {:?} — \
                 the verb refused or never reached its answer line, so nothing was read back \
                 and the step stays with its person",
                super::ops_judge::REPORT_STEP,
                pattern.as_str()
            )),
        }
    }
}

/// The routing a rule row may ask for (v3, dda0713c): when none of
/// `steps` is open because the packet has not been triaged, complete
/// `step` on a packet of `kind` with `metadata` — the step kind's
/// required vocabulary, `{branch}`/`{car}`/`{title}` substituted — and
/// let the fork open the branch `steps` then completes. A rule row
/// carries one per kind (v4, 1c704bb8).
struct Route {
    kind: String,
    step: String,
    metadata: serde_json::Map<String, serde_json::Value>,
}

/// A rule-row arg holding a JSON object as a string (`done_metadata`,
/// and the route's `metadata`). Bad rule authoring is permanent —
/// redelivery cannot fix a malformed template, and dying on it would
/// also kill the evidence write — so a malformed one is a warning and
/// `None`, never an error.
pub(crate) fn template_arg(
    args: &[(String, Value)],
    name: &str,
    rule: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let Some(Value::String(src)) = arg(args, name) else {
        return None;
    };
    match serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(src) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(
                rule = %rule,
                "{name} is not a JSON object ({e}) — completing with evidence only"
            );
            None
        }
    }
}

/// The `route` arg: a JSON array of `{kind, step, metadata}` objects,
/// or a single such object read as a list of one (the v3 shape, still
/// accepted so a tenant's own rule row keeps parsing). Bad rule
/// authoring is permanent, so a malformed value — or one malformed
/// entry — is a warning and an EMPTY list, never an error: the
/// obligation then runs as v2 did.
fn parse_routes(args: &[(String, Value)], rule: &str) -> Vec<Route> {
    let Some(Value::String(src)) = arg(args, "route") else {
        return Vec::new();
    };
    let entries = match serde_json::from_str::<serde_json::Value>(src) {
        Ok(serde_json::Value::Array(list)) => list,
        Ok(one @ serde_json::Value::Object(_)) => vec![one],
        _ => Vec::new(),
    };
    let routes: Option<Vec<Route>> = entries
        .iter()
        .map(|v| {
            Some(Route {
                kind: v.get("kind")?.as_str()?.to_string(),
                step: v.get("step")?.as_str()?.to_string(),
                metadata: v.get("metadata")?.as_object()?.clone(),
            })
        })
        .collect();
    match routes {
        Some(routes) if !routes.is_empty() => routes,
        _ => {
            tracing::warn!(
                rule = %rule,
                "route is not a JSON object (or a non-empty array of objects) with \
                 kind/step/metadata — completing without routing"
            );
            Vec::new()
        }
    }
}

pub(crate) fn is_open(step: &serde_json::Value) -> bool {
    step.get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|st| OPEN_STATUSES.contains(&st))
}

/// The one open step among `allowed`, if any.
fn open_step<'a>(job: &'a serde_json::Value, allowed: &[&str]) -> Option<&'a serde_json::Value> {
    allowed
        .iter()
        .filter_map(|slug| step_by_slug(job, slug))
        .find(|s| is_open(s))
}

/// Does this step already carry `car` under `evidence_key`?
fn stamped_by(step: &serde_json::Value, evidence_key: &str, car: &str) -> bool {
    step.get("metadata")
        .and_then(|m| m.get(evidence_key))
        .and_then(|e| e.get("car"))
        .and_then(|v| v.as_str())
        == Some(car)
}

/// Is this key unset on the step? Absent, or the `""` placeholder.
///
/// `user-feedback` and `backlog-item` default their design-review's
/// `verdict` to `""` — the key boss-expr needs PRESENT before the step
/// completes, and (the Workflow's own words) "an empty string can never
/// be an enum member, so the lint reads it as the unset placeholder".
/// `fill` read presence alone, so on a design-review this obligation
/// would have kept the placeholder and PUT `verdict: ""` — a 400 at
/// the write, and the loop breaking exactly where it was fixed
/// (backlog 5f0b2661, found by the design-close witness test). A
/// person's real verdict is never the empty string, so this cannot
/// overwrite one.
pub(crate) fn is_unset(v: Option<&serde_json::Value>) -> bool {
    match v {
        None => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        Some(_) => false,
    }
}

/// Fill `merged` from a template: unset keys only — metadata a
/// person already wrote is their record, not this obligation's to
/// overwrite — with string values substituting the car's facts.
fn fill(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    template: &serde_json::Map<String, serde_json::Value>,
    shipped: &Shipped,
) {
    for (k, v) in template {
        if !is_unset(merged.get(k)) {
            continue;
        }
        let v = match v {
            serde_json::Value::String(s) => {
                let base = s
                    .replace("{branch}", &shipped.branch)
                    .replace("{car}", &shipped.car)
                    .replace("{title}", &shipped.title);
                // The answer's groups (v5): a numeric group renders as
                // its digits, a string as itself.
                serde_json::Value::String(shipped.answer.iter().fold(base, |acc, (name, val)| {
                    let text = match val {
                        serde_json::Value::String(t) => t.clone(),
                        other => other.to_string(),
                    };
                    acc.replace(&format!("{{{name}}}"), &text)
                }))
            }
            other => other.clone(),
        };
        merged.insert(k.clone(), v);
    }
}

/// What a verb that ran and failed left behind (v6): its exit, and the
/// line that says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerbFailure {
    pub exit: String,
    pub line: String,
}

/// The dedup key a failure alert carries: the request whose verb
/// failed. One alert per failed request while it is open.
pub(crate) const FOR_REQUEST: &str = "for_request";

/// How a verb says it stopped, in the order the line is looked for:
/// the forge verbs' `fail()` prints `FAILED — …` and their `refuse()`
/// prints `REFUSED — …`. `REFUSED` earns a pass of its own rather than
/// riding the last-line fallback because `refuse()` prints a SECOND
/// line after the reason — `  Nothing was written.` — which the
/// fallback would hand the alert: true, and not a verdict (CLAUDE.md
/// §Diagnosis, a verdict must name what failed).
const FAILURE_MARKERS: [&str; 2] = ["FAILED", "REFUSED"];

/// PURE over the closing packet: `Some` when its `execute` step records
/// a non-zero `exit_code` — the ops-runner's record of a verb that RAN
/// and failed (a RUNNER refusal ran nothing, carries no exit, and
/// closes `refused`, which no answered-rule fires on). The line is the
/// last one carrying the first marker above that appears at all, else
/// the last non-empty line: the runner records both streams merged and
/// a verb says why it stopped last. c98a782f's line was the 4th of 4.
pub(crate) fn verb_failure(closing: &serde_json::Value) -> Option<VerbFailure> {
    let meta = step_by_slug(closing, super::ops_judge::REPORT_STEP)?.get("metadata")?;
    let exit = match meta.get("exit_code")? {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if exit.is_empty() || exit == "0" {
        return None;
    }
    let output = meta.get("output").and_then(|v| v.as_str()).unwrap_or("");
    let mut lines = output.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = FAILURE_MARKERS
        .iter()
        .find_map(|marker| lines.clone().rfind(|l| l.contains(marker)))
        .or_else(|| lines.next_back())
        .unwrap_or("(no output recorded)")
        .to_string();
    Some(VerbFailure { exit, line })
}

/// PURE: the urgent packet one failed verb becomes — named after the
/// verb, the request and the step it leaves open, the FAILED line
/// verbatim under `failed`, the request under `for_request` (the dedup
/// key). Subject: the troubled packet's own, so "what went wrong with
/// this publish" answers from Subject history.
#[allow(clippy::too_many_arguments)]
pub(crate) fn failure_alert_body(
    closing_id: &str,
    verb: &str,
    target_id: &str,
    target: &serde_json::Value,
    step_slug: &str,
    failure: &VerbFailure,
    owner: &str,
    ctx: &InvocationContext,
) -> serde_json::Value {
    let target_kind = target
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("packet");
    let target_title = target.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let req8 = closing_id.get(..8).unwrap_or(closing_id);
    let tgt8 = target_id.get(..8).unwrap_or(target_id);
    let subject = target
        .get("subject")
        .filter(|s| {
            s.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .unwrap_or_else(|| json!({ "subject_kind": "custom", "id": "bosspipeline" }));
    json!({
        "kind": "backlog-item",
        "title": format!(
            "{verb} FAILED (exit {}) on ops-request {req8}: {target_kind} {tgt8} stays at {step_slug}",
            failure.exit
        ),
        "subject": subject,
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": {
            "area": "platform",
            FOR_REQUEST: closing_id,
            "for_packet": target_id,
            "verb": verb,
            "exit": failure.exit,
            "failed": failure.line,
            "step": step_slug,
            "reporter": ctx.rule_name,
            "triggered_by_event_id": ctx.triggering_event_id,
            "detail": format!(
                "Filed by {} (backlog f47861a5): ops-request {req8} ran `{verb}` on the host and \
                 the verb FAILED (exit {}); the request closed `answered`, because the verb ran. \
                 The {target_kind} it was filed for ({tgt8}, \"{target_title}\") is left at its \
                 `{step_slug}` step, which stays open with the same line under `failed` — the \
                 verb did not do what the step records. The verb's last line: {}",
                ctx.rule_name, failure.exit, failure.line
            ),
        },
    })
}

impl JobsCompleteLinkedStep {
    /// THE FAILURE MODE (v6, f47861a5): `on_failure = "annotate-and-
    /// alert"`. The verb the closing request ran FAILED, so the open
    /// step on the far end is not completed — it is not done — and it
    /// is not left untouched either, which is what happened to publish
    /// 254177e2's open-pr for five hours. The alert is filed FIRST and
    /// the step annotated second, naming the alert: a redelivery after
    /// the alert landed but before the note did finds the open alert
    /// by `for_request` and reuses it, and one after both landed finds
    /// `failed_source` on the step and writes nothing. Nothing open to
    /// annotate (a person completed it, or the fork never opened it)
    /// is silent, like a redelivery against a completed branch.
    #[allow(clippy::too_many_arguments)]
    async fn annotate_and_alert(
        &self,
        closing_id: &str,
        closing_meta: &serde_json::Value,
        target_id: &str,
        target: &serde_json::Value,
        allowed: &[&str],
        failure: &VerbFailure,
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let Some(step) = open_step(target, allowed) else {
            return Ok(());
        };
        let Some(step_id) = step.get("id").and_then(|v| v.as_str()) else {
            return Ok(());
        };
        if step
            .get("metadata")
            .and_then(|m| m.get("failed_source"))
            .and_then(|v| v.as_str())
            == Some(closing_id)
        {
            return Ok(());
        }
        let slug = step
            .get("spec_slug")
            .and_then(|v| v.as_str())
            .unwrap_or(allowed.first().copied().unwrap_or("step"));
        let verb = closing_meta
            .get("verb")
            .and_then(|v| v.as_str())
            .unwrap_or("the verb");
        let base = self.jobs_base.trim_end_matches('/');

        let open =
            super::common::open_jobs_of_kind(&self.client, base, "backlog-item", rule).await?;
        let alert_id = match open
            .iter()
            .find(|j| {
                j.get("metadata")
                    .and_then(|m| m.get(FOR_REQUEST))
                    .and_then(|v| v.as_str())
                    == Some(closing_id)
            })
            .and_then(|j| j.get("id").and_then(|v| v.as_str()))
        {
            Some(existing) => existing.to_string(),
            None => {
                let owner = super::common::owner_for_filing(self.owner.as_ref(), rule).await;
                let body = failure_alert_body(
                    closing_id, verb, target_id, target, slug, failure, &owner, ctx,
                );
                super::common::post_json_minted_id(
                    &self.client,
                    &format!("{base}/api/jobs"),
                    &body,
                    rule,
                )
                .await?
            }
        };

        // The step-side merge door (PATCH .../steps/{id}/metadata):
        // top-level keys merged server-side, status untouched — the
        // step stays open, because it is.
        super::common::write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{base}/api/jobs/{target_id}/steps/{step_id}/metadata"),
            &json!({
                "failed": failure.line,
                "failed_exit": failure.exit,
                "failed_source": closing_id,
                "alert": alert_id,
            }),
            rule,
        )
        .await?;
        tracing::warn!(
            rule = %rule,
            request = %closing_id,
            packet = %target_id,
            alert = %alert_id,
            "{verb} FAILED (exit {}) — `{slug}` annotated and left open; {}",
            failure.exit,
            failure.line
        );
        Ok(())
    }

    /// The evidence. "The work you asked for shipped" is only worth
    /// saying if it names WHAT shipped — an id and a title a reader
    /// can go look at, plus the train that carried it and the
    /// generation it landed in when those are reachable. Absent facts
    /// are null, never invented.
    async fn shipped(
        &self,
        closing_id: &str,
        closing: &serde_json::Value,
        closing_meta: &serde_json::Value,
        answer: &serde_json::Map<String, serde_json::Value>,
        ctx: &InvocationContext,
    ) -> Shipped {
        let branch = closing_meta.get("branch").and_then(|v| v.as_str());
        let title = closing.get("title").and_then(|v| v.as_str()).unwrap_or("");
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
        Shipped {
            car: closing_id.to_string(),
            branch: branch.unwrap_or("(no branch recorded)").to_string(),
            title: title.to_string(),
            evidence: json!({
                "car": closing_id,
                "title": title,
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
                "branch": branch,
                "outcome": ctx.event_payload.get("outcome").and_then(|v| v.as_str()),
                "closed_on": ctx.event_payload.get("closed_on").cloned(),
                "train": train_id,
                "generation": generation,
                "by_rule": ctx.rule_name,
            }),
            answer: answer.clone(),
        }
    }

    /// Complete `step` on `target_id`: the template's vocabulary
    /// (absent keys only) plus the evidence under `evidence_key`,
    /// merged into the step's existing metadata — PATCH-on-PUT
    /// replaces top-level `metadata` wholesale, and `authority_role`
    /// living there is what keeps the step gated.
    async fn complete_step(
        &self,
        target_id: &str,
        step: &serde_json::Value,
        template: Option<&serde_json::Map<String, serde_json::Value>>,
        shipped: &Shipped,
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
            fill(&mut merged, template, shipped);
        }
        merged.insert(evidence_key.to_string(), shipped.evidence.clone());

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
            .header("x-boss-user", dispatcher_actor_header(rule))
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

    /// The owner port every case here constructs the handler with. No
    /// case in this module files an alert (the failure mode's tests
    /// live in tests/publish_pr_answer.rs, where a fixture may spell
    /// the forge's address), so the fixed owner is never read.
    pub(super) fn test_owner() -> Arc<dyn boss_core::platform_owner::PlatformOwner> {
        Arc::new(boss_core::platform_owner::Fixed("emp-owner".into()))
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

    pub(super) type Puts = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// Stand-in for jobs-api: serves the Jobs by id, records every
    /// step PUT, and records every job-metadata PATCH — the noop-note
    /// write the first mock had no route for, which is how a write
    /// that 422'd in production passed every test (c65110d6).
    ///
    /// STATEFUL since the route (v3): a step PUT lands on the stored
    /// Job, and the mock plays the one `ready_when` the route depends
    /// on — a `pending` `build` step becomes `ready` once `triage`
    /// completes with `disposition = "build"` — standing in for
    /// jobs-api's own re-evaluation on the write. The handler routes,
    /// re-reads, and must find the branch it opened.
    pub(super) async fn mock_jobs(jobs: Vec<serde_json::Value>) -> (String, Puts, Puts) {
        let patches: Puts = Arc::new(Mutex::new(Vec::new()));
        let puts: Puts = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<std::collections::HashMap<String, serde_json::Value>>> =
            Arc::new(Mutex::new(
                jobs.into_iter()
                    .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
                    .collect(),
            ));

        let get_puts = puts.clone();
        let get_jobs = by_id.clone();
        let put_jobs = by_id.clone();
        let app = Router::new()
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
                        let puts = get_puts.clone();
                        let by_id = put_jobs.clone();
                        async move {
                            puts.lock().unwrap().push((step_id.clone(), body.clone()));
                            if let Some(job) = by_id.lock().unwrap().get_mut(&id) {
                                apply_step_put(job, &step_id, &body);
                            }
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

    /// The mock's PUT: overlay status + metadata on the stored step,
    /// then re-evaluate the single predicate the route tests rely on.
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
        let routed_to_build = steps.iter().any(|s| {
            s.get("spec_slug").and_then(|v| v.as_str()) == Some("triage")
                && s.get("status").and_then(|v| v.as_str()) == Some("completed")
                && s.get("metadata")
                    .and_then(|m| m.get("disposition"))
                    .and_then(|v| v.as_str())
                    == Some("build")
        });
        if routed_to_build {
            for step in steps.iter_mut() {
                if step.get("spec_slug").and_then(|v| v.as_str()) == Some("build")
                    && step.get("status").and_then(|v| v.as_str()) == Some("pending")
                {
                    step["status"] = json!("ready");
                }
            }
        }
    }

    /// v3 rule args (a-landed-car-advances-its-backlog-item): the
    /// routing a proven car may make on an untriaged backlog item
    /// rides as data too.
    fn args_with_route() -> Vec<(String, Value)> {
        let mut a = args_with_done_metadata();
        a.push((
            "route".to_string(),
            Value::String(
                r#"{"kind": "backlog-item", "step": "triage", "metadata": {"disposition": "build", "evidence": "shipped and proven: {branch} — {title} (car {car})"}}"#.into(),
            ),
        ));
        a
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

    /// v4 rule args (1c704bb8): `route` is a LIST — one entry per kind
    /// whose triage a shipped, proven car may make. The single-object
    /// form above is still accepted; this is the shape the rule file
    /// carries now.
    fn args_with_routes() -> Vec<(String, Value)> {
        let mut a = args_with_done_metadata();
        a.push((
            "route".to_string(),
            Value::String(
                r#"[{"kind": "backlog-item", "step": "triage", "metadata": {"disposition": "build", "evidence": "shipped and proven: {branch} — {title} (car {car})"}}, {"kind": "user-feedback", "step": "triage", "metadata": {"disposition": "build", "evidence": "shipped and proven: {branch} — {title} (car {car})"}}]"#.into(),
            ),
        ));
        a
    }

    /// A live `user-feedback` packet nobody has triaged, in the shape
    /// the live Workflow gives it: `submitted` fired, `triage` open,
    /// every branch (and `needs-info`) still `pending`. The packet a
    /// car parked with `--park-backlog-item` names when what it was
    /// parked against is feedback rather than a backlog item.
    fn untriaged_feedback() -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "title": "A page for the codebase stats",
            "status": "open",
            "metadata": { "submitted_by": "emp-david" },
            "steps": [
                { "id": "s-submitted", "spec_slug": "submitted", "status": "completed",
                  "metadata": {} },
                { "id": "s-triage", "spec_slug": "triage", "status": "ready", "metadata": {} },
                { "id": "s-investigate", "spec_slug": "investigate", "status": "pending",
                  "metadata": {} },
                { "id": "s-design-review", "spec_slug": "design-review", "status": "pending",
                  "metadata": {} },
                { "id": BRANCH_STEP, "spec_slug": "build", "status": "pending", "metadata": {} },
                { "id": "s-needs-info", "spec_slug": "needs-info", "status": "pending",
                  "metadata": {} },
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
    ///
    /// ca76d8f9: and it lands on BOTH ends. The item is the side that
    /// looks like unfinished work, and it used to be told nothing.
    #[tokio::test]
    async fn an_untriaged_packet_notes_the_noop_on_both_ends() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            untriaged_packet(),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");

        assert!(
            puts.lock().unwrap().is_empty(),
            "no step completed — triage is a routing decision an obligation must not make"
        );
        let patches = patches.lock().unwrap().clone();
        assert_eq!(
            patches.len(),
            2,
            "the noop note lands once on each end: {patches:?}"
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

        let (id, body) = &patches[1];
        assert_eq!(
            id, PACKET,
            "and on the ITEM — the side that still looks like unfinished work"
        );
        assert_eq!(
            body["obligation_noop"]["car"], CAR,
            "read from the item, the note names the car that could not advance it"
        );
        assert!(
            body["obligation_noop"]["why"]
                .as_str()
                .unwrap_or_default()
                .contains("triage")
        );
    }

    /// dda0713c — the route. A car parked with `--park-backlog-item`
    /// lands, is proven, and closes `merged`; the backlog item it
    /// names is still at triage because nobody measured a claim a
    /// landed car has already settled. With a `route` on the rule row
    /// the handler completes triage with `disposition = build` +
    /// evidence naming the car, re-reads the packet, finds the `build`
    /// branch the fork opened, and completes it with the same proof —
    /// the two writes an operator made by hand on five items in one
    /// night, in the same order, through the same door.
    #[tokio::test]
    async fn a_proven_car_routes_the_backlog_item_it_names_and_completes_the_build() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            untriaged_packet(),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_route(), &ctx(close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "triage, then build: {calls:?}");

        let (step_id, body) = &calls[0];
        assert_eq!(step_id, "s-triage", "the routing step is completed FIRST");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["disposition"], "build");
        let evidence = body["metadata"]["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("fix/x") && evidence.contains(CAR),
            "the triage evidence names the branch and the car: {evidence}"
        );
        assert_eq!(
            body["metadata"]["arrived_from"]["car"], CAR,
            "the same proof object lands on triage — which car routed it"
        );

        let (step_id, body) = &calls[1];
        assert_eq!(step_id, BRANCH_STEP, "the build branch the route opened");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["arrived_from"]["car"], CAR);
        assert_eq!(body["metadata"]["arrived_from"]["branch"], "fix/x");
        assert_eq!(body["metadata"]["arrived_from"]["generation"], "abc1234");

        assert!(
            patches.lock().unwrap().is_empty(),
            "nothing to apologise for on the car — the obligation acted"
        );
    }

    /// A route is scoped to the kind it names. The v3 SINGLE-OBJECT
    /// form is still accepted (1c704bb8 made `route` a list without
    /// retiring the object), and under it a user-feedback packet at
    /// triage keeps the v2 answer: no step completed, the noop note
    /// lands on both ends. The list form below is what routes it.
    #[tokio::test]
    async fn the_route_applies_only_to_the_kind_it_names() {
        let mut feedback = untriaged_packet();
        feedback["kind"] = json!("user-feedback");
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            feedback,
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_route(), &ctx(close_marker()))
            .await
            .expect("runs");

        assert!(puts.lock().unwrap().is_empty(), "no step completed");
        assert_eq!(
            patches.lock().unwrap().len(),
            2,
            "the noop note lands on the car and on the packet"
        );
    }

    /// An item already routed elsewhere is not re-routed. Triage chose
    /// `verify`, so `measure` is the open step — not one this
    /// obligation completes, and not one it may route around.
    #[tokio::test]
    async fn an_item_routed_elsewhere_is_left_where_its_triage_put_it() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            json!({
                "id": PACKET,
                "kind": "backlog-item",
                "title": "A claim someone chose to re-measure",
                "status": "open",
                "metadata": {},
                "steps": [
                    { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                      "metadata": { "disposition": "verify", "evidence": "measured by hand" } },
                    { "id": "s-measure", "spec_slug": "measure", "status": "ready", "metadata": {} },
                    { "id": BRANCH_STEP, "spec_slug": "build", "status": "pending", "metadata": {} },
                ],
            }),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_route(), &ctx(close_marker()))
            .await
            .expect("runs");

        assert!(puts.lock().unwrap().is_empty(), "a person's route stands");
        let patches = patches.lock().unwrap().clone();
        assert_eq!(patches.len(), 2, "car and item: {patches:?}");
        for (on, body) in &patches {
            assert!(
                body["obligation_noop"]["why"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("measure"),
                "the note on {on} names the step a reader should look at"
            );
        }
        assert_eq!(
            patches[1].0, PACKET,
            "the item is told its car landed and could not advance it — it is the side \
             someone will read when they come back to triage it"
        );
    }

    /// Idempotent: the second delivery finds triage routed and build
    /// completed — by the first delivery, or by a person — and writes
    /// nothing, says nothing.
    #[tokio::test]
    async fn a_redelivery_after_the_route_writes_nothing() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            json!({
                "id": PACKET,
                "kind": "backlog-item",
                "title": "Already advanced",
                // Still `open`: the `closed` outcome fires on the
                // dispatcher's next tick, and a redelivery can land
                // in that window.
                "status": "open",
                "metadata": {},
                "steps": [
                    { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                      "metadata": { "disposition": "build", "evidence": "shipped",
                                    "arrived_from": { "car": CAR } } },
                    { "id": BRANCH_STEP, "spec_slug": "build", "status": "completed",
                      "metadata": { "arrived_from": { "car": CAR } } },
                ],
            }),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_route(), &ctx(close_marker()))
            .await
            .expect("runs");

        assert!(puts.lock().unwrap().is_empty());
        assert!(patches.lock().unwrap().is_empty());
    }

    /// Bad rule authoring is permanent, so a malformed `route` must
    /// not dead-letter the event — the obligation runs as v2 did.
    #[tokio::test]
    async fn a_malformed_route_is_a_warning_not_a_dead_letter() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            untriaged_packet(),
            train(),
        ])
        .await;
        let mut a = args_with_done_metadata();
        a.push(("route".to_string(), Value::String("not json".into())));
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&a, &ctx(close_marker())).await.expect("runs");

        assert!(puts.lock().unwrap().is_empty());
        assert_eq!(
            patches.lock().unwrap().len(),
            2,
            "the v2 noop note lands on both ends"
        );
    }

    /// 1c704bb8 — the route list. David's feedback 9827c699 had its
    /// car built, landed and PROVEN inside seventy minutes, and the
    /// packet stayed open at triage: v3's route named `backlog-item`
    /// only, so the obligation wrote `obligation_noop` on the car and
    /// the operator closed the packet by hand ten minutes after the
    /// proof — the manual act this rule exists to remove. The v3
    /// reasoning ("a filer's decision is still theirs") does not hold
    /// when a car was PARKED AGAINST the packet: whoever built and
    /// linked the car made the routing decision, and the proven car is
    /// the evidence. So the same two writes the backlog-item route
    /// makes — triage `build` with the car as evidence, then the
    /// `build` branch the fork opened — land on a user-feedback packet
    /// when the rule row lists its kind.
    #[tokio::test]
    async fn a_proven_car_routes_the_user_feedback_it_was_parked_against() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "feat/codebase-stats" })),
            untriaged_feedback(),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_routes(), &ctx(close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "triage, then build: {calls:?}");

        let (step_id, body) = &calls[0];
        assert_eq!(step_id, "s-triage", "the routing step is completed FIRST");
        assert_eq!(body["status"], "completed");
        assert_eq!(
            body["metadata"]["disposition"], "build",
            "a disposition the live user-feedback triage admits"
        );
        let evidence = body["metadata"]["evidence"].as_str().unwrap_or_default();
        assert!(
            evidence.contains("feat/codebase-stats") && evidence.contains(CAR),
            "the triage evidence names the branch and the car: {evidence}"
        );
        assert_eq!(body["metadata"]["arrived_from"]["car"], CAR);

        let (step_id, body) = &calls[1];
        assert_eq!(step_id, BRANCH_STEP, "the build branch the route opened");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["arrived_from"]["car"], CAR);

        assert!(
            patches.lock().unwrap().is_empty(),
            "nothing to apologise for — the obligation acted"
        );
    }

    /// The list did not narrow what v3 routed: a backlog item at
    /// triage takes the same two writes under the list form.
    #[tokio::test]
    async fn the_route_list_still_routes_the_backlog_item() {
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            untriaged_packet(),
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_routes(), &ctx(close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "triage, then build: {calls:?}");
        assert_eq!(calls[0].0, "s-triage");
        assert_eq!(calls[0].1["metadata"]["disposition"], "build");
        assert_eq!(calls[1].0, BRANCH_STEP);
        assert!(patches.lock().unwrap().is_empty());
    }

    /// The list is still a scope, not a licence: a kind no entry names
    /// keeps the v2 answer — nothing completed, the note on both ends.
    #[tokio::test]
    async fn the_route_list_leaves_a_kind_it_does_not_name_alone() {
        let mut other = untriaged_packet();
        other["kind"] = json!("ops-request");
        let (base, puts, patches) = mock_jobs(vec![
            car(json!({ "backlog_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            other,
            train(),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_routes(), &ctx(close_marker()))
            .await
            .expect("runs");

        assert!(puts.lock().unwrap().is_empty(), "no step completed");
        assert_eq!(
            patches.lock().unwrap().len(),
            2,
            "the noop note lands on the car and on the packet"
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "nothing to complete without a declared edge"
        );
    }

    /// A PARTIAL EDGE IS PROVENANCE, NOT AN OBLIGATION (e1325456).
    ///
    /// `boss gate --park-partial-item <id>` records `metadata.partial_item`
    /// on the car so a reader can see which item the change belongs to,
    /// for an item that is several separable pieces and must NOT close
    /// when one of them lands (cf0f5e2d was three pieces; piece (3) was
    /// waiting on an operator's explicit yes). The inertness is
    /// structural: this handler reads the ONE key its rule names, so a
    /// car carrying only the partial key completes nothing — and this
    /// test is what keeps that true if the lookup ever widens.
    #[tokio::test]
    async fn a_partial_item_is_provenance_and_closes_nothing() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "partial_item": PACKET, "train": TRAIN, "branch": "fix/x" })),
            packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args_with_route(), &ctx(close_marker()))
            .await
            .expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "a partial edge authorises no write on the item: {:?}",
            puts.lock().unwrap()
        );
    }

    /// An item-less car's recorded reason is likewise inert — it is prose
    /// for a reader, not a reference, and nothing follows it.
    #[tokio::test]
    async fn a_recorded_no_item_reason_closes_nothing() {
        let (base, puts, _) = mock_jobs(vec![
            car(json!({ "no_item_reason": "David asked for this in conversation" })),
            packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&args(), &ctx(close_marker())).await.expect("runs");
        assert!(puts.lock().unwrap().is_empty(), "nothing to complete");
    }

    /// A packet that already reached a terminal is untouched. Its
    /// filer got their answer from whatever closed it.
    #[tokio::test]
    async fn an_already_terminal_packet_is_untouched() {
        let mut closed = packet("ready");
        closed["status"] = json!("closed");
        let (base, puts, _) = mock_jobs(vec![car(json!({ "backlog_item": PACKET })), closed]).await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
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
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1", test_owner());
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
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1", test_owner());
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
        let h = JobsCompleteLinkedStep::new("http://127.0.0.1:1", test_owner());
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

    const DESIGN: &str = "c6bd173e-3dc9-426f-8fff-866a3b2a6117";
    const REVIEW_STEP: &str = "55555555-5555-5555-5555-555555555555";

    /// A published design-doc that names the feedback it answers — the
    /// `answers` edge `boss design --answers` writes (backlog 5f0b2661).
    fn design(metadata: serde_json::Value) -> serde_json::Value {
        json!({
            "id": DESIGN,
            "kind": "design-doc",
            "title": "A car lands where its change goes live",
            "status": "closed",
            "subject": { "subject_kind": "custom", "id": "boss-platform" },
            "metadata": metadata,
            "steps": [],
        })
    }

    /// A user-feedback packet triage routed to `design`: its
    /// design-review is open, carrying the question the verb wrote and
    /// the empty verdict the Workflow defaults — the live shape of
    /// 61366e5a on 2026-09-15.
    fn feedback_routed_to_design(review_status: &str) -> serde_json::Value {
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "title": "Feedback on /it",
            "status": "open",
            "metadata": { "submitted_by": "emp-david" },
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                  "metadata": { "disposition": "design" } },
                { "id": REVIEW_STEP, "spec_slug": "design-review", "status": review_status,
                  "metadata": { "authority_role": "platform-admin", "verdict": "",
                                "question": "Decide design 'A car lands where its change goes live' (c6bd173e)" } },
                { "id": BRANCH_STEP, "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// The args `complete-feedback-design-review-on-design-doc-published`
    /// carries: the `answers` edge, the one branch a design decides, the
    /// verdict + answer the answer-question kind requires at done, and
    /// an evidence key that says what happened (a design was decided,
    /// nothing arrived).
    fn design_rule_args() -> Vec<(String, Value)> {
        vec![
            ("link".to_string(), Value::String("answers".into())),
            ("steps".to_string(), Value::String("design-review".into())),
            (
                "done_metadata".to_string(),
                Value::String(
                    r#"{"verdict": "approved", "answer": "design decided: {title} ({car}) — every question resolved and the doc published"}"#.into(),
                ),
            ),
            ("evidence_key".to_string(), Value::String("decided_by".into())),
        ]
    }

    fn design_close_marker() -> serde_json::Value {
        json!({
            "id": DESIGN,
            "kind": "design-doc",
            "outcome": "published",
            "closed_on": "2026-09-15",
            "title": "A car lands where its change goes live",
            "parent_step_id": null,
        })
    }

    /// Backlog 5f0b2661 — deciding the design decides the feedback. A
    /// feedback routed to design gave one person two decisions, the
    /// second an `answer-question` with no question (David's bug
    /// 4f6019d7). With the design carrying an `answers` edge, its
    /// `published` close completes the feedback's open design-review
    /// through THIS handler — the same shape the merge obligation
    /// uses, with a different link, one step, and the design's facts
    /// in the answer. No new handler code: the rule row is the whole
    /// change, and this test is its witness.
    #[tokio::test]
    async fn a_published_design_completes_the_design_review_of_the_feedback_it_answers() {
        let (base, puts, patches) = mock_jobs(vec![
            design(json!({ "answers": PACKET, "title": "A car lands where its change goes live" })),
            feedback_routed_to_design("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        let mut ctx = ctx(design_close_marker());
        ctx.rule_name = "complete-feedback-design-review-on-design-doc-published".into();
        h.invoke(&design_rule_args(), &ctx).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "exactly the design-review completes: {calls:?}"
        );
        let (step_id, body) = &calls[0];
        assert_eq!(step_id, REVIEW_STEP);
        assert_eq!(body["status"], "completed");
        assert_eq!(
            body["metadata"]["verdict"], "approved",
            "a published design has every question resolved — the Workflow's `covers` \
             contract — so the feedback's verdict is approved and its build opens"
        );
        let answer = body["metadata"]["answer"].as_str().unwrap_or_default();
        assert!(
            answer.contains("A car lands where its change goes live") && answer.contains(DESIGN),
            "the answer names the design by title and id: {answer}"
        );
        assert_eq!(
            body["metadata"]["decided_by"]["car"], DESIGN,
            "the evidence names the design under the rule's own key"
        );
        assert_eq!(body["metadata"]["decided_by"]["outcome"], "published");
        assert!(
            body["metadata"]["decided_by"]["branch"].is_null(),
            "a design has no branch; the evidence says null rather than inventing one"
        );
        // The step's own keys survive the wholesale metadata replace.
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert!(
            body["metadata"]["question"]
                .as_str()
                .is_some_and(|q| q.contains("c6bd173e")),
            "the question the verb wrote is still on the completed step"
        );
        assert!(
            patches.lock().unwrap().is_empty(),
            "nothing to apologise for"
        );
    }

    /// A user-feedback packet in the shape f90ca046 gives the design
    /// route (2026-09-18): triage routed to `design`, the executor's
    /// `draft-design` task done carrying the id of the design it
    /// filed, and `design-review` — `ready_when =
    /// steps.draft-design.done` — open with the verb's question.
    fn feedback_drafted_for_design(draft_status: &str, review_status: &str) -> serde_json::Value {
        let draft_metadata = if draft_status == "completed" {
            json!({ "authority_role": "platform-admin", "design_id": DESIGN })
        } else {
            json!({ "authority_role": "platform-admin" })
        };
        json!({
            "id": PACKET,
            "kind": "user-feedback",
            "title": "Feedback on /it/estate",
            "status": "open",
            "metadata": { "submitted_by": "emp-david" },
            "steps": [
                { "id": "s-triage", "spec_slug": "triage", "status": "completed",
                  "metadata": { "disposition": "design" } },
                { "id": "s-draft", "spec_slug": "draft-design", "status": draft_status,
                  "metadata": draft_metadata },
                { "id": REVIEW_STEP, "spec_slug": "design-review", "status": review_status,
                  "metadata": { "authority_role": "platform-admin", "verdict": "",
                                "question": "Decide design 'A car lands where its change goes live' (c6bd173e)" } },
                { "id": BRANCH_STEP, "spec_slug": "build", "status": "pending", "metadata": {} },
            ],
        })
    }

    /// Backlog f90ca046: the design route now opens the executor's
    /// draft first and the founder's review waits on it. The rule is
    /// unchanged — it names `design-review` and follows `answers` —
    /// so a published design must still close the review it was
    /// filed for, now that the review opens one step later. Measured
    /// 2026-09-18 on 54f0ab33: the design (5fc71f03) was filed by hand
    /// against a review that had been ready, empty, for the founder
    /// since triage.
    #[tokio::test]
    async fn a_published_design_still_closes_the_review_that_waited_on_its_draft() {
        let (base, puts, patches) = mock_jobs(vec![
            design(json!({ "answers": PACKET, "title": "A car lands where its change goes live" })),
            feedback_drafted_for_design("completed", "ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        let mut ctx = ctx(design_close_marker());
        ctx.rule_name = "complete-feedback-design-review-on-design-doc-published".into();
        h.invoke(&design_rule_args(), &ctx).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "exactly the design-review completes — never the draft: {calls:?}"
        );
        let (step_id, body) = &calls[0];
        assert_eq!(step_id, REVIEW_STEP);
        assert_eq!(body["status"], "completed");
        assert_eq!(body["metadata"]["verdict"], "approved");
        assert_eq!(body["metadata"]["decided_by"]["car"], DESIGN);
        assert!(patches.lock().unwrap().is_empty(), "nothing to note");
    }

    /// The other half of the same shape: a design that publishes while
    /// the draft is still open — the executor filed it without
    /// `--answers`, say, and completed nothing — finds the review
    /// PENDING and writes nothing. A pending review is one the route
    /// has not opened, and completing it would fabricate the draft's
    /// record; the handler notes the noop on both ends instead, and
    /// the draft stays with the executor.
    #[tokio::test]
    async fn a_design_published_before_its_draft_is_done_completes_nothing() {
        let (base, puts, patches) = mock_jobs(vec![
            design(json!({ "answers": PACKET })),
            feedback_drafted_for_design("ready", "pending"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        let mut ctx = ctx(design_close_marker());
        ctx.rule_name = "complete-feedback-design-review-on-design-doc-published".into();
        h.invoke(&design_rule_args(), &ctx).await.expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "a pending review is never completed, and the draft is not the rule's to touch"
        );
        assert!(
            !patches.lock().unwrap().is_empty(),
            "the noop is noted rather than silent — the draft is still open"
        );
    }

    /// The person decided the feedback step first (the order the bug
    /// report describes). The design's close then finds it completed
    /// and writes nothing — their verdict stands, and no noop note is
    /// filed, because the work was done.
    #[tokio::test]
    async fn a_design_review_a_person_already_decided_is_left_alone() {
        let (base, puts, patches) = mock_jobs(vec![
            design(json!({ "answers": PACKET })),
            feedback_routed_to_design("completed"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&design_rule_args(), &ctx(design_close_marker()))
            .await
            .expect("runs");
        assert!(
            puts.lock().unwrap().is_empty(),
            "a person's decision stands"
        );
        assert!(
            patches.lock().unwrap().is_empty(),
            "the work was done — nothing to say on either end"
        );
    }

    /// A `step.done.review-design` marker in the shape `http/steps.rs`
    /// emits: the design is still OPEN (its fold is the agent's next
    /// step), and the packet's own id rides as `job_id`, not `id`.
    fn design_review_done_marker() -> serde_json::Value {
        json!({
            "job_id": DESIGN,
            "step_id": "s-review",
            "kind": "review-design",
            "workflow_kind": "design-doc",
            "spec_slug": "review",
            "subject_kind": "custom",
            "subject_id": "boss-platform",
            "completed_on": "2026-09-18",
            "metadata": { "resolutions": [{ "anchor": "q1", "decision": "yes" }] },
            "notify_on_done": false,
        })
    }

    /// Backlog 8f83cade — the decision is the review's completion, not
    /// the fold's. Measured 2026-09-18 23:10Z: David answered both open
    /// designs (11e60367, 55417146) and saw two 'Decide the design'
    /// steps (b4afd7b9, 92921c2f) still in his queue with nothing to
    /// do, because the completion fired on `published`, and between
    /// the review and `published` sits `fold` — the agent's write-up,
    /// minutes to hours later. Fired from `step.done.review-design`
    /// instead: the design is still open, the marker carries `job_id`
    /// rather than `id`, and the linked Decide step completes with the
    /// verdict all the same. The fold stays the design packet's own
    /// obligation.
    #[tokio::test]
    async fn a_decided_review_completes_the_linked_decide_step_while_the_design_is_still_open() {
        let mut open_design =
            design(json!({ "answers": PACKET, "title": "A car lands where its change goes live" }));
        open_design["status"] = json!("open");
        let (base, puts, patches) =
            mock_jobs(vec![open_design, feedback_routed_to_design("ready")]).await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        let mut ctx = ctx(design_review_done_marker());
        ctx.rule_name = "complete-feedback-design-review-on-design-review-decided".into();
        ctx.triggering_topic = "step.done.review-design".into();
        h.invoke(&design_rule_args(), &ctx).await.expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            1,
            "the design-review completes at the review, not at the fold: {calls:?}"
        );
        let (step_id, body) = &calls[0];
        assert_eq!(step_id, REVIEW_STEP);
        assert_eq!(body["status"], "completed");
        assert_eq!(
            body["metadata"]["verdict"], "approved",
            "a completed review has every question resolved — `resolutions` covers `questions` \
             at done — so the verdict is approved without waiting for the fold"
        );
        assert_eq!(
            body["metadata"]["decided_by"]["car"], DESIGN,
            "the evidence names the design read off the marker's job_id"
        );
        assert!(
            body["metadata"]["decided_by"]["outcome"].is_null(),
            "the design has not closed; the evidence says null rather than inventing an outcome"
        );
        assert!(
            patches.lock().unwrap().is_empty(),
            "nothing to apologise for"
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

/// THE ANSWER (backlog 05d301be): a rule may ask the obligation to read
/// the closing packet's recorded output and write what it says.
#[cfg(test)]
mod answer_tests {
    use super::tests::test_owner;
    use super::*;
    use boss_dispatcher::rules::expr::NoHelpers;
    use boss_dispatcher::rules::registry::{Registry, match_event};

    const REQUEST: &str = "55555555-5555-4555-8555-555555555555";
    const RELEASE: &str = "66666666-6666-4666-8666-666666666666";
    const TAG_STEP: &str = "77777777-7777-4777-8777-777777777777";
    const MERGE_SHA: &str = "e4d5d9816d34a1b2c3d4e5f60718293a4b5c6d7e";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            rule_name: "complete-release-tag-on-tag-release-answered".into(),
            triggering_event_id: "evt-close-9".into(),
            triggering_topic: "jobs.job.closed".into(),
            event_payload: payload,
        }
    }

    /// The rule complete-release-tag-on-tag-release-answered, read from
    /// its file and matched against an answered ops-request's close, so
    /// the args below are exactly what the dispatcher hands this handler
    /// — not a copy of the file typed here.
    fn tag_release_rule_args() -> Vec<(String, Value)> {
        let path = boss_testing::dispatcher_rules_dir()
            .join("complete-release-tag-on-tag-release-answered.toml");
        let toml = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let reg = Registry::from_toml(&toml).expect("the rule file parses");
        let matched =
            match_event(&reg, "jobs.job.closed", &request_close_marker(), &NoHelpers).matched;
        assert_eq!(
            matched.len(),
            1,
            "an answered ops-request fires the rule once"
        );
        let inv = &matched[0].invocations[0];
        assert_eq!(inv.handler, "jobs.complete_linked_step");
        inv.args.clone()
    }

    fn request_close_marker() -> serde_json::Value {
        json!({
            "id": REQUEST,
            "kind": "ops-request",
            "outcome": "answered",
            "closed_on": "2026-09-18",
            "title": "Run tag-release on forge",
            "subject_id": "forge",
            "parent_step_id": null,
        })
    }

    /// An answered tag-release ops-request: the verb's output rides its
    /// `execute` step, the release packet its `release` edge.
    fn tag_release_request(verb: &str, output: &str) -> serde_json::Value {
        json!({
            "id": REQUEST,
            "kind": "ops-request",
            "title": "Run tag-release on forge",
            "status": "closed",
            "subject": { "subject_kind": "custom", "id": "forge" },
            "metadata": { "host": "forge", "verb": verb,
                          "args": ["v1.2.3", MERGE_SHA, RELEASE],
                          "release": RELEASE, "outcome": "answered" },
            "steps": [
                { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
                { "id": "r-execute", "spec_slug": "execute", "status": "completed",
                  "metadata": { "disposition": "answered", "exit_code": "0",
                                "output": output, "runner_host": "forge" } },
            ],
        })
    }

    /// The release packet at its `tag` step (cut-a-release: tag and sha
    /// required at done; `authority_role` on the step keeps it gated).
    fn release_packet(tag_status: &str) -> serde_json::Value {
        json!({
            "id": RELEASE,
            "kind": "cut-a-release",
            "title": "Release v1.2.3 decided — boss",
            "status": "open",
            "metadata": { "version": "1.2.3" },
            "steps": [
                { "id": "s-approve", "spec_slug": "approve", "status": "completed",
                  "metadata": { "decision": "approved" } },
                { "id": TAG_STEP, "spec_slug": "tag", "status": tag_status,
                  "metadata": { "authority_role": "platform-admin" } },
                { "id": "s-mirror", "spec_slug": "mirror", "status": "pending", "metadata": {} },
            ],
        })
    }

    fn answered_output() -> String {
        let short = &MERGE_SHA[..12];
        format!(
            "tag-release: forge: no tag v1.2.3 on remote forgejo\n\
             tag-release: record: pr-train 0000abcd merged as {short} (57 closed trains read)\n\
             tag-release: converged main: {short} carries {short}\n\
             tag-release: read back: refs/tags/v1.2.3 on remote forgejo peels to {MERGE_SHA}\n\
             tag-release: v1.2.3 at {MERGE_SHA} (packet {RELEASE})\n"
        )
    }

    #[tokio::test]
    async fn an_answered_tag_release_completes_the_release_tag_step_from_its_read_back() {
        let (base, puts, patches) = super::tests::mock_jobs(vec![
            tag_release_request("tag-release", &answered_output()),
            release_packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&tag_release_rule_args(), &ctx(request_close_marker()))
            .await
            .expect("runs");

        let calls = puts.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "exactly the tag step completed: {calls:?}");
        let (step_id, body) = &calls[0];
        assert_eq!(step_id, TAG_STEP);
        assert_eq!(body["status"], "completed");
        // The step's two required fields, COPIED from the read-back line.
        assert_eq!(body["metadata"]["tag"], "v1.2.3");
        assert_eq!(body["metadata"]["sha"], MERGE_SHA);
        // The evidence names the request, under the rule's own key.
        assert_eq!(body["metadata"]["tagged_by"]["car"], REQUEST);
        assert_eq!(body["metadata"]["tagged_by"]["outcome"], "answered");
        assert_eq!(body["metadata"]["authority_role"], "platform-admin");
        assert!(patches.lock().unwrap().is_empty(), "nothing to note");
    }

    /// A verb that SUCCEEDED and printed no line the pattern matches —
    /// its output shape moved on, and nothing was read back. The step
    /// stays the founder's and both packets say why (v5's answer,
    /// which a clean exit keeps: the failure mode reads the exit, and
    /// this one is 0).
    ///
    /// This case used to be spelled with a `REFUSED` output and exit 0,
    /// which the verb cannot produce — `refuse()` exits 1 like `fail()`
    /// does, so that request's step records exit 1 and v7 troubles the
    /// release. The refusal is pinned there, in
    /// tests/publish_pr_answer.rs, where a mock can take the alert.
    #[tokio::test]
    async fn an_answer_with_no_read_back_line_completes_nothing_and_says_so_on_both_ends() {
        let no_line = "tag-release: forge: no tag v1.2.3 on remote forgejo\n\
                       tag-release: converged checkout: e4d5d98 resolves to e4d5d9816d34a1b2c3d4e5f60718293a4b5c6d7e\n\
                       tag-release: done\n";
        let (base, puts, patches) = super::tests::mock_jobs(vec![
            tag_release_request("tag-release", no_line),
            release_packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&tag_release_rule_args(), &ctx(request_close_marker()))
            .await
            .expect("runs");

        assert!(puts.lock().unwrap().is_empty(), "no step completed");
        let notes = patches.lock().unwrap().clone();
        let mut on: Vec<&str> = notes.iter().map(|(id, _)| id.as_str()).collect();
        on.sort_unstable();
        assert_eq!(on, [REQUEST, RELEASE], "the note lands on both ends");
        for (_, body) in &notes {
            let why = body["obligation_noop"]["why"].as_str().unwrap_or("");
            assert!(
                why.contains("no line"),
                "the note says what was missing: {why}"
            );
            assert!(why.contains("tag-release"), "and names the pattern: {why}");
        }
    }

    /// Another verb's answered request carrying the same `release` edge
    /// (the GitHub-release verb, one day) is not this rule's.
    #[tokio::test]
    async fn another_verbs_answer_is_not_this_rules() {
        let (base, puts, patches) = super::tests::mock_jobs(vec![
            tag_release_request("github-release", &answered_output()),
            release_packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&tag_release_rule_args(), &ctx(request_close_marker()))
            .await
            .expect("runs");
        assert!(puts.lock().unwrap().is_empty(), "no step completed");
        assert!(patches.lock().unwrap().is_empty(), "nothing noted either");
    }

    /// Redelivery: the tag step already completed by the first delivery
    /// is left alone, silently.
    #[tokio::test]
    async fn a_redelivered_answer_writes_nothing() {
        let (base, puts, patches) = super::tests::mock_jobs(vec![
            tag_release_request("tag-release", &answered_output()),
            release_packet("completed"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        h.invoke(&tag_release_rule_args(), &ctx(request_close_marker()))
            .await
            .expect("runs");
        assert!(puts.lock().unwrap().is_empty());
        assert!(patches.lock().unwrap().is_empty());
    }

    /// A pattern that is not a regex is rule authoring, the same on
    /// every redelivery: Permanent, never a retry.
    #[tokio::test]
    async fn a_malformed_verdict_pattern_is_a_permanent_error() {
        let (base, _, _) = super::tests::mock_jobs(vec![
            tag_release_request("tag-release", &answered_output()),
            release_packet("ready"),
        ])
        .await;
        let h = JobsCompleteLinkedStep::with_client(reqwest::Client::new(), base, test_owner());
        let mut a = tag_release_rule_args();
        for (k, v) in a.iter_mut() {
            if k == "verdict_pattern" {
                *v = Value::String("(?P<tag>[".into());
            }
        }
        let err = h
            .invoke(&a, &ctx(request_close_marker()))
            .await
            .expect_err("a bad regex cannot be retried into a good one");
        assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
    }

    #[test]
    fn answer_substitutions_render_every_named_group_as_text() {
        let groups = json!({ "tag": "v1.2.3", "n": 7 });
        let shipped = Shipped {
            car: REQUEST.into(),
            branch: "(no branch recorded)".into(),
            title: "t".into(),
            evidence: json!({}),
            answer: groups.as_object().cloned().unwrap_or_default(),
        };
        let mut merged = serde_json::Map::new();
        let template: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(r#"{"tag": "{tag}", "count": "{n} cars", "who": "{car}"}"#)
                .unwrap();
        fill(&mut merged, &template, &shipped);
        assert_eq!(merged["tag"], "v1.2.3");
        assert_eq!(
            merged["count"], "7 cars",
            "a numeric group renders as its digits"
        );
        assert_eq!(merged["who"], REQUEST, "the car facts still substitute");
    }
}
